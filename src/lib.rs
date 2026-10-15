//! lamlvm — Read-only LVM2 Logical Volume reader for no_std environments.
//!
//! Vendored + modernized fork of [`main--/rust-lvm2`](https://github.com/main--/rust-lvm2)
//! v0.0.3 (MIT). See `PROVENANCE.md` for the origin story and the list of
//! changes from upstream. Notable change: `acid_io` (unmaintained since
//! 2022) replaced with `embedded-io` (actively maintained by the
//! embedded-rust working group).
//!
//! Format reference:
//! <https://github.com/libyal/libvslvm/blob/main/documentation/Logical%20Volume%20Manager%20(LVM)%20format.asciidoc>
//!
//! Vocabulary: in this crate we use the term "sheet" to describe a block of
//! exactly 512 bytes (to avoid confusion around the word "sector").
//!
//! # Coverage
//!
//! Linear logical volumes, and RAID1 logical volumes through a leg on the PV
//! opened, one physical volume at a time. Striped, thin pool, snapshot, and
//! cache LVs return [`OpenLvError::Unsupported`] or analogous errors. This
//! covers the canonical layout used by Proxmox VE and most default
//! single-disk LVM installs. See `PROVENANCE.md` for the full coverage matrix
//! and the rationale for the narrow scope.
//!
//! The PV label may sit in any of the first four sectors, and its checksum is
//! checked. Each metadata area's header and committed text are checked
//! against their checksums, an area marked ignored is skipped, and the next
//! area on the PV is tried when one is ignored or damaged.

#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(rustdoc::broken_intra_doc_links)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use embedded_io::{ErrorKind, Read, Seek, SeekFrom};
use serde::Deserialize;
use snafu::{OptionExt, Snafu, ensure};

use crate::header::{
    LocationDescriptor, MetadataAreaHeader, PhysicalVolumeHeader, PhysicalVolumeLabelHeader,
};
use crate::metadata::{MetadataRoot, deserialize::MetadataElements};

mod force_de_typed_map;
mod header;
mod lv;
pub mod metadata;
mod owned;

pub use lv::*;
pub use owned::OwnedLvReader;

/// Internal parser entry points re-exported for the fuzz harness only.
/// **Not stable API.** Behind the `__fuzzing` feature so non-fuzz
/// consumers can't accidentally reach in.
#[cfg(feature = "__fuzzing")]
pub mod __fuzzing {
    pub use crate::force_de_typed_map::ForceDeTypedMap;
    pub use crate::header::{
        LocationDescriptor, MetadataAreaHeader, PhysicalVolumeHeader, PhysicalVolumeLabelHeader,
    };
    pub use crate::metadata::MetadataRoot;
    pub use crate::metadata::deserialize::MetadataElements;
}

/// Top-level error type for the lamlvm crate.
///
/// I/O errors are reduced to `embedded_io::ErrorKind` rather than carrying
/// the generic `T::Error` from each call site — keeps the enum concrete and
/// consumer code simple. Specific I/O failures are still recognizable via
/// the `ErrorKind` value; the original I/O error is converted via
/// `embedded_io::Error::kind()`.
#[derive(Debug, Snafu)]
pub enum Error {
    /// I/O failure from the underlying reader.
    #[snafu(display("I/O error: {kind:?}"))]
    Io { kind: ErrorKind },

    /// Underlying reader ended before a `read_exact` could complete.
    /// `embedded_io::ReadExactError::UnexpectedEof` is mapped here since
    /// it has no equivalent in `embedded_io::ErrorKind`.
    #[snafu(display("unexpected end of PV input during read_exact"))]
    UnexpectedEof,

    /// PV label header at sheet 1 did not have the `LABELONE` magic.
    #[snafu(display("PV label header has wrong magic"))]
    WrongMagic,

    /// nom parser rejected the input bytes (header or metadata-area-header).
    #[snafu(display("parse error: {reason}"))]
    Parse { reason: String },

    /// VG metadata text declared more than one volume group. lamlvm
    /// currently supports the canonical single-VG-per-PV layout only.
    #[snafu(display("metadata declares multiple VGs (single-VG-per-PV only)"))]
    MultipleVGs,

    /// VG metadata text did not list the PV we opened — broken or
    /// mismatched metadata.
    #[snafu(display("PV metadata does not reference this PV's own UUID"))]
    PVDoesntContainItself,

    /// serde rejected the parsed metadata token stream.
    #[snafu(display("metadata deserialize error: {reason}"))]
    Serde { reason: String },

    /// PV header has no metadata area descriptor (impossible on a valid PV).
    #[snafu(display("PV header is missing a metadata area descriptor"))]
    MissingMetadata,

