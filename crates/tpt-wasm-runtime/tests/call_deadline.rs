// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) TPT Solutions

//! A wall-clock deadline on a call: a guest that never returns traps once the deadline passes, and
//! a deadline is refused on a tier that does not enforce it.

use std::time::{Duration, Instant};

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::Trap;

/// A module that exports `spin`, a function whose loop never ends.
fn spin() -> Vec<u8> {
    vec![
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic and version
        0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type: () -> ()
        0x03, 0x02, 0x01, 0x00, // function 0 has type 0
        0x07, 0x08, 0x01, 0x04, b's', b'p', b'i', b'n', 0x00, 0x00, // export "spin"
        0x0a, 0x09, 0x01, 0x07, 0x00, // code: one body of seven bytes, no locals
        0x03, 0x40, // loop
        0x0c, 0x00, // br 0: back to the loop
        0x0b, 0x0b, // end of loop, end of function
    ]
}

#[test]
fn a_call_that_never_returns_traps_at_its_deadline() {
    let config = Config {
        max_call_time: Some(Duration::from_millis(50)),
        ..Config::default()
    };
    let engine = Engine::new(config).expect("engine builds");
    let mut instance = engine.instantiate_bytes(&spin()).expect("instantiates");
    let started = Instant::now();
    let result = instance.call("spin", Vec::new());
    assert!(
        matches!(result, Err(RuntimeError::Trap(Trap::DeadlineExceeded))),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the deadline is not enforced promptly"
    );
}

#[test]
fn a_deadline_is_refused_on_a_tier_that_does_not_enforce_it() {
    let config = Config {
        engine_mode: EngineMode::Baseline,
        max_call_time: Some(Duration::from_millis(50)),
        ..Config::default()
    };
    assert!(matches!(
        Engine::new(config),
        Err(RuntimeError::UnsupportedFeature(_))
    ));
}
