# Changelog

All notable changes to lamlvm are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
