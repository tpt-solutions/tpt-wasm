// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A funcref table shared between two instances. A table entry is a store address, so a call through the
//! table reaches the function that was written into it. The baseline can run a function of its own module,
//! but not one of another instance, so that call is refused by name rather than run wrongly.

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

fn header() -> Vec<u8> {
    vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
}

/// `provider` owns a one-slot funcref table, exported as `table`, and fills slot 0 with its function `f`,
/// which returns 7. Its export `call` calls through slot 0.
fn provider() -> Vec<u8> {
    let mut module = header();
    module.extend([1, 5, 1, 0x60, 0, 1, 0x7f]); // type 0: () -> i32
    module.extend([3, 3, 2, 0, 0]); // f and call, both type 0
    module.extend([4, 4, 1, 0x70, 0, 1]); // one funcref table, minimum 1
    module.extend([
        7, 0x10, 2, // two exports
        5, b't', b'a', b'b', b'l', b'e', 1, 0, //
        4, b'c', b'a', b'l', b'l', 0, 1,
    ]);
    module.extend([9, 7, 1, 0, 0x41, 0, 0x0b, 1, 0]); // active segment: slot 0 is function 0 (f)
    module.extend([
        10, 14, 2, //
        4, 0x00, 0x41, 7, 0x0b, // f returns 7
        7, 0x00, 0x41, 0, 0x11, 0, 0,
        0x0b, // call: call_indirect type 0 through table 0, slot 0
    ]);
    module
}

/// `consumer` imports the provider's table, defines `g` which returns 9, writes `g` into slot 0 of the shared
/// table, and exports `call`, which calls through slot 0.
fn consumer() -> Vec<u8> {
    let mut module = header();
    module.extend([1, 5, 1, 0x60, 0, 1, 0x7f]); // type 0: () -> i32
    module.extend([
        2, 0x14, 1, // one import
        8, b'p', b'r', b'o', b'v', b'i', b'd', b'e', b'r', //
        5, b't', b'a', b'b', b'l', b'e', 1, 0x70, 0, 1, // the table, funcref, minimum 1
    ]);
    module.extend([3, 3, 2, 0, 0]); // g and call, both type 0
    module.extend([7, 8, 1, 4, b'c', b'a', b'l', b'l', 0, 1]); // export call = function 1
    module.extend([9, 7, 1, 0, 0x41, 0, 0x0b, 1, 0]); // active segment: slot 0 is function 0 (g)
    module.extend([
        10, 14, 2, //
        4, 0x00, 0x41, 9, 0x0b, // g returns 9
        7, 0x00, 0x41, 0, 0x11, 0, 0,
        0x0b, // call: call_indirect type 0 through table 0, slot 0
    ]);
    module
}

fn engine(mode: EngineMode) -> Engine {
    Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine")
}

#[test]
fn a_call_through_a_table_to_a_function_of_the_same_module_runs_on_both_engines() {
    for mode in [EngineMode::Micro, EngineMode::Baseline] {
        let mut engine = engine(mode);
        let provider = engine
            .instantiate_bytes(&provider())
            .unwrap_or_else(|error| panic!("{mode:?} provider: {error}"));
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .expect("defines the provider");
        let mut consumer = engine
            .instantiate_bytes(&consumer())
            .unwrap_or_else(|error| panic!("{mode:?} consumer: {error}"));
        assert_eq!(
            consumer.call("call", Vec::new()).expect("runs"),
            vec![Value::I32(9)],
            "{mode:?}: the consumer's own table entry is its own function"
        );
    }
}

#[test]
fn the_interpreter_runs_a_function_of_another_instance_reached_through_a_shared_table() {
    let mut engine = engine(EngineMode::Micro);
    let mut provider = engine
        .instantiate_bytes(&provider())
        .expect("provider instantiates");
    engine
        .linker_mut()
        .define_instance("provider", &provider)
        .expect("defines the provider");
    engine
        .instantiate_bytes(&consumer())
        .expect("consumer instantiates over the shared table");
    assert_eq!(
        provider.call("call", Vec::new()).expect("runs"),
        vec![Value::I32(9)],
        "the consumer overwrote slot 0, so the provider's call reaches the consumer's g"
    );
}

#[test]
fn the_baseline_reaches_another_instances_function_through_the_shared_table() {
    let mut engine = engine(EngineMode::Baseline);
    let mut provider = engine
        .instantiate_bytes(&provider())
        .expect("provider instantiates");
    engine
        .linker_mut()
        .define_instance("provider", &provider)
        .expect("defines the provider");
    engine
        .instantiate_bytes(&consumer())
        .expect("consumer instantiates over the shared table");
    assert_eq!(
        provider.call("call", Vec::new()).expect("runs"),
        vec![Value::I32(9)],
        "the consumer's g runs from the provider's call, as on the interpreter"
    );
}
