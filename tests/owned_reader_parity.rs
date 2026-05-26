//! Integration test: read every byte of an LV via `OwnedLvReader` and
//! compare against the same bytes read via the borrowed `OpenLV`. Any
//! divergence would indicate that the pre-resolved segment-map in
//! `OwnedLvReader` and the on-demand resolution in `OpenLV` disagree,
//! which would be a correctness bug.
//!
//! Requires the `image.raw` fixture. Build it via `tests/build-fixture.sh`.

use std::fs::File;

use embedded_io::{Read, Seek, SeekFrom};
use embedded_io_adapters::std::FromStd;

type EioFile = FromStd<File>;

#[test]
fn owned_reader_parity_with_open_lv() {
    let path = "image.raw";
    if !std::path::Path::new(path).exists() {
        eprintln!(
            "skipping owned_reader_parity: {path} not present. \
             Build it via `sudo tests/build-fixture.sh`."
        );
        return;
    }

    let mut f0: EioFile = FromStd::new(File::open(path).expect("open image.raw"));
    let lvm = lamlvm::Lvm2::open(&mut f0).expect("parse PV");
    let first_lv = lvm.lvs().next().expect("test image has at least one LV");
    let lv_name = first_lv.name().to_string();

    // --- Path A: borrowed OpenLV ---
    let f1: EioFile = FromStd::new(File::open(path).expect("re-open for borrowed reader"));
    let mut borrowed = lvm.open_lv_by_name(&lv_name, f1).expect("LV not found");

    // --- Path B: owned LvReader ---
    let f2: EioFile = FromStd::new(File::open(path).expect("re-open for owned reader"));
    let mut owned = lvm
        .open_lv_owned_by_name(&lv_name, f2)
        .expect("owned-open errored")
        .expect("LV not found via owned API");

    // Length sanity check first — the owned reader pre-computes total length.
    let extents = first_lv.size_in_extents();
    let total_bytes = extents * lvm.extent_size();
    assert_eq!(owned.len(), total_bytes, "owned len() vs metadata");

    // Read both end-to-end in chunks and compare. Use a chunk size that
    // straddles segment boundaries for any multi-segment LVs.
    const CHUNK: usize = 4096;
    let mut buf_a = vec![0u8; CHUNK];
    let mut buf_b = vec![0u8; CHUNK];

    borrowed.seek(SeekFrom::Start(0)).expect("borrowed seek 0");
    owned.seek(SeekFrom::Start(0)).expect("owned seek 0");

    let mut total = 0u64;
    loop {
        let na = read_short_ok(&mut borrowed, &mut buf_a);
        let nb = read_short_ok(&mut owned, &mut buf_b);
        assert_eq!(
            na, nb,
            "short-read disagreement at offset {total}: borrowed={na} owned={nb}"
        );
        if na == 0 {
            break;
        }
        assert_eq!(
            &buf_a[..na],
            &buf_b[..nb],
            "byte mismatch at LV offset {total}..{}",
            total + na as u64,
        );
        total += na as u64;
        if total >= total_bytes {
            break;
        }
    }
    assert_eq!(total, total_bytes, "did not read full LV via owned reader");

    // Targeted seek-and-read at three pseudo-random offsets to exercise
    // the binary-search path in OwnedLvReader::find_segment.
    for &offset in &[0u64, total_bytes / 3, total_bytes - 512] {
        let mut x = [0u8; 256];
        let mut y = [0u8; 256];
        borrowed.seek(SeekFrom::Start(offset)).expect("seek borrowed");
        owned.seek(SeekFrom::Start(offset)).expect("seek owned");
        borrowed.read_exact(&mut x).expect("read_exact borrowed");
        owned.read_exact(&mut y).expect("read_exact owned");
        assert_eq!(x, y, "byte mismatch in targeted read at offset {offset}");
    }
}

/// `embedded_io::Read::read` may return Ok(short) at segment boundaries.
/// Wrap it so the comparison loop sees the same short-read count from
/// both readers when they hit the same boundary, which is the key
/// property we're verifying.
fn read_short_ok<R: Read>(r: &mut R, buf: &mut [u8]) -> usize
where
    R::Error: std::fmt::Debug,
{
    r.read(buf).expect("short read on integration fixture")
}
