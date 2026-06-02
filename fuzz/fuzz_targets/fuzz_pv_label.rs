// Copyright 2026 Lamco Development LLC
//
// Licensed under the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT>.

//! Narrow fuzz of `PhysicalVolumeLabelHeader::parse` — the 32-byte
//! label header at sector 1 that opens the PV-parse sequence. Bugs
//! here would surface as parser panics on adversarial firmware-supplied
//! disk bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lamlvm::__fuzzing::PhysicalVolumeLabelHeader::parse(data);
});
