// Copyright 2026 Lamco Development
//
// Licensed under the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT>.

//! End-to-end fuzz of the PV-open path: arbitrary bytes go in, lamlvm
//! tries to parse them as an LVM2 Physical Volume. The harness covers
//! every parser in the open sequence (PV label header → PV header →
//! metadata area header → VG metadata text → serde deserialization)
//! plus the byte-stream I/O glue.
//!
//! Mirrors lambutter-fuzz's `fuzz_superblock` end-to-end pattern.

#![no_main]

use std::io::Cursor;

use embedded_io_adapters::std::FromStd;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Need at least sector 0 + sector 1 (label header lives at byte 512).
    if data.len() < 1024 {
        return;
    }
    let mut reader = FromStd::new(Cursor::new(data));
    let _ = lamlvm::Lvm2::open(&mut reader);
});
