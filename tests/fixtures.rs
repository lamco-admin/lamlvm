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
    pv(fixture, 0)
}

fn pv(fixture: &str, n: usize) -> Vec<u8> {
    let compressed: &[u8] = match (fixture, n) {
        ("wrapped_metadata", 0) => include_bytes!("fixtures/wrapped_metadata.pv0.img.zst"),
        ("no_timestamp", 0) => include_bytes!("fixtures/no_timestamp.pv0.img.zst"),
        ("striped_and_thin", 0) => include_bytes!("fixtures/striped_and_thin.pv0.img.zst"),
        ("label_sectors", 0) => include_bytes!("fixtures/label_sectors.pv0.img.zst"),
        ("label_sectors", 1) => include_bytes!("fixtures/label_sectors.pv1.img.zst"),
        ("mda_ignored", 0) => include_bytes!("fixtures/mda_ignored.pv0.img.zst"),
        ("mda_ignored", 1) => include_bytes!("fixtures/mda_ignored.pv1.img.zst"),
        ("mda_second_copy", 0) => include_bytes!("fixtures/mda_second_copy.pv0.img.zst"),
        ("raid1", 0) => include_bytes!("fixtures/raid1.pv0.img.zst"),
        ("raid1", 1) => include_bytes!("fixtures/raid1.pv1.img.zst"),
        other => panic!("unknown fixture {other:?}"),
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
    for (name, _, _) in &lvs {
        check_lv(fixture, &image, &lvm, name);
    }
    lvm
}

/// Read LV `name` from `image` and compare it with what the kernel read.
fn check_lv(fixture: &str, image: &[u8], lvm: &Lvm2, name: &str) {
    for (lv_name, size, sha) in expected(fixture) {
        if lv_name != name {
            continue;
        }
        let image = image.to_vec();
        let name = lv_name;
        let mut lv = lvm
            .open_lv_owned_by_name(name, reader(&image))
            .unwrap_or_else(|e| panic!("{fixture}/{name}: open: {e:?}"))
            .unwrap_or_else(|| panic!("{fixture}/{name}: not in the VG"));
        assert_eq!(lv.len(), size, "{fixture}/{name}: size");
        let mut data = vec![0u8; usize::try_from(size).expect("size fits")];
        lv.read_exact(&mut data)
            .unwrap_or_else(|e| panic!("{fixture}/{name}: read: {e:?}"));
        assert_eq!(sha_hex(&data), sha, "{fixture}/{name}: contents");
        // The borrowing reader resolves segments on every seek; it must agree.
        let mut borrowed = lvm
            .open_lv_by_name(name, reader(&image))
            .unwrap_or_else(|| panic!("{fixture}/{name}: not in the VG"));
        let mut again = vec![0u8; data.len()];
        borrowed
            .read_exact(&mut again)
            .unwrap_or_else(|e| panic!("{fixture}/{name}: borrowed read: {e:?}"));
        assert!(again == data, "{fixture}/{name}: borrowed reader differs");
        return;
    }
    panic!("{fixture}/{name}: not in the manifest");
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

fn parse_error(image: &[u8]) -> String {
    match Lvm2::open(reader(image)) {
        Err(lamlvm::Error::Parse { reason }) => reason,
        Err(e) => panic!("expected a parse error, got {e}"),
        Ok(lvm) => panic!("opened VG {}", lvm.vg_name()),
    }
}

/// Labels in sector 0 and sector 3. 0.1.2 looked in sector 1 only.
#[test]
fn labels_in_sectors_0_and_3_read() {
    for (n, lv) in [(0, "on_pv0"), (1, "on_pv1")] {
        let image = pv("label_sectors", n);
        let lvm = Lvm2::open(reader(&image)).unwrap_or_else(|e| panic!("pv{n}: open: {e}"));
        check_lv("label_sectors", &image, &lvm, lv);
    }
}

/// pv0's only metadata area is marked ignored and still holds text from
/// before LV `late` existed. 0.1.2 read that stale text; it's refused now,
/// while pv1's current copy reads.
#[test]
fn an_ignored_metadata_area_is_not_read() {
    let reason = parse_error(&pv("mda_ignored", 0));
    assert!(reason.contains("ignored"), "{reason}");
    let lvm = Lvm2::open(reader(&pv("mda_ignored", 1))).expect("pv1 opens");
    for name in ["root", "late"] {
        assert!(lvm.lvs().any(|lv| lv.name() == name), "{name} missing");
    }
}

/// The first of two metadata areas holds text that still parses but fails its
/// checksum (root has 5 extents instead of 4). 0.1.2 read it and gave root the
/// wrong size; the second copy is used now.
#[test]
fn a_damaged_metadata_area_falls_back_to_the_second() {
    let lvm = check_lvs("mda_second_copy");
    assert_eq!(lvm.vg_name(), "lamlvm_mda2");
}

/// A RAID1 LV with a leg on each PV reads from either PV. 0.1.2 refused it as
/// not linear.
#[test]
fn raid1_reads_from_the_leg_on_each_pv() {
    for n in [0, 1] {
        let image = pv("raid1", n);
        let lvm = Lvm2::open(reader(&image)).unwrap_or_else(|e| panic!("pv{n}: open: {e}"));
        check_lv("raid1", &image, &lvm, "mirror");
    }
}

/// A label whose checksum doesn't match is refused. 0.1.2 never checked it.
#[test]
fn a_damaged_label_is_refused() {
    let mut image = first_pv("wrapped_metadata");
    // A byte of the PV UUID, inside the label checksum's span.
    image[512 + 40] ^= 0x01;
    let reason = parse_error(&image);
    assert!(reason.contains("label checksum"), "{reason}");
}

/// A metadata area header whose checksum doesn't match is refused when it's
/// the only area. 0.1.2 never checked it.
#[test]
fn a_damaged_metadata_area_header_is_refused() {
    let mut image = first_pv("wrapped_metadata");
    // The first metadata area starts at 4096 on these PVs; byte 300 of its
    // header is padding the checksum covers.
    image[4096 + 300] ^= 0x01;
    let reason = parse_error(&image);
    assert!(reason.contains("header checksum"), "{reason}");
}
