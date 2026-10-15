//! Oracle tests against images the LVM2 tools made. Every LV the kernel read
//! comes back with the bytes device-mapper showed, and an LV lamlvm can't read
//! fails on its own without taking the volume group with it. Regenerate with
//! `tests/fixtures/regenerate.sh`.
#![expect(
    clippy::expect_used,
    reason = "a fixture that doesn't decode or parse is a test failure, so panicking with the reason is right"
)]

use std::io::{Cursor, Read};

use embedded_io::Read as _;
use embedded_io_adapters::std::FromStd;
use lamlvm::{Lvm2, OpenLvError};
use sha2::{Digest, Sha256};

const MANIFEST: &str = include_str!("fixtures/MANIFEST");

type Pv = FromStd<Cursor<Vec<u8>>>;

fn first_pv(fixture: &str) -> Vec<u8> {
    let compressed: &[u8] = match fixture {
        "wrapped_metadata" => include_bytes!("fixtures/wrapped_metadata.pv0.img.zst"),
        "no_timestamp" => include_bytes!("fixtures/no_timestamp.pv0.img.zst"),
        "striped_and_thin" => include_bytes!("fixtures/striped_and_thin.pv0.img.zst"),
        other => panic!("unknown fixture {other}"),
    };
    let mut out = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(compressed)
        .expect("zstd header")
        .read_to_end(&mut out)
        .expect("decompress");
    out
}

fn reader(image: &[u8]) -> Pv {
    FromStd::new(Cursor::new(image.to_vec()))
}

/// `(lv, size, sha256)` for every LV the manifest lists under `fixture`.
fn expected(fixture: &str) -> Vec<(&'static str, u64, &'static str)> {
    MANIFEST
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split('|');
            let (fx, lv, size, sha) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            (fx == fixture).then(|| (lv, size.parse().expect("size"), sha))
        })
        .collect()
}

fn sha_hex(data: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(data)
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// Read every LV the manifest lists and compare it with what the kernel read.
fn check_lvs(fixture: &str) -> Lvm2 {
    let image = first_pv(fixture);
    let lvm = Lvm2::open(reader(&image)).unwrap_or_else(|e| panic!("{fixture}: open: {e}"));
    let lvs = expected(fixture);
    assert!(!lvs.is_empty(), "{fixture}: no LVs in the manifest");
    for (name, size, sha) in lvs {
        let mut lv = lvm
            .open_lv_owned_by_name(name, reader(&image))
            .unwrap_or_else(|e| panic!("{fixture}/{name}: open: {e:?}"))
            .unwrap_or_else(|| panic!("{fixture}/{name}: not in the VG"));
        assert_eq!(lv.len(), size, "{fixture}/{name}: size");
        let mut data = vec![0u8; usize::try_from(size).expect("size fits")];
        lv.read_exact(&mut data)
            .unwrap_or_else(|e| panic!("{fixture}/{name}: read: {e:?}"));
        assert_eq!(sha_hex(&data), sha, "{fixture}/{name}: contents");
    }
    lvm
}

/// The committed metadata text runs past the end of its ring buffer and
/// continues at the start. 0.1.1 read it in one piece and failed.
#[test]
fn wrapped_metadata_reads() {
    let lvm = check_lvs("wrapped_metadata");
    assert_eq!(lvm.vg_name(), "lamlvm_wrap");
}

/// LVM2 leaves creation_time and creation_host out for an LV without a
/// timestamp. 0.1.1 refused the whole volume group over it.
#[test]
fn lv_without_timestamp_reads() {
    let lvm = check_lvs("no_timestamp");
    let time = |name: &str| {
        lvm.lvs().find(|lv| lv.name() == name).map_or_else(
            || panic!("{name} missing"),
            |lv| lv.raw_metadata().creation_time,
        )
    };
    assert_eq!(time("unstamped"), 0);
    assert_ne!(time("stamped"), 0);
}

/// A striped LV and a thin pool sit next to a linear root. 0.1.1 refused the
/// whole volume group over the striped segment; now only those LVs fail.
#[test]
fn unsupported_lvs_fail_alone() {
    let image = first_pv("striped_and_thin");
    let lvm = check_lvs("striped_and_thin");
    for name in ["striped", "pool", "thin"] {
        assert!(
            lvm.lvs().any(|lv| lv.name() == name),
            "{name} should be listed"
        );
        match lvm.open_lv_owned_by_name(name, reader(&image)) {
            Err(OpenLvError::Unsupported(_)) => {}
            Err(e) => panic!("{name}: expected Unsupported, got {e:?}"),
            Ok(lv) => panic!("{name}: opened ({:?} bytes)", lv.map(|l| l.len())),
        }
    }
}