    /// VG metadata bytes were not valid UTF-8.
    #[snafu(display("metadata text was not valid UTF-8"))]
    MetadataNotUtf8,
}

/// The metadata area header's own size; the ring buffer follows it.
const MDA_HEADER_SIZE: u64 = 512;

/// A PV label sits in one of the first four 512-byte sectors.
const LABEL_SCAN_SECTORS: usize = 4;
/// The label checksum covers the label sector from this byte on (after the
/// magic, the sector number and the checksum itself).
const LABEL_CRC_FROM: usize = 20;
/// `RAW_LOCN_IGNORED`: The metadata area is kept but not used (`pvchange
/// --metadataignore y`), so its text may be stale.
const RAW_LOCN_IGNORED: u32 = 0x1;
/// LVM2's checksum: CRC-32 (reflected polynomial 0xedb88320) seeded with this
/// value and without a final inversion.
const LVM_CRC_SEED: u32 = 0xf597_a6cf;

/// LVM2's checksum of `data`, continuing from `crc`.
fn lvm_crc(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// The PV label header from the first of the first four sectors that holds a
/// valid one, with that sector's bytes.
fn find_label<R: Read + Seek>(
    reader: &mut R,
) -> Result<(PhysicalVolumeLabelHeader, [u8; 512]), Error> {
    let mut bad_checksum = false;
    for sector in 0..LABEL_SCAN_SECTORS {
        let mut buf = [0u8; 512];
        reader
            .seek(SeekFrom::Start((sector * 512) as u64))
            .map_err(io_err)?;
        reader.read_exact(&mut buf).map_err(read_exact_err)?;
        let Ok((_, label)) = PhysicalVolumeLabelHeader::parse(&buf) else {
            continue;
        };
        // The label records its own sector, and a copy found elsewhere (an
        // image of another PV's start) isn't this PV's label.
        if label.sector_number != sector as u64 {
            continue;
        }
        if lvm_crc(LVM_CRC_SEED, &buf[LABEL_CRC_FROM..]) != label.checksum {
            bad_checksum = true;
            continue;
        }
        return Ok((label, buf));
    }
    if bad_checksum {
        return Err(Error::Parse {
            reason: "PV label checksum doesn't match".to_string(),
        });
    }
    Err(Error::WrongMagic)
}

/// The committed metadata text in the metadata area that starts at
/// `mda_start` and is `mda_size` bytes long, after checking the area header's
/// checksum and position and the text's checksum. The second location slot
/// is a precommit LVM2 hasn't committed yet, so only the first is read.
fn read_metadata_area<R: Read + Seek>(
    reader: &mut R,
    mda_start: u64,
    mda_size: u64,
) -> Result<Vec<u8>, Error> {
    let bad = |reason: &str| Error::Parse {
        reason: reason.to_string(),
    };
    let mut buf = [0u8; 512];
    reader.seek(SeekFrom::Start(mda_start)).map_err(io_err)?;
    reader.read_exact(&mut buf).map_err(read_exact_err)?;
    let (_, mah) = MetadataAreaHeader::parse(&buf).map_err(|e| Error::Parse {
        reason: e.to_string(),
    })?;
    if lvm_crc(LVM_CRC_SEED, &buf[4..]) != mah.checksum {
        return Err(bad("metadata area header checksum doesn't match"));
    }
    if mah.version != 1 {
        return Err(bad("metadata area header version isn't 1"));
    }
    if mah.metadata_area_offset != mda_start || mah.metadata_area_size > mda_size {
        return Err(bad("metadata area header doesn't describe its own area"));
    }
    let committed = mah
        .location_descriptors
        .first()
        .context(MissingMetadataSnafu)?;
    if committed.flags & RAW_LOCN_IGNORED != 0 {
        return Err(bad("metadata area is marked ignored"));
    }
    let text = read_metadata_ring(reader, mda_start, &mah, committed)?;
    if lvm_crc(LVM_CRC_SEED, &text) != committed.checksum {
        return Err(bad("metadata text checksum doesn't match"));
    }
    Ok(text)
}

/// The RAID1 legs of each LV segment, read from the same metadata text as
/// [`MetadataRoot`] but kept out of its public types.
#[derive(Deserialize)]
struct RaidVgDesc {
    #[serde(default)]
    logical_volumes: BTreeMap<String, RaidLvDesc>,
}

#[derive(Deserialize)]
struct RaidLvDesc {
    #[serde(flatten)]
    segments: force_de_typed_map::ForceDeTypedMap<String, RaidSegDesc>,
}

#[derive(Deserialize)]
struct RaidSegDesc {
    r#type: String,
    #[serde(default)]
    raids: Vec<String>,
    #[serde(default)]
    data_offset: u64,
}

/// A RAID1 segment's data legs (`rimage` sub-LVs), in leg order.
#[derive(Debug, Clone)]
pub(crate) struct RaidLegs {
    pub(crate) images: Vec<String>,
    /// Set when the legs don't start their data at their first extent (a
    /// reshape), which isn't read.
    pub(crate) data_offset: u64,
}

/// Read the metadata text a location descriptor names from the area that
/// starts at `mda_start`, joining the two pieces when it wraps the ring.
fn read_metadata_ring<R: Read + Seek>(
    reader: &mut R,
    mda_start: u64,
    mah: &MetadataAreaHeader,
    locdesc: &LocationDescriptor,
) -> Result<Vec<u8>, Error> {
    let bad = |reason: &str| Error::Parse {
        reason: reason.to_string(),
    };
    let area = mah.metadata_area_size;
    let (offset, size) = (locdesc.data_area_offset, locdesc.data_area_size);
    if offset < MDA_HEADER_SIZE || offset >= area || size > area - MDA_HEADER_SIZE {
        return Err(bad("metadata text lies outside its metadata area"));
    }
    let len = usize::try_from(size).map_err(|_| bad("metadata area size overflows usize"))?;
    let first = usize::try_from(size.min(area - offset))
        .map_err(|_| bad("metadata area size overflows usize"))?;
    let mut text = vec![0u8; len];
    reader
        .seek(SeekFrom::Start(mda_start + offset))
        .map_err(io_err)?;
    reader
        .read_exact(&mut text[..first])
        .map_err(read_exact_err)?;
    if first < len {
        reader
            .seek(SeekFrom::Start(mda_start + MDA_HEADER_SIZE))
            .map_err(io_err)?;
        reader
            .read_exact(&mut text[first..])
            .map_err(read_exact_err)?;
    }
    Ok(text)
}

/// Map any `embedded_io::Error` into our flat `Io` variant.
#[expect(
    clippy::needless_pass_by_value,
    reason = "used as a `map_err` function argument, which must take E by value"
)]
fn io_err<E: embedded_io::Error>(e: E) -> Error {
    Error::Io { kind: e.kind() }
}

