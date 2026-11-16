// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! An `externref` table on the interpreter. The host passes extern values in and reads them back out
//! through `table.set` and `table.get`, a null extern is the default slot value, and an index past the
//! table traps. The baseline does not yet lower an `externref` table, so it refuses the module by name.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{RefValue, ReferenceType, Trap, Value};

/// A module with one `externref` table of two slots, exported as `table`, and two functions:
/// `set(i32, externref)` stores into the table, and `get(i32) -> externref` loads from it.
fn externref_table_module() -> Vec<u8> {
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // Types: 0 is (i32, externref) -> (), 1 is (i32) -> externref.
    module.extend([
        0x01, 0x0b, 2, 0x60, 2, 0x7f, 0x6f, 0, 0x60, 1, 0x7f, 1, 0x6f,
    ]);
    // Functions: set is type 0, get is type 1.
    module.extend([0x03, 3, 2, 0, 1]);
    // One table of externref, minimum 2, no maximum.
    module.extend([0x04, 4, 1, 0x6f, 0, 2]);
    // Exports: set, get, and the table.
    module.extend([
        0x07, 21, 3, //
        3, b's', b'e', b't', 0x00, 0, //
        3, b'g', b'e', b't', 0x00, 1, //
        5, b't', b'a', b'b', b'l', b'e', 0x01, 0,
    ]);
    // Bodies: set is local.get 0; local.get 1; table.set 0. get is local.get 0; table.get 0.
    module.extend([
        0x0a, 0x11, 2, //
        8, 0x00, 0x20, 0x00, 0x20, 0x01, 0x26, 0x00, 0x0b, //
        6, 0x00, 0x20, 0x00, 0x25, 0x00, 0x0b,
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
fn an_externref_table_holds_extern_values_and_starts_null() {
    let mut instance = engine(EngineMode::Micro)
        .instantiate_bytes(&externref_table_module())
        .expect("instantiates");
    assert_eq!(
        instance.call("get", vec![Value::I32(0)]).expect("reads"),
        vec![Value::Ref(RefValue::Null(ReferenceType::ExternRef))],
        "an unset slot is a null externref"
    );
    instance
        .call(
            "set",
            vec![Value::I32(1), Value::Ref(RefValue::ExternRef(42))],
        )
        .expect("stores an extern value");
    assert_eq!(
        instance.call("get", vec![Value::I32(1)]).expect("reads"),
        vec![Value::Ref(RefValue::ExternRef(42))],
        "the stored extern value comes back unchanged"
    );
}

#[test]
fn an_externref_table_refuses_a_funcref_value_and_traps_past_its_end() {
    let mut instance = engine(EngineMode::Micro)
        .instantiate_bytes(&externref_table_module())
        .expect("instantiates");
    assert!(
        instance
            .call("set", vec![Value::I32(0), Value::Ref(RefValue::FuncRef(0))])
            .is_err(),
        "a funcref is not an externref, so the store is refused"
    );
    let error: RuntimeError = instance
        .call("get", vec![Value::I32(2)])
        .expect_err("slot 2 is past a two-slot table");
    assert!(
        matches!(error, RuntimeError::Trap(Trap::TableOutOfBounds)),
        "the access traps as out of bounds: {error:?}"
    );
}

#[test]
fn the_baseline_refuses_an_externref_table_by_name() {
    let error = engine(EngineMode::Baseline)
        .instantiate_bytes(&externref_table_module())
        .expect_err("the baseline does not lower an externref table yet");
    match error {
        RuntimeError::Refused(message) => assert!(
            message.contains("externref table"),
            "the refusal names the table kind: {message}"
        ),
        other => panic!("expected a refusal naming the table, got {other:?}"),
    }
}
