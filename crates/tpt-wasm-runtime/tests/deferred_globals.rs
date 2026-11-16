// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A defined global initialized by `global.get` of an imported global takes the imported value, once
//! the import is installed, on both the interpreter and the baseline.

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// A module exporting one immutable i32 global, `g`, with the value 666.
const HOLDER: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x06, 0x07, 0x01, 0x7f, 0x00, 0x41, 0x9a, 0x05, 0x0b, // global: i32 = 666
    0x07, 0x05, 0x01, 0x01, b'g', 0x03, 0x00, // export "g" as global 0
];

/// Imports `env.g` as global 0 and defines global 1 as `global.get 0`, exported as `d`.
const COPIER: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x02, 0x0a, 0x01, 0x03, b'e', b'n', b'v', 0x01, b'g', 0x03, 0x7f, 0x00, // import global
    0x06, 0x06, 0x01, 0x7f, 0x00, 0x23, 0x00, 0x0b, // global 1 = global.get 0
    0x07, 0x05, 0x01, 0x01, b'd', 0x03, 0x01, // export "d"
];

fn copied_value(mode: EngineMode) -> Value {
    let mut engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let holder = engine
        .instantiate_bytes(HOLDER)
        .expect("holder instantiates");
    engine
        .linker_mut()
        .define_export("env", "g", &holder, "g")
        .expect("the holder's global is defined");
    let copier = engine
        .instantiate_bytes(COPIER)
        .expect("copier instantiates");
    copier.global("d").expect("reads the copied global")
}

#[test]
fn a_global_initialized_from_an_import_takes_its_value_on_the_interpreter() {
    assert_eq!(copied_value(EngineMode::Micro), Value::I32(666));
}

#[test]
fn a_global_initialized_from_an_import_takes_its_value_on_the_baseline() {
    assert_eq!(copied_value(EngineMode::Baseline), Value::I32(666));
}

/// Imports `env.g`, has one memory, and writes `hi` at the address `g` holds, which is 666.
const SEGMENT_AT_IMPORT: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x02, 0x0a, 0x01, 0x03, b'e', b'n', b'v', 0x01, b'g', 0x03, 0x7f, 0x00, // import global
    0x05, 0x03, 0x01, 0x00, 0x01, // one memory, one page
    0x07, 0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, // export "memory"
    0x0b, 0x08, 0x01, 0x00, 0x23, 0x00, 0x0b, 0x02, b'h', b'i', // data at global 0
];

fn written_at_import(mode: EngineMode) -> Vec<u8> {
    let mut engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let holder = engine
        .instantiate_bytes(HOLDER)
        .expect("holder instantiates");
    engine
        .linker_mut()
        .define_export("env", "g", &holder, "g")
        .expect("the holder's global is defined");
    let instance = engine
        .instantiate_bytes(SEGMENT_AT_IMPORT)
        .expect("the segment at the imported offset instantiates");
    instance
        .read_memory("memory", 666, 2)
        .expect("reads the segment")
}

#[test]
fn a_data_segment_at_an_imported_offset_is_written_on_the_interpreter() {
    assert_eq!(written_at_import(EngineMode::Micro), b"hi".to_vec());
}

#[test]
fn a_data_segment_at_an_imported_offset_is_written_on_the_baseline() {
    assert_eq!(written_at_import(EngineMode::Baseline), b"hi".to_vec());
}
