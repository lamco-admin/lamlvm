//! Logical Volume access — `OpenLV` implements `embedded_io::Read + Seek`
//! by mapping LV offsets to PV offsets via the linear-segment table.

use alloc::vec::Vec;

use embedded_io::{ErrorKind, ErrorType, Read, Seek, SeekFrom};

use crate::Lvm2;
use crate::metadata::{LVDesc, LVSegmentDesc};

/// A logical volume descriptor, borrowed from a parsed `Lvm2`.
#[derive(Clone, Copy)]
pub struct LV<'a> {
    pub(crate) name: &'a str,
    pub(crate) desc: &'a LVDesc,
}

impl<'a> LV<'a> {
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn id(&self) -> &'a str {
        &self.desc.id
    }

    pub fn size_in_extents(&self) -> u64 {
        self.desc
            .segments
            .0
            .values()
            .map(|x| x.start_extent + x.extent_count)
            .max()
            // A segmentless LV (only reachable from malformed metadata) is zero extents.
            .unwrap_or(0)
    }

    pub fn raw_metadata(&self) -> &'a LVDesc {
        self.desc
    }
}

/// A run of LV extents stored contiguously on the PV opened.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PvRun {
    /// First LV extent of the run.
    pub(crate) lv_start: u64,
    /// One past the run's last LV extent.
    pub(crate) lv_end: u64,
    /// PV byte offset of the run's first extent.
    pub(crate) pv_base: u64,
}

/// Where the extents of segment `seg_name` of LV `lv_name` live on the PV
/// opened: one run for a linear segment, and for a RAID1 segment the runs of
/// the first leg that's whole on this PV.
pub(crate) fn segment_runs(
    lvm: &Lvm2,
    lv_name: &str,
    seg_name: &str,
    seg: &LVSegmentDesc,
) -> Result<Vec<PvRun>, &'static str> {
    if seg.r#type == "raid1" {
        return raid1_runs(lvm, lv_name, seg_name, seg);
    }
    Ok(alloc::vec![linear_run(lvm, seg)?])
}

/// A linear segment's run on the PV opened.
fn linear_run(lvm: &Lvm2, seg: &LVSegmentDesc) -> Result<PvRun, &'static str> {
    if seg.r#type != "striped" || seg.stripe_count != Some(1) {
        return Err("segment is not linear");
    }
    let (pv, pv_extent) = seg.stripes.as_ref().ok_or("segment has no stripes")?;
    if pv != lvm.pv_name() {
        return Err("segment data is on a different PV than the one opened");
    }
    let extent_size = lvm.extent_size();
    let lv_end = seg
        .start_extent
        .checked_add(seg.extent_count)
        .ok_or("segment extents overflow")?;
    let run_bytes = seg
        .extent_count
        .checked_mul(extent_size)
        .ok_or("segment extents overflow")?;
    let mut in_area = pv_extent
        .checked_mul(extent_size)
        .ok_or("segment extents overflow")?;
    // Walk the PV's data areas to the one holding this extent; the run must
    // fit inside it.
    for dd in &lvm.pv_header().data_descriptors {
        if dd.size == 0 || dd.size > in_area {
            if dd.size != 0
                && in_area
                    .checked_add(run_bytes)
                    .is_none_or(|end| end > dd.size)
            {
                return Err("segment runs past the end of its data area");
            }
            let pv_base = in_area
                .checked_add(dd.offset)
                .ok_or("segment extents overflow")?;
            return Ok(PvRun {
                lv_start: seg.start_extent,
                lv_end,
                pv_base,
            });
        }
        in_area -= dd.size;
    }
    Err("data is beyond the end of this PV")
}

/// A RAID1 segment's runs, read from the first leg that's whole on the PV
/// opened. Every leg holds the full data, and leg extent `n` holds LV extent
/// `n`.
fn raid1_runs(
    lvm: &Lvm2,
    lv_name: &str,
    seg_name: &str,
    seg: &LVSegmentDesc,
) -> Result<Vec<PvRun>, &'static str> {
    let legs = lvm
        .raid_legs(lv_name, seg_name)
        .ok_or("RAID1 segment lists no legs")?;
    if legs.data_offset != 0 {
        return Err("RAID1 legs keep their data past their first extent (a reshape)");
    }
    let end = seg
        .start_extent
        .checked_add(seg.extent_count)
        .ok_or("segment extents overflow")?;
    let mut last = "no RAID1 leg is on the PV opened";
    for image in &legs.images {
        let Some(leg) = lvm.sub_lv(image) else {
            last = "a RAID1 leg isn't in the metadata";
            continue;
        };
        // A leg LVM is still rebuilding doesn't hold the data yet.
        if leg.status.iter().chain(&leg.flags).any(|f| f == "REBUILD") {
            last = "the RAID1 leg is being rebuilt";
            continue;
        }
        match leg_runs(lvm, leg, seg.start_extent, end) {
            Ok(runs) => return Ok(runs),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// The runs of `leg` that cover LV extents `start..end`, which must all be
/// linear on the PV opened.
fn leg_runs(lvm: &Lvm2, leg: &LVDesc, start: u64, end: u64) -> Result<Vec<PvRun>, &'static str> {
    let extent_size = lvm.extent_size();
    let mut segs: Vec<&LVSegmentDesc> = leg.segments.0.values().collect();
    segs.sort_unstable_by_key(|s| s.start_extent);
    let mut runs = Vec::new();
    let mut covered = start;
    for seg in segs {
        let seg_end = seg
            .start_extent
            .checked_add(seg.extent_count)
            .ok_or("segment extents overflow")?;
        if seg_end <= start || seg.start_extent >= end {
            continue;
        }
        let run = linear_run(lvm, seg)?;
        let lo = run.lv_start.max(start);
        let hi = run.lv_end.min(end);
        if lo != covered {
            return Err("a RAID1 leg doesn't cover its LV");
        }
        let skip = (lo - run.lv_start)
            .checked_mul(extent_size)
            .and_then(|b| b.checked_add(run.pv_base))
            .ok_or("segment extents overflow")?;
        runs.push(PvRun {
            lv_start: lo,
            lv_end: hi,
            pv_base: skip,
        });
        covered = hi;
    }
    if covered != end {
        return Err("a RAID1 leg doesn't cover its LV");
    }
    Ok(runs)
}

/// An open, byte-addressable view of a Logical Volume.
///
/// Implements `embedded_io::Read + Seek`. Reads map LV offsets to PV
/// offsets via the segment table; linear segments on the current PV and
/// RAID1 segments with a leg on it are supported (anything else errors with
/// `OpenLvError::Unsupported`).
pub struct OpenLV<'a, T> {
    pub(crate) lv: LV<'a>,
    pub(crate) lvm: &'a Lvm2,
    pub(crate) reader: T,

    pub(crate) position: u64,
    pub(crate) current_segment_end: u64,
}

