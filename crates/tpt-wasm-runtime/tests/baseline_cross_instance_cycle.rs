// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Two instances that call into each other through one shared table. The baseline runs each instance's
//! functions in a copy of its module, and a copy that is already running is not entered again. The cycle is
//! refused by name rather than deadlocking on the thread that holds the copy.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};

fn header() -> Vec<u8> {
    vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
}

/// `provider` owns a two-slot funcref table, exported as `table`, and fills both slots with `h`. `h` and
/// `call` both call through slot 0. Exported `call` is function 1.
fn provider() -> Vec<u8> {
    let mut module = header();
    module.extend([1, 5, 1, 0x60, 0, 1, 0x7f]); // type 0: () -> i32
    module.extend([3, 3, 2, 0, 0]); // h and call, both type 0
    module.extend([4, 4, 1, 0x70, 0, 2]); // one funcref table, minimum 2
    module.extend([
        7, 0x10, 2, //
        5, b't', b'a', b'b', b'l', b'e', 1, 0, //
        4, b'c', b'a', b'l', b'l', 0, 1,
    ]);
    module.extend([9, 8, 1, 0, 0x41, 0, 0x0b, 2, 0, 0]); // slots 0 and 1 are h
    module.extend([
        10, 17, 2, //
        7, 0x00, 0x41, 0, 0x11, 0, 0, 0x0b, // h: call_indirect slot 0
        7, 0x00, 0x41, 0, 0x11, 0, 0, 0x0b, // call: call_indirect slot 0
    ]);
    module
}

/// `consumer` imports the provider's table and defines `g`, which calls slot 1 through the shared table. Its
/// instantiation overwrites slot 0 with `g`.
fn consumer() -> Vec<u8> {
    let mut module = header();
    module.extend([1, 5, 1, 0x60, 0, 1, 0x7f]); // type 0: () -> i32
    module.extend([
        2, 0x14, 1, //
        8, b'p', b'r', b'o', b'v', b'i', b'd', b'e', b'r', //
        5, b't', b'a', b'b', b'l', b'e', 1, 0x70, 0, 2, // the table, funcref, minimum 2
    ]);
    module.extend([3, 2, 1, 0]); // g, type 0
    module.extend([9, 7, 1, 0, 0x41, 0, 0x0b, 1, 0]); // slot 0 is g
    module.extend([
        10, 9, 1, //
        7, 0x00, 0x41, 1, 0x11, 0, 0, 0x0b, // g: call_indirect slot 1
    ]);
    module
}

#[test]
fn a_cycle_between_instances_is_refused_by_name_on_the_baseline() {
    let mut engine = Engine::new(Config {
        engine_mode: EngineMode::Baseline,
        ..Config::default()
    })
    .expect("engine");
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
    let error: RuntimeError = provider
        .call("call", Vec::new())
        .expect_err("g re-enters the provider, which re-enters g while it is running");
    assert!(
        error.to_string().contains("already running"),
        "the refusal names the re-entry: {error}"
    );
    // The instance still works afterwards, since the refusal left every copy unlocked.
    assert!(provider.call("call", Vec::new()).is_err());
}
