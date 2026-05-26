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
//! Linear logical volumes on a single physical volume only. Striped,
//! mirrored, thin pool, snapshot, and cache LVs return [`Error::Unsupported`]
//! or analogous errors. This covers the canonical layout used by Proxmox VE
//! and most default single-disk LVM installs. See `PROVENANCE.md` for the
//! full coverage matrix and the rationale for the narrow scope.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_io::{ErrorKind, Read, Seek, SeekFrom};
use serde::Deserialize;
use snafu::{Snafu, ensure, OptionExt};

use crate::header::{MetadataAreaHeader, PhysicalVolumeHeader, PhysicalVolumeLabelHeader};
use crate::metadata::{deserialize::MetadataElements, MetadataRoot};

mod header;
mod force_de_typed_map;
mod lv;
pub mod metadata;

pub use lv::*;

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

/// Map any `embedded_io::Error` into our flat `Io` variant.
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
}

impl Lvm2 {
    /// Parse the PV label + header + VG metadata from `reader`.
    ///
    /// `reader` must be positioned over the start of the PV (typically the
    /// first byte of a partition that contains LVM2 metadata).
    pub fn open<T: Read + Seek>(mut reader: T) -> Result<Self, Error> {
        // Sheet 0 is zero-padding; PV label header lives at sheet 1.
        reader
            .seek(SeekFrom::Start(512))
            .map_err(io_err)?;

        let mut buf = [0u8; 512];
        reader.read_exact(&mut buf).map_err(read_exact_err)?;
        tracing::trace!(?buf);

        let (_, vhl) = PhysicalVolumeLabelHeader::parse(&buf)
            .map_err(|e| Error::Parse { reason: e.to_string() })?;
        tracing::trace!(?vhl);
        let (_, pvh) = PhysicalVolumeHeader::parse(&buf[(vhl.data_offset as usize)..])
            .map_err(|e| Error::Parse { reason: e.to_string() })?;
        tracing::trace!(?pvh);

        let metadata_descriptor = pvh.metadata_descriptors.first().context(MissingMetadataSnafu)?;

        reader
            .seek(SeekFrom::Start(metadata_descriptor.offset))
            .map_err(io_err)?;
        reader.read_exact(&mut buf).map_err(read_exact_err)?;
        let (_, mah) = MetadataAreaHeader::parse(&buf)
            .map_err(|e| Error::Parse { reason: e.to_string() })?;
        tracing::trace!(?mah);

        // Read the VG metadata text. Upstream used `read_to_string` which
        // doesn't exist on embedded-io. Equivalent: allocate per
        // location descriptor, read_exact, then UTF-8-validate at the end.
        let mut metadata_bytes: Vec<u8> = Vec::new();
        for locdesc in &mah.location_descriptors {
            reader
                .seek(SeekFrom::Start(
                    metadata_descriptor.offset + locdesc.data_area_offset,
                ))
                .map_err(io_err)?;
            let len = usize::try_from(locdesc.data_area_size)
                .map_err(|_| Error::Parse { reason: "metadata area size overflows usize".to_string() })?;
            let start = metadata_bytes.len();
            metadata_bytes.resize(start + len, 0);
            reader
                .read_exact(&mut metadata_bytes[start..])
                .map_err(read_exact_err)?;
        }
        let metadata = core::str::from_utf8(&metadata_bytes)
            .map_err(|_| Error::MetadataNotUtf8)?;
        tracing::debug!(%metadata);

        let (trailing_garbage, metadata) = MetadataElements::parse(metadata)
            .map_err(|e| Error::Parse { reason: e.to_string() })?;
        tracing::debug!(?trailing_garbage, ?metadata);

        let meta_root = force_de_typed_map::ForceDeTypedMap::<String, MetadataRoot>::deserialize(
            &metadata,
        )
        .map_err(|e| Error::Serde { reason: e.to_string() })?;
        tracing::debug!(?meta_root);

        ensure!(meta_root.0.len() == 1, MultipleVGsSnafu);
        let (vg_name, vg_config) = meta_root.0.into_iter().next().unwrap();

        let pv_name = vg_config
            .physical_volumes
            .iter()
            .find(|(_, v)| v.id.replace('-', "") == pvh.pv_ident)
            .context(PVDoesntContainItselfSnafu)?
            .0
            .clone();

        Ok(Self { pvh, pv_name, vg_name, vg_config })
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
}
