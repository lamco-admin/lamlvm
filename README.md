# lamlvm

[![Crates.io](https://img.shields.io/crates/v/lamlvm.svg)](https://crates.io/crates/lamlvm)
[![Docs.rs](https://docs.rs/lamlvm/badge.svg)](https://docs.rs/lamlvm)
[![CI](https://github.com/lamco-admin/lamlvm/actions/workflows/ci.yml/badge.svg)](https://github.com/lamco-admin/lamlvm/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/lamco-admin/lamlvm/blob/main/LICENSE-MIT)

Read-only LVM2 Logical Volume reader for `no_std` environments — primarily UEFI bootloaders that need to mount a filesystem from a logical volume without pulling in a userspace LVM stack.

Vendored + modernized fork of [main--/rust-lvm2](https://github.com/main--/rust-lvm2). See `PROVENANCE.md` for the origin story and the full list of changes from upstream.

## What it does

Given any `embedded_io::Read + Seek` source that holds an LVM2 Physical Volume (typically a block device or partition):

1. Parses the PV label (`LABELONE` signature at sector 1) and PV header.
2. Reads the Volume Group metadata text (LVM2 format-text in the metadata area).
3. Enumerates the Logical Volumes in the VG.
4. Returns each LV as an `OpenLV` that itself implements `embedded_io::Read + Seek`, with offsets mapped from LV space to PV space via the segment table.

Existing filesystem crates (ext4 readers, btrfs readers, etc.) plug onto the `OpenLV` byte stream unchanged — they don't know or care whether bytes come from a partition directly or from an LV sitting on top of a partition.

## What it doesn't do

Only linear LVs on a single PV are supported. Striped, mirrored (RAID-1/5/6), thin pool, snapshot, and cache LVs all error with a diagnostic. This covers the canonical layout used by Proxmox VE and most default single-disk LVM installs. See `PROVENANCE.md` for the full coverage matrix and the rationale for the narrow scope.

This is a **reader only.** It never writes to a PV.

## Usage sketch

```rust
use embedded_io::{Read, Seek};
use lamlvm::Lvm2;

// `reader` is anything implementing embedded_io::Read + Seek over the PV's
// bytes — a block device, a partition, or an image file (std callers can
// wrap a File with the embedded-io-adapters crate). Lvm2::open and
// open_lv_by_name each borrow it in turn.
fn read_root_lv<R: Read + Seek>(mut reader: R) -> Result<(), lamlvm::Error> {
    let lvm = Lvm2::open(&mut reader)?;

    // open_lv_by_name returns None when the VG has no LV by that name.
    if let Some(open_lv) = lvm.open_lv_by_name("root", &mut reader) {
        // `open_lv` implements embedded_io::Read + Seek. Hand it to any
        // filesystem reader (e.g. ext4-view) unchanged — it can't tell the
        // bytes come from an LV rather than from a bare partition.
        let _ = open_lv;
    }
    Ok(())
}
```

A worked example walking ext4 on top of a real LV is in `examples/walk_ext4_on_lv.rs`.

## no_std

Set `default-features = false` for full `no_std` operation. `alloc` is required (the metadata-text parse path allocates).

## License

MIT. See `LICENSE-MIT`. Original copyright held by main-- (2022); modifications copyright Lamco Development LLC (2026).
