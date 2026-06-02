//! Lifetime-free LV reader.
//!
//! [`OwnedLvReader`] pre-computes the LV→PV byte-offset mapping at open
//! time and stores its own copies of everything it needs from `Lvm2`. The
//! resulting struct has no lifetime parameter, so it can be embedded in
//! long-lived filesystem-backend types (e.g. LamBoot's `FsBackend`
//! instances) that outlive the parsed `Lvm2`.
//!
//! Trade-off vs the borrowed [`OpenLV`](crate::OpenLV):
//!
//! * Borrowed `OpenLV`: zero-copy, but ties reader lifetime to the
//!   `Lvm2`. Seek is O(n_segments) × O(n_data_descriptors).
//! * Owned `OwnedLvReader`: one allocation of a small `Vec<SegmentMap>`
//!   at open time. Seek is O(log n_segments). No lifetime constraints.
//!
//! Owned is the right default when the segment count is small (which it
//! is for every Proxmox host install — usually 1-3 segments per LV).

use alloc::vec::Vec;
use embedded_io::{ErrorType, Read, Seek, SeekFrom};

use crate::Lvm2;
use crate::lv::{LV, OpenLvError};

/// A single segment's LV→PV byte mapping. Within `[lv_start, lv_end)` the
/// mapping is linear: `pv_byte = pv_base + (lv_byte - lv_start)`.
#[derive(Clone, Copy, Debug)]
struct SegmentMap {
    lv_start: u64,
    lv_end: u64,
    pv_base: u64,
}

/// Lifetime-free, owning LV reader.
///
/// Construct via [`Lvm2::open_lv_owned_by_name`] or
/// [`Lvm2::open_lv_owned_by_id`]. Implements `embedded_io::Read + Seek`.
///
/// The reader owns:
/// * The PV-byte reader `T`
/// * A pre-computed `Vec<SegmentMap>` of the LV's segment table
/// * The total LV byte length (for `SeekFrom::End`)
pub struct OwnedLvReader<T> {
    reader: T,
    segments: Vec<SegmentMap>,
    lv_len: u64,
    position: u64,
}

impl<T: ErrorType> ErrorType for OwnedLvReader<T> {
    type Error = OpenLvError<T::Error>;
}

impl<T: Read + Seek> OwnedLvReader<T> {
    /// Build an owned reader from an `Lvm2` + `LV` + a PV reader, by
    /// pre-resolving every segment's LV-byte range to a PV-byte base
    /// offset. Returns `Unsupported` if any segment is non-linear,
    /// references a different PV, or extends beyond the PV data area.
    pub(crate) fn build(lvm: &Lvm2, lv: LV<'_>, reader: T) -> Result<Self, OpenLvError<T::Error>> {
        let extent_size = lvm.extent_size();
        let mut segments = Vec::with_capacity(lv.raw_metadata().segments.0.len());

        for seg in lv.raw_metadata().segments.0.values() {
            if seg.r#type != "striped" || seg.stripe_count != Some(1) {
                return Err(OpenLvError::Unsupported("segment is not linear"));
            }
            let (pv, pv_extent) = seg
                .stripes
                .as_ref()
                .ok_or(OpenLvError::Unsupported("segment has no stripes"))?;
            if pv != lvm.pv_name() {
                return Err(OpenLvError::Unsupported(
                    "segment data is on a different PV than the one opened",
                ));
            }

            let lv_start = seg
                .start_extent
                .checked_mul(extent_size)
                .ok_or(OpenLvError::SeekOverflow)?;
            let seg_bytes = seg
                .extent_count
                .checked_mul(extent_size)
                .ok_or(OpenLvError::SeekOverflow)?;
            let lv_end = lv_start
                .checked_add(seg_bytes)
                .ok_or(OpenLvError::SeekOverflow)?;

            // Resolve the PV-extent index to a byte offset on the PV by
            // walking the data descriptors. Same logic as the borrowed
            // variant in lv.rs — kept duplicated here to avoid pulling
            // pv_header into a public surface.
            let mut seek_target = pv_extent
                .checked_mul(extent_size)
                .ok_or(OpenLvError::SeekOverflow)?;
            let mut resolved = false;
            for dd in &lvm.pv_header().data_descriptors {
                if dd.size == 0 || dd.size > seek_target {
                    seek_target = seek_target
                        .checked_add(dd.offset)
                        .ok_or(OpenLvError::SeekOverflow)?;
                    resolved = true;
                    break;
                }
                seek_target = seek_target
                    .checked_sub(dd.size)
                    .ok_or(OpenLvError::SeekOverflow)?;
            }
            if !resolved {
                return Err(OpenLvError::Unsupported(
                    "data is beyond the end of this PV",
                ));
            }

            segments.push(SegmentMap {
                lv_start,
                lv_end,
                pv_base: seek_target,
            });
        }

