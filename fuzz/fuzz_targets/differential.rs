// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

#![no_main]

use libfuzzer_sys::fuzz_target;

// The oracle is the test; this target only feeds it bytes. A divergence panics, which
// libFuzzer records as a crash and minimizes, so the input that found it is kept.
fuzz_target!(|data: &[u8]| {
    if let Err(divergence) = tpt_wasm_oracle::differential_check(data) {
        panic!("backends diverged: {divergence}");
    }
});
