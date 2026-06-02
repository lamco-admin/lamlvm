//! Logical Volume access — `OpenLV` implements `embedded_io::Read + Seek`
//! by mapping LV offsets to PV offsets via the linear-segment table.

use embedded_io::{ErrorKind, ErrorType, Read, Seek, SeekFrom};

use crate::Lvm2;
use crate::metadata::LVDesc;

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

/// An open, byte-addressable view of a Logical Volume.
///
/// Implements `embedded_io::Read + Seek`. Reads map LV offsets to PV
/// offsets via the segment table; only linear segments on the current PV
/// are supported (anything else errors with `OpenLvError::Unsupported`).
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

        let target_extent = pos / self.lvm.extent_size();

        let segment = self
            .lv
            .desc
            .segments
            .0
            .values()
            .find(|x| x.extents().contains(&target_extent))
            .ok_or(OpenLvError::NoSegment)?;
        if segment.r#type != "striped" || segment.stripe_count != Some(1) {
            return Err(OpenLvError::Unsupported("segment is not linear"));
        }

        let offs_in_segment = pos - (segment.start_extent * self.lvm.extent_size());

        let (pv, loc) = segment
            .stripes
            .as_ref()
            .ok_or(OpenLvError::Unsupported("segment has no stripes"))?;
        if pv != self.lvm.pv_name() {
            return Err(OpenLvError::Unsupported(
                "segment data is on a different PV than the one opened",
            ));
        }

        let mut seek_target = loc * self.lvm.extent_size() + offs_in_segment;
        let mut found = false;
        for dd in &self.lvm.pv_header().data_descriptors {
            if dd.size == 0 || dd.size > seek_target {
                seek_target += dd.offset;
                found = true;
                break;
            }
            seek_target -= dd.size;
        }
        if !found {
            return Err(OpenLvError::Unsupported(
                "data is beyond the end of this PV",
            ));
        }

        self.reader
            .seek(SeekFrom::Start(seek_target))
            .map_err(OpenLvError::Pv)?;

        self.current_segment_end = segment.extents().end * self.lvm.extent_size();
        self.position = pos;
        Ok(pos)
    }
}