/// Errors raised by `OpenLV`'s `Read + Seek` implementations.
///
/// Implements `embedded_io::Error` so the LamBoot adapter can flatten
/// everything to a kind via `.kind()` cleanly.
#[derive(Debug)]
pub enum OpenLvError<E> {
    /// Underlying PV-reader error.
    Pv(E),

    /// LV-level position is not covered by any segment.
    NoSegment,

    /// Segment type or layout we don't support (non-linear, multi-stripe,
    /// or data on a different PV than the one we opened).
    Unsupported(&'static str),

    /// Arithmetic overflow / underflow in seek offset computation.
    SeekOverflow,
}

impl<E: embedded_io::Error> embedded_io::Error for OpenLvError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            OpenLvError::Pv(inner) => inner.kind(),
            OpenLvError::NoSegment | OpenLvError::SeekOverflow => ErrorKind::InvalidInput,
            OpenLvError::Unsupported(_) => ErrorKind::Unsupported,
        }
    }
}

impl<T: ErrorType> ErrorType for OpenLV<'_, T> {
    type Error = OpenLvError<T::Error>;
}

impl<T: Read + Seek> Read for OpenLV<'_, T> {
    fn read(&mut self, mut buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.position == self.current_segment_end {
            // Re-seek to current position to load the next segment's bounds.
            self.seek(SeekFrom::Current(0))?;
        }

        let max_read = self.current_segment_end - self.position;
        if u64::try_from(buf.len()).unwrap_or(u64::MAX) > max_read {
            let len = usize::try_from(max_read).unwrap_or(buf.len());
            buf = &mut buf[..len];
        }
        let n = self.reader.read(buf).map_err(OpenLvError::Pv)?;
        self.position = self.position.saturating_add(n as u64);
        Ok(n)
    }
}

impl<T: Read + Seek> Seek for OpenLV<'_, T> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        // Resolve the target LV-relative byte offset, with checked arithmetic
        // to avoid the silent-wrap behavior of the upstream `as i64 / as u64`
        // casts. See PROVENANCE.md for the rationale.
        let lv_size_bytes = self
            .lv
            .size_in_extents()
            .checked_mul(self.lvm.extent_size())
            .ok_or(OpenLvError::SeekOverflow)?;
        let pos = match pos {
            SeekFrom::Start(x) => x,
            SeekFrom::End(x) => {
                let signed = i64::try_from(lv_size_bytes).map_err(|_| OpenLvError::SeekOverflow)?;
                let target = signed.checked_add(x).ok_or(OpenLvError::SeekOverflow)?;
                u64::try_from(target).map_err(|_| OpenLvError::SeekOverflow)?
            }
            SeekFrom::Current(x) => {
                let signed = i64::try_from(self.position).map_err(|_| OpenLvError::SeekOverflow)?;
                let target = signed.checked_add(x).ok_or(OpenLvError::SeekOverflow)?;
                u64::try_from(target).map_err(|_| OpenLvError::SeekOverflow)?
            }
        };

        let extent_size = self.lvm.extent_size();
        let target_extent = pos / extent_size;

        let (seg_name, segment) = self
            .lv
            .desc
            .segments
            .0
            .iter()
            .find(|(_, x)| x.extents().contains(&target_extent))
            .ok_or(OpenLvError::NoSegment)?;
        let runs = segment_runs(self.lvm, self.lv.name, seg_name, segment)
            .map_err(OpenLvError::Unsupported)?;
        let run = runs
            .iter()
            .find(|r| (r.lv_start..r.lv_end).contains(&target_extent))
            .ok_or(OpenLvError::NoSegment)?;
        let seek_target = run.lv_start * extent_size;
        let seek_target = run.pv_base + (pos - seek_target);

        self.reader
            .seek(SeekFrom::Start(seek_target))
            .map_err(OpenLvError::Pv)?;

        self.current_segment_end = run.lv_end * extent_size;
        self.position = pos;
        Ok(pos)
    }
}
