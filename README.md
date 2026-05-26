# lamlvm

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
use lamlvm::Lvm2;

let mut reader = /* embedded_io::Read + Seek over the PV's bytes */;
let lvm = Lvm2::open(&mut reader)?;

// Find the LV by name (e.g., "root" on a typical Proxmox install)
let open_lv = lvm.open_lv_by_name("root", &mut reader)
    .ok_or("no LV named 'root' in this VG")?;

// open_lv implements embedded_io::Read + Seek; plug it into your FS crate
let fs = my_ext4_crate::SuperBlock::new(adapter(open_lv))?;
```

A worked example walking ext4 on top of a real LV is in `examples/walk_ext4_on_lv.rs`.

## no_std

Set `default-features = false` for full `no_std` operation. `alloc` is required (the metadata-text parse path allocates).

## License

MIT. See `LICENSE-MIT`. Original copyright held by main-- (2022); modifications copyright Lamco Development (2026).
