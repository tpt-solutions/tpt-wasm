// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A module with two tables validates, and an indirect call through the second one reaches it, on the
//! interpreter and on the baseline alike.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};

/// `f() -> i32` is `i32.const 0; call_indirect (type 0) table 1`. Table 0 and table 1 are both `funcref`
/// tables of one element, so the call through table 1 finds an empty slot and traps.
fn two_tables() -> Vec<u8> {
    vec![
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
        0x03, 0x02, 0x01, 0x00, // one function of type 0
        0x04, 0x07, 0x02, 0x70, 0x00, 0x01, 0x70, 0x00,
        0x01, // two tables of one funcref element
        0x07, 0x05, 0x01, 0x01, b'f', 0x00, 0x00, // export "f"
        0x0a, 0x09, 0x01, 0x07, 0x00, 0x41, 0x00, 0x11, 0x00, 0x01,
        0x0b, // call_indirect table 1
    ]
}

#[test]
fn an_indirect_call_through_the_second_table_traps_on_the_interpreter() {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(&two_tables())
        .expect("a module with two tables validates and instantiates");
    let error = instance
        .call("f", Vec::new())
        .expect_err("the second table's only slot is empty");
    assert!(
        matches!(error, RuntimeError::Trap(_)),
        "the call reaches the second table and traps there, not earlier: {error:?}"
    );
}

#[test]
fn the_baseline_traps_on_the_second_table_as_the_interpreter_does() {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Baseline,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(&two_tables())
        .expect("the baseline holds both tables");
    let error = instance
        .call("f", Vec::new())
        .expect_err("the second table's only slot is empty");
    assert!(
        matches!(error, RuntimeError::Trap(_)),
        "the call reaches the second table and traps there: {error:?}"
    );
}
