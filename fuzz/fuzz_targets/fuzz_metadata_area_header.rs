// Copyright 2026 Lamco Development LLC
//
// Licensed under the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT>.

//! Narrow fuzz of `MetadataAreaHeader::parse` — the fixed-magic +
//! variable-length-descriptor block that precedes the VG metadata text.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lamlvm::__fuzzing::MetadataAreaHeader::parse(data);
});
