// Copyright 2026 Lamco Development LLC
//
// Licensed under the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT>.

//! Narrow fuzz of `PhysicalVolumeHeader::parse` — the variable-length
//! header right after the label that lists data + metadata descriptors.
//! Adversarial `many_till` termination + UTF-8 decoding inside this
//! parser have been bug-prone historically in nom-based readers.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lamlvm::__fuzzing::PhysicalVolumeHeader::parse(data);
});
