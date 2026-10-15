# Changelog

All notable changes to lamlvm are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.3] - 2026-10-15

### Fixed

- **An ignored metadata area was read.** A PV whose metadata area is marked
  ignored (`pvchange --metadataignore y`) keeps the text from before, which
  LVM2 stops updating, so 0.1.2 opened the volume group with stale metadata:
  An LV created afterwards was missing and a resized one had its old size. An
  ignored area is skipped now, and a PV whose only area is ignored is refused.
- **Checksums were never checked.** The PV label, each metadata area header
  and the committed metadata text all carry LVM2's checksum. A damaged label
  or area header is refused, and damaged text that still parsed was read as
  the volume group's layout; now the next metadata area is used.
- **Only the first metadata area was read.** A PV made with
  `--metadatacopies 2` falls back to its second area when the first is
  ignored or fails a check.
- **The label was looked for in sector 1 only.** LVM2 can put it in any of the
  first four sectors (`pvcreate --labelsector`); all four are searched, and a
  label must name its own sector.
- A PV header offset past the end of the label sector panicked; it's a parse
  error now.

### Added

- **RAID1 LVs** read through the leg on the PV opened, where 0.1.2 refused
  them as not linear. A leg LVM is rebuilding, or legs that keep their data
  past their first extent (a reshape), aren't read.
- Fixtures built with the LVM2 tools: Labels in sectors 0 and 3, an ignored
  and stale metadata area, a damaged first metadata area with a good second
  one, and a RAID1 LV with a leg on each PV. Six of the nine fixture tests
  fail on 0.1.2.

## [0.1.2] - 2026-10-15

### Fixed

- A volume group whose committed metadata wraps its ring buffer failed to open
  with `MetadataNotUtf8`. LVM2 writes each new copy of the metadata after the
  last one in a ring that starts right after the 512-byte area header, so a
  long-lived VG (a Proxmox host with many VM disks, say) eventually has its
  copy run past the end and continue at the start. The text is now read in its
  two pieces.
- Only the committed copy of the metadata is read. 0.1.1 joined every location
  descriptor's text, which reads garbage if a precommitted copy is present.
- An LV with no `creation_time` or `creation_host`, which LVM2 leaves out for
  an LV without a timestamp, made the whole VG fail to open. The fields
  default to 0 and an empty string.
- A segment with more than one stripe made the whole VG fail to open, because
  `stripes` was parsed as exactly one PV and extent. It now parses any number;
  `stripes` holds the pair only for a one-stripe segment, so the rest of the
  VG opens and only opening that LV fails with `OpenLvError::Unsupported`.

### Changed

- snafu 0.9, since Debian sid carries only snafu 0.9 and 0.7 kept lamlvm out
  of the archive. Nothing in lamlvm's use of it changes.
- The description names Lamco Development LLC, the README gains website,
  documentation and install links, testing and contributing sections, and CI
  also runs weekly.

### Added

- Oracle tests against images the LVM2 tools made: a wrapped metadata ring, an
  LV without timestamps, and a linear root beside a striped LV and a thin pool.
  The tests need Rust 1.87 for ruzstd 0.9.1; the library's MSRV stays 1.85.

## [0.1.1] — 2026-06-04

### Changed

- Relax the `snafu` dependency from `0.8` to the `0.7` line. lamlvm uses only the
  stable snafu subset (`#[derive(Snafu)]`, `#[snafu(display)]`, context selectors,
  `ensure!`, `OptionExt`), identical across 0.7 and 0.8, so 0.7 is sufficient.
  This matches the version in the Debian archive and avoids forcing a snafu 0.8
  transition there. (rust-lvm2, the fork upstream, also used snafu 0.7.)

## [0.1.0] — 2026-06-02

First published release. lamlvm is a maintained `no_std` fork of
[main--/rust-lvm2](https://github.com/main--/rust-lvm2) v0.0.3 (MIT). See
[`PROVENANCE.md`](PROVENANCE.md) for the full origin and the complete list of
changes from upstream.

### Changed from upstream v0.0.3
- Replaced the unmaintained `acid_io` no_std I/O traits with `embedded-io`,
  the embedded-rust ecosystem standard.
- Migrated to the 2024 edition.
- Replaced `as i64` / `as u64` seek casts with `checked_add_signed`, removing
  silent wraparound on extreme offsets.
- Refreshed dependency pins (`serde`, `nom`, `snafu`, `tracing`) to current
  maintained versions.

### Added
- `OwnedLvReader` — a lifetime-free LV reader that owns its backing reader,
  for callers that cannot thread a borrow (e.g. handing an LV byte stream to a
  filesystem crate that takes ownership).
- `cargo-fuzz` harness suite covering the PV label, PV header, metadata-area
  header, VG metadata text, and full-open parse paths.
- Integration test mounting ext4 (via `ext4-view`) on a linear LV, exercising
  the same code path `lamboot-core` ships.

### Scope (unchanged from upstream)
- Read-only. Linear LVs on a single PV. Striped, mirrored, thin, snapshot, and
  cache LVs, and multi-PV VGs, error with a diagnostic rather than misreading.

[0.1.0]: https://github.com/lamco-admin/lamlvm/releases/tag/v0.1.0
