// Copyright 2026 Lamco Development LLC
//
// Licensed under the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT>.

//! Narrow fuzz of the VG metadata text parse + deserialize path. This
//! is the most complex parser in lamlvm (nom + serde with a custom
//! force-typed map deserializer); arbitrary text in, structured
//! `MetadataRoot` candidate out.
//!
//! Feeds UTF-8 input only — non-UTF-8 bytes fail early at
//! `core::str::from_utf8` in the open path, so fuzzing them through
//! this harness would mostly exercise UTF-8 validation rather than
//! the parser.

#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize as _;

fuzz_target!(|data: &str| {
    let Ok((_trailing, elements)) = lamlvm::__fuzzing::MetadataElements::parse(data) else {
        return;
    };
    let _ = lamlvm::__fuzzing::ForceDeTypedMap::<String, lamlvm::__fuzzing::MetadataRoot>::deserialize(
        &elements,
    );
});