/// Map a `ReadExactError<E>` into either `Io` or `UnexpectedEof`.
fn read_exact_err<E: embedded_io::Error>(e: embedded_io::ReadExactError<E>) -> Error {
    match e {
        embedded_io::ReadExactError::UnexpectedEof => Error::UnexpectedEof,
        embedded_io::ReadExactError::Other(inner) => Error::Io { kind: inner.kind() },
    }
}

/// A parsed LVM2 Physical Volume — Volume Group metadata loaded, ready to
/// open Logical Volumes.
pub struct Lvm2 {
    pvh: PhysicalVolumeHeader,
    pv_name: String,
    vg_name: String,
    vg_config: MetadataRoot,
    /// RAID1 legs by `(LV name, segment name)`.
    raid_legs: BTreeMap<(String, String), RaidLegs>,
}

impl Lvm2 {
    /// Parse the PV label + header + VG metadata from `reader`.
    ///
    /// `reader` must be positioned over the start of the PV (typically the
    /// first byte of a partition that contains LVM2 metadata).
    pub fn open<T: Read + Seek>(mut reader: T) -> Result<Self, Error> {
        let (vhl, label_sector) = find_label(&mut reader)?;
        tracing::trace!(?vhl);
        let pvh_bytes = usize::try_from(vhl.data_offset)
            .ok()
            .and_then(|at| label_sector.get(at..))
            .ok_or_else(|| Error::Parse {
                reason: "PV header offset lies outside the label sector".to_string(),
            })?;
        let (_, pvh) = PhysicalVolumeHeader::parse(pvh_bytes).map_err(|e| Error::Parse {
            reason: e.to_string(),
        })?;
        tracing::trace!(?pvh);

        // A PV carries one or two metadata areas. Use the first whose header
        // and committed text check out, so an ignored or damaged area falls
        // back to the other copy.
        let mut last_error = None;
        let mut metadata_bytes = None;
        for area in &pvh.metadata_descriptors {
            match read_metadata_area(&mut reader, area.offset, area.size) {
                Ok(text) => {
                    metadata_bytes = Some(text);
                    break;
                }
                Err(e) => last_error = Some(e),
            }
        }
        let metadata_bytes = match (metadata_bytes, last_error) {
            (Some(text), _) => text,
            (None, Some(e)) => return Err(e),
            (None, None) => return MissingMetadataSnafu.fail(),
        };
        let metadata = core::str::from_utf8(&metadata_bytes).map_err(|_| Error::MetadataNotUtf8)?;
        tracing::debug!(%metadata);

        let (trailing_garbage, metadata) =
            MetadataElements::parse(metadata).map_err(|e| Error::Parse {
                reason: e.to_string(),
            })?;
        tracing::debug!(?trailing_garbage, ?metadata);

        let meta_root =
            force_de_typed_map::ForceDeTypedMap::<String, MetadataRoot>::deserialize(&metadata)
                .map_err(|e| Error::Serde {
                    reason: e.to_string(),
                })?;
        let raid_vgs =
            force_de_typed_map::ForceDeTypedMap::<String, RaidVgDesc>::deserialize(&metadata)
                .map_err(|e| Error::Serde {
                    reason: e.to_string(),
                })?;
        tracing::debug!(?meta_root);

        ensure!(meta_root.0.len() == 1, MultipleVGsSnafu);
        #[expect(
            clippy::unwrap_used,
            reason = "the ensure! above guarantees exactly one entry"
        )]
        let (vg_name, vg_config) = meta_root.0.into_iter().next().unwrap();

        let pv_name = vg_config
            .physical_volumes
            .iter()
            .find(|(_, v)| v.id.replace('-', "") == pvh.pv_ident)
            .context(PVDoesntContainItselfSnafu)?
            .0
            .clone();

        // rimage sub-LVs alternate with rmeta ones in `raids`.
        let mut raid_legs = BTreeMap::new();
        if let Some(raid_vg) = raid_vgs.0.into_values().next() {
            for (lv_name, lv) in raid_vg.logical_volumes {
                for (seg_name, seg) in lv.segments.0 {
                    if seg.r#type == "raid1" {
                        let images = seg.raids.into_iter().skip(1).step_by(2).collect();
                        raid_legs.insert(
                            (lv_name.clone(), seg_name),
                            RaidLegs {
                                images,
                                data_offset: seg.data_offset,
                            },
                        );
                    }
                }
            }
        }

        Ok(Self {
            pvh,
            pv_name,
            vg_name,
            vg_config,
            raid_legs,
        })
    }

    pub fn pv_id(&self) -> &str {
        &self.vg_config.physical_volumes[&self.pv_name].id
    }

    pub fn pv_name(&self) -> &str {
        &self.pv_name
    }

    pub fn vg_name(&self) -> &str {
        &self.vg_name
    }

    pub fn vg_id(&self) -> &str {
        &self.vg_config.id
    }

    pub fn lvs(&self) -> impl Iterator<Item = LV<'_>> {
        self.vg_config
            .logical_volumes
            .iter()
            .map(|(name, desc)| LV { name, desc })
    }

    pub fn open_lv_by_name<'a, T: Read + Seek>(
        &'a self,
        name: &str,
        reader: T,
    ) -> Option<OpenLV<'a, T>> {
        self.vg_config
            .logical_volumes
            .get_key_value(name)
            .map(move |(name, desc)| self.open_lv(LV { name, desc }, reader))
    }

    pub fn open_lv_by_id<'a, T: Read + Seek>(
        &'a self,
        id: &str,
        reader: T,
    ) -> Option<OpenLV<'a, T>> {
        self.lvs()
            .find(|lv| lv.id() == id)
            .map(move |lv| self.open_lv(lv, reader))
    }

    pub fn open_lv<'a, T: Read + Seek>(&'a self, lv: LV<'a>, reader: T) -> OpenLV<'a, T> {
        OpenLV {
            lv,
            lvm: self,
            reader,
            position: 0,
            current_segment_end: 0,
        }
    }

    pub fn extent_size(&self) -> u64 {
        self.vg_config.extent_size * 512
    }

    /// Access to the parsed PV header (used by `lv.rs` for segment→PV offset
    /// resolution; exposed pub(crate) here to keep the resolver in one place).
    pub(crate) fn pv_header(&self) -> &PhysicalVolumeHeader {
        &self.pvh
    }

    /// The RAID1 legs of segment `seg_name` of LV `lv_name`, if it's a RAID1
    /// segment.
    pub(crate) fn raid_legs(&self, lv_name: &str, seg_name: &str) -> Option<&RaidLegs> {
        self.raid_legs
            .get(&(String::from(lv_name), String::from(seg_name)))
    }

    /// A sub-LV (a RAID1 leg) by name.
    pub(crate) fn sub_lv(&self, name: &str) -> Option<&crate::metadata::LVDesc> {
        self.vg_config.logical_volumes.get(name)
    }

    /// Borrowed lookup helper shared by both the borrowing and owning
    /// LV-open paths.
    pub(crate) fn vg_config_lookup(
        &self,
        name: &str,
    ) -> Option<(&String, &crate::metadata::LVDesc)> {
        self.vg_config.logical_volumes.get_key_value(name)
    }
}
