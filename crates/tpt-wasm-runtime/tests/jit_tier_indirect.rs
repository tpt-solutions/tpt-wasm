//! Indirect calls through a private table, run natively under `EngineMode::Jit`, checked against
//! Micro, including every trap a call through a table can raise.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// A module whose `dispatch(x, y, sel)` calls the table's slot `sel` with `(x, y)`. The table holds
/// four slots: `add` and `mul` (both `(i32, i32) -> i32`), a null, and `neg` (`(i32) -> i32`), whose
/// type does not match the call.
const MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x13, 0x03, // type section: three types
    0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // T0: (i32, i32) -> i32
    0x60, 0x01, 0x7f, 0x01, 0x7f, // T1: (i32) -> i32
    0x60, 0x03, 0x7f, 0x7f, 0x7f, 0x01, 0x7f, // T2: (i32, i32, i32) -> i32
    0x03, 0x05, 0x04, 0x00, 0x00, 0x01, 0x02, // function section: add, mul, neg, dispatch
    0x04, 0x04, 0x01, 0x70, 0x00, 0x04, // table section: one funcref table of four slots
    0x07, 0x0c, 0x01, 0x08, 0x64, 0x69, 0x73, 0x70, 0x61, 0x74, 0x63, 0x68, 0x00,
    0x03, // export
    0x09, 0x12, 0x01, // element section: one active segment at slot 0
    0x04, 0x41, 0x00, 0x0b, 0x04, // flags 4, offset 0, four elements
    0xd2, 0x00, 0x0b, // ref.func 0 (add)
    0xd2, 0x01, 0x0b, // ref.func 1 (mul)
    0xd0, 0x70, 0x0b, // ref.null funcref
    0xd2, 0x02, 0x0b, // ref.func 2 (neg)
    0x0a, 0x25, 0x04, // code section: four bodies
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b, // add: x + y
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6c, 0x0b, // mul: x * y
    0x07, 0x00, 0x41, 0x00, 0x20, 0x00, 0x6b, 0x0b, // neg: 0 - x
    0x0b, 0x00, 0x20, 0x00, 0x20, 0x01, 0x20, 0x02, 0x11, 0x00, 0x00, 0x0b, // dispatch
];

/// The same module with its table exported, which lets another instance write the table, so the
/// native tier must not compile the indirect call.
const EXPORTED_TABLE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x13, 0x03, // type section
    0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, //
    0x60, 0x01, 0x7f, 0x01, 0x7f, //
    0x60, 0x03, 0x7f, 0x7f, 0x7f, 0x01, 0x7f, //
    0x03, 0x05, 0x04, 0x00, 0x00, 0x01, 0x02, // function section
    0x04, 0x04, 0x01, 0x70, 0x00, 0x04, // table section
    0x07, 0x12, 0x02, // export section: dispatch and the table
    0x08, 0x64, 0x69, 0x73, 0x70, 0x61, 0x74, 0x63, 0x68, 0x00, 0x03, //
    0x03, 0x74, 0x61, 0x62, 0x01, 0x00, // "tab": the table
    0x09, 0x12, 0x01, // element section
    0x04, 0x41, 0x00, 0x0b, 0x04, //
    0xd2, 0x00, 0x0b, //
    0xd2, 0x01, 0x0b, //
    0xd0, 0x70, 0x0b, //
    0xd2, 0x02, 0x0b, //
    0x0a, 0x25, 0x04, // code section
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b, //
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6c, 0x0b, //
    0x07, 0x00, 0x41, 0x00, 0x20, 0x00, 0x6b, 0x0b, //
    0x0b, 0x00, 0x20, 0x00, 0x20, 0x01, 0x20, 0x02, 0x11, 0x00, 0x00, 0x0b, //
];

/// The calls: each `sel` against a few operand pairs, including the selectors that trap.
fn calls() -> Vec<(i32, i32, i32)> {
    let mut out = Vec::new();
    for sel in [-1, 0, 1, 2, 3, 4, 5, 100, i32::MIN] {
        for (x, y) in [(7, 3), (-4, 5), (0, 0)] {
            out.push((x, y, sel));
        }
    }
    out
}

fn run(mode: EngineMode, bytes: &[u8]) -> (Vec<String>, bool) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(bytes)
        .expect("module instantiates");
    let natively = instance.runs_natively("dispatch");
    let outcomes = calls()
        .into_iter()
        .map(|(x, y, sel)| {
            let args = vec![Value::I32(x), Value::I32(y), Value::I32(sel)];
            format!("{:?}", instance.call("dispatch", args))
        })
        .collect();
    (outcomes, natively)
}

#[test]
fn indirect_calls_run_natively_and_agree_with_micro() {
    let (golden, _) = run(EngineMode::Micro, MODULE);
    let (native, natively) = run(EngineMode::Jit, MODULE);
    assert!(natively, "dispatch runs as machine code");
    assert_eq!(native, golden, "the native tier disagrees with Micro");
    // Every outcome the table can produce has to occur, or the comparison is weaker than it looks.
    assert!(
        native.iter().any(|outcome| outcome == "Ok([I32(10)])"),
        "add"
    );
    assert!(
        native.iter().any(|outcome| outcome == "Ok([I32(21)])"),
        "mul"
    );
    assert!(
        native
            .iter()
            .any(|outcome| outcome.contains("NullReference")),
        "null"
    );
    assert!(
        native
            .iter()
            .any(|outcome| outcome.contains("IndirectCallTypeMismatch")),
        "mismatch"
    );
    assert!(
        native
            .iter()
            .any(|outcome| outcome.contains("TableOutOfBounds")),
        "out of range"
    );
}

#[test]
fn an_exported_table_is_left_to_the_baseline() {
    let (golden, _) = run(EngineMode::Micro, EXPORTED_TABLE);
    let (native, natively) = run(EngineMode::Jit, EXPORTED_TABLE);
    assert!(!natively, "another instance could write this table");
    assert_eq!(native, golden, "the baseline still agrees with Micro");
}
