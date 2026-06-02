# lamlvm — Provenance

This crate is a vendored + modernized fork of the upstream `lvm2` crate.

## Upstream

- **Project:** rust-lvm2
- **Repository:** https://github.com/main--/rust-lvm2
- **License:** MIT
- **Original author:** main-- (GitHub user 754850)
- **Vendored commit:** `424d5e3cae7d68b641c2b29e610fd2dc5004b2a5` (v0.0.3, 2022-08-16)
- **State at vendoring:** dormant — no commits since 2022-08-16; 0 open issues; 2 PRs merged historically

The original Cargo.toml is preserved as `Cargo.toml.upstream` for reference.

## Why we vendored

A survey of the Rust LVM ecosystem found this was the right base to build on:

- Only viable Rust no_std-capable LVM2 read implementation
- MIT-licensed (compatible with LamBoot's MIT-OR-Apache-2.0 dual)
- Demonstrably correct: passing integration test that mounts ext4 on an LV
- Handles the canonical Proxmox case (linear LV on single PV)
- Upstream is dormant — vendoring lets us maintain at our own cadence
  rather than depending on an unmaintained crates.io release

## Modernization changes from upstream

This fork makes the following changes from the v0.0.3 baseline:

| Area | Change | Rationale |
|---|---|---|
| no_std I/O traits | `acid_io` → `embedded-io` | acid_io is also dormant since 2022; embedded-io is the maintained embedded-rust ecosystem standard |
| Edition | 2021 → 2024 | Current Rust edition; trivial migration |
| Seek arithmetic | `as i64` / `as u64` casts → `checked_add_signed` | Prevents silent wrap on edge cases (LVs >2^63 bytes; underflow on negative SeekFrom::Current) |
| Dependency pins | `serde 1.0.142` / `snafu 0.7.1` / `tracing 0.1.36` → current patch levels | Patch-level updates; no API changes |
| Tests | one integration test → integration + fuzz harness (cargo-fuzz) targeting PV header + VG metadata text parsers | Format parsers on untrusted disk bytes need fuzzing |
| Build hygiene | Add LICENSE-MIT file, PROVENANCE.md, README.md | upstream had none |

The fundamental parser logic and on-disk format handling are unchanged.

## What we did NOT change

- **The on-disk format parser**: LVM2 format has been stable since ~2003; no changes from upstream needed.
- **nom 7 → nom 8 migration**: deferred. nom 7.1 line is still maintained for security fixes; migration is a significant rewrite (closures → trait API). Will tackle when nom 7.1 stops getting security updates or when we need a nom 8 feature.
- **Coverage scope**: still only linear LV on single PV. Striped/RAID/thin/snapshot LVs error cleanly with diagnostic. Proxmox's `pve-root` is always a linear single-PV LV.
- **Multi-VG-per-PV**: still errors. Uncommon in practice; not a Proxmox concern.

## Maintenance policy

- We own this fork. Bug fixes + dependency updates land here, not upstream.
- Published to crates.io as `lamlvm` — a maintained `no_std` fork, distinct in
  name from the upstream `lvm2` crate so the two never collide.
- We do NOT track upstream commits (upstream is dormant; no commits to track).

## Coverage matrix (what works, what doesn't)

| LV type | Status | Notes |
|---|---|---|
| Linear LV on single PV | ✓ supported | Proxmox `pve-root` is always this |
| Multi-segment linear (extended LV) | ✓ supported | seek is O(n_segments) — acceptable |
| Striped (RAID-0) | ✗ errors with diagnostic | uncommon on hosts |
| Mirror (RAID-1/5/6) | ✗ errors with diagnostic | Proxmox uses ZFS for HA-root, not LVM RAID |
| Thin pool / thin LV | ✗ errors with diagnostic | Proxmox `pve-data` is thin but we don't read kernels from there |
| Snapshot LV | ✗ errors with diagnostic | not a Proxmox boot concern |
| Cache LV (dm-cache) | ✗ errors with diagnostic | uncommon |
| Multi-PV VG (segments reference other PVs) | ✗ errors with diagnostic | rare on Proxmox hosts |
