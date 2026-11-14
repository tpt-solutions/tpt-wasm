// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! `table.get` and `table.set` on Micro, through the public engine.
//!
//! The module has one `funcref` table of two elements and three functions: `f`, which
//! does nothing, `set(i)`, which stores `ref.func f` at index `i`, and `probe(i)`, which
//! returns 1 when the element at `i` is null. The baseline does not yet compile these
//! instructions, so a module that uses them is refused there rather than run partly.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::Value;

/// A LEB128 unsigned integer, for the sizes and counts a section header holds.
fn leb(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// A section: its id, then its size, then its contents.
fn section(id: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(leb(contents.len() as u32));
    out.extend_from_slice(contents);
    out
}

/// A function body: no locals, the given code, and the terminating `end`.
fn body(code: &[u8]) -> Vec<u8> {
    let mut inner = vec![0x00];
    inner.extend_from_slice(code);
    inner.push(0x0b);
    let mut out = leb(inner.len() as u32);
    out.extend(inner);
    out
}

/// The module, with `set` and `probe` given their code. `f` is always empty.
///
/// The types are `() -> ()` (index 0, for `f`) and `(i32) -> i32` (index 1, for `set` and
/// `probe`). The table is `funcref` with two elements and no maximum.
fn module(set_code: &[u8], probe_code: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // Types: () -> (), and (i32) -> i32.
    bytes.extend(section(
        0x01,
        &[0x02, 0x60, 0x00, 0x00, 0x60, 0x01, 0x7f, 0x01, 0x7f],
    ));
    // Functions: f has type 0, set and probe have type 1.
    bytes.extend(section(0x03, &[0x03, 0x00, 0x01, 0x01]));
    // Table: one funcref table, minimum 2, no maximum.
    bytes.extend(section(0x04, &[0x01, 0x70, 0x00, 0x02]));
    // Exports: "set" is function 1 and "probe" is function 2.
    bytes.extend(section(
        0x07,
        &[
            0x02, //
            0x03, b's', b'e', b't', 0x00, 0x01, //
            0x05, b'p', b'r', b'o', b'b', b'e', 0x00, 0x02,
        ],
    ));
    // Code: f, set, and probe.
    let mut code = vec![0x03];
    code.extend(body(&[]));
    code.extend(body(set_code));
    code.extend(body(probe_code));
    bytes.extend(section(0x0a, &code));
    bytes
}

/// `set(i)`: store `ref.func 0` at index `i`, then return 1.
const SET: &[u8] = &[
    0x20, 0x00, // local.get 0: the index
    0xd2, 0x00, // ref.func 0: the reference
    0x26, 0x00, // table.set 0
    0x41, 0x01, // i32.const 1
];

/// `probe(i)`: return 1 when the element at `i` is null, as `ref.is_null` reports.
const PROBE: &[u8] = &[
    0x20, 0x00, // local.get 0
    0x25, 0x00, // table.get 0
    0xd1, // ref.is_null
];

fn instantiate(mode: EngineMode, bytes: &[u8]) -> Result<tpt_wasm_runtime::Instance, RuntimeError> {
    Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine")
    .instantiate_bytes(bytes)
}

#[test]
fn an_empty_slot_reads_as_null_and_a_stored_reference_reads_back() {
    let mut instance =
        instantiate(EngineMode::Micro, &module(SET, PROBE)).expect("the module is valid");
    let probe = |instance: &mut tpt_wasm_runtime::Instance, index: i32| {
        instance
            .call("probe", vec![Value::I32(index)])
            .expect("probe runs")
    };
    assert_eq!(
        probe(&mut instance, 1),
        vec![Value::I32(1)],
        "slot 1 starts null"
    );
    assert_eq!(
        instance.call("set", vec![Value::I32(1)]).expect("set runs"),
        vec![Value::I32(1)]
    );
    assert_eq!(
        probe(&mut instance, 1),
        vec![Value::I32(0)],
        "slot 1 now holds f"
    );
    assert_eq!(
        probe(&mut instance, 0),
        vec![Value::I32(1)],
        "slot 0 is untouched"
    );
}

#[test]
fn an_index_past_the_table_traps_on_get_and_on_set() {
    let mut instance =
        instantiate(EngineMode::Micro, &module(SET, PROBE)).expect("the module is valid");
    assert!(
        instance.call("probe", vec![Value::I32(2)]).is_err(),
        "table.get past the end traps"
    );
    assert!(
        instance.call("set", vec![Value::I32(2)]).is_err(),
        "table.set past the end traps"
    );
}

#[test]
fn a_reference_of_the_wrong_kind_is_refused_by_validation() {
    // set stores an i32 where the table needs a funcref.
    let wrong_value: &[u8] = &[0x20, 0x00, 0x41, 0x00, 0x26, 0x00, 0x41, 0x01];
    assert!(
        instantiate(EngineMode::Micro, &module(wrong_value, PROBE)).is_err(),
        "a table.set of an i32 does not validate"
    );
}

#[test]
fn an_index_into_a_table_that_does_not_exist_is_refused_by_validation() {
    let no_such_table: &[u8] = &[0x20, 0x00, 0x25, 0x01];
    assert!(
        instantiate(EngineMode::Micro, &module(SET, no_such_table)).is_err(),
        "table 1 does not exist"
    );
}

#[test]
fn every_engine_agrees_with_micro_on_table_reads_writes_and_traps() {
    // Each call's outcome is recorded as text, so a value and a trap compare the same way.
    let calls: [(&str, i32); 7] = [
        ("probe", 1),
        ("set", 1),
        ("probe", 1),
        ("probe", 0),
        ("probe", 2),
        ("set", 2),
        ("set", 0),
    ];
    let outcomes = |mode: EngineMode| -> Vec<String> {
        let mut instance = instantiate(mode, &module(SET, PROBE)).expect("the module is valid");
        calls
            .iter()
            .map(|(name, index)| format!("{:?}", instance.call(name, vec![Value::I32(*index)])))
            .collect()
    };
    let expected = outcomes(EngineMode::Micro);
    assert!(
        expected[4].contains("Err"),
        "probing past the table traps on Micro: {expected:?}"
    );
    for mode in [
        EngineMode::Baseline,
        EngineMode::Jit,
        EngineMode::Optimizing,
    ] {
        assert_eq!(outcomes(mode), expected, "{mode:?} agrees with Micro");
    }
}

#[test]
fn a_table_entry_set_on_the_baseline_is_read_back_on_the_baseline() {
    let mut instance =
        instantiate(EngineMode::Baseline, &module(SET, PROBE)).expect("the module is valid");
    assert_eq!(
        instance
            .call("probe", vec![Value::I32(1)])
            .expect("probe runs"),
        vec![Value::I32(1)],
        "slot 1 starts null"
    );
    instance.call("set", vec![Value::I32(1)]).expect("set runs");
    assert_eq!(
        instance
            .call("probe", vec![Value::I32(1)])
            .expect("probe runs"),
        vec![Value::I32(0)],
        "the write is visible to the next read on the same instance"
    );
}