        // Sort by lv_start so binary search works in seek().
        segments.sort_unstable_by_key(|s| s.lv_start);

        let lv_len = segments.last().map_or(0, |s| s.lv_end);

        Ok(Self {
            reader,
            segments,
            lv_len,
            position: 0,
        })
    }

    /// Total LV size in bytes (sum of all segments' byte spans).
    pub fn len(&self) -> u64 {
        self.lv_len
    }

    /// True if `self.len() == 0`.
    pub fn is_empty(&self) -> bool {
        self.lv_len == 0
    }

    /// Underlying PV reader. Useful for tests and diagnostics; do not use
    /// to bypass the LV→PV mapping.
    pub fn into_inner(self) -> T {
        self.reader
    }

    /// Resolve an LV byte offset to the segment containing it.
    fn find_segment(&self, lv_pos: u64) -> Option<&SegmentMap> {
        // Binary search by lv_start; the matching segment is the
        // largest one whose lv_start <= lv_pos and whose lv_end > lv_pos.
        match self.segments.binary_search_by_key(&lv_pos, |s| s.lv_start) {
            Ok(i) => self.segments.get(i),
            Err(0) => None, // lv_pos < first segment's start
            Err(i) => {
                let s = self.segments.get(i - 1)?;
                if lv_pos < s.lv_end { Some(s) } else { None }
            }
        }
    }
}

impl<T: Read + Seek> Read for OwnedLvReader<T> {
    fn read(&mut self, mut buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(seg) = self.find_segment(self.position) else {
            // Either past EOF or in a gap (gaps shouldn't exist for
            // canonical linear LVs but are not our problem to invent).
            return Ok(0);
        };

        // Clamp to segment boundary so we never read across a segment
        // edge in one call. Callers (Ext4Read adapter, std::io::Read
        // bridges) handle short reads correctly.
        let remaining_in_segment = seg.lv_end - self.position;
        if (buf.len() as u64) > remaining_in_segment {
            let len = usize::try_from(remaining_in_segment).unwrap_or(buf.len());
            buf = &mut buf[..len];
        }

        let pv_offset = seg.pv_base + (self.position - seg.lv_start);
        self.reader
            .seek(SeekFrom::Start(pv_offset))
            .map_err(OpenLvError::Pv)?;
        let n = self.reader.read(buf).map_err(OpenLvError::Pv)?;
        self.position = self.position.saturating_add(n as u64);
        Ok(n)
    }
}

impl<T: Read + Seek> Seek for OwnedLvReader<T> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos = match pos {
            SeekFrom::Start(x) => x,
            SeekFrom::End(x) => {
                let signed = i64::try_from(self.lv_len).map_err(|_| OpenLvError::SeekOverflow)?;
                let t = signed.checked_add(x).ok_or(OpenLvError::SeekOverflow)?;
                u64::try_from(t).map_err(|_| OpenLvError::SeekOverflow)?
            }
            SeekFrom::Current(x) => {
                let signed = i64::try_from(self.position).map_err(|_| OpenLvError::SeekOverflow)?;
                let t = signed.checked_add(x).ok_or(OpenLvError::SeekOverflow)?;
                u64::try_from(t).map_err(|_| OpenLvError::SeekOverflow)?
            }
        };
        // Seeking past EOF is allowed (matches std::io::Seek); the next
        // read() will return 0.
        self.position = new_pos;
        Ok(new_pos)
    }
}

impl Lvm2 {
    /// Open an LV by name and return a lifetime-free owning reader.
    pub fn open_lv_owned_by_name<T: Read + Seek>(
        &self,
        name: &str,
        reader: T,
    ) -> Result<Option<OwnedLvReader<T>>, OpenLvError<T::Error>> {
        let Some((name, desc)) = self.vg_config_lookup(name) else {
            return Ok(None);
        };
        let lv = LV { name, desc };
        Ok(Some(OwnedLvReader::build(self, lv, reader)?))
    }

    /// Open an LV by UUID and return a lifetime-free owning reader.
    pub fn open_lv_owned_by_id<T: Read + Seek>(
        &self,
        id: &str,
        reader: T,
    ) -> Result<Option<OwnedLvReader<T>>, OpenLvError<T::Error>> {
        let Some(lv) = self.lvs().find(|lv| lv.id() == id) else {
            return Ok(None);
        };
        Ok(Some(OwnedLvReader::build(self, lv, reader)?))
    }
}
