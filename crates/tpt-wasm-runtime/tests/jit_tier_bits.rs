//! Integer count and rotate operations run natively through `EngineMode::Jit`, checked against Micro over
//! the values that exercise every bit position, zero, all ones, and rotation counts at and past the width.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// An unsigned LEB128 integer.
fn leb(mut value: usize) -> Vec<u8> {
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

/// Append a section with `content`.
fn section(bytes: &mut Vec<u8>, id: u8, content: &[u8]) {
    bytes.push(id);
    bytes.extend(leb(content.len()));
    bytes.extend(content);
}

/// The signatures the module uses: `(i32) -> i32`, `(i32, i32) -> i32`, `(i64) -> i64` and
/// `(i64, i64) -> i64`, as type indices 0 to 3.
const TYPES: [&[u8]; 4] = [
    &[0x60, 0x01, 0x7f, 0x01, 0x7f],
    &[0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f],
    &[0x60, 0x01, 0x7e, 0x01, 0x7e],
    &[0x60, 0x02, 0x7e, 0x7e, 0x01, 0x7e],
];

/// A module that exports each `(name, type index, body)` as a function of that type.
fn module(functions: &[(&str, u8, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let mut types = vec![TYPES.len() as u8];
    for signature in TYPES {
        types.extend(signature);
    }
    section(&mut bytes, 1, &types);
    let mut declared = leb(functions.len());
    for (_, type_index, _) in functions {
        declared.push(*type_index);
    }
    section(&mut bytes, 3, &declared);
    let mut exports = leb(functions.len());
    for (index, (name, _, _)) in functions.iter().enumerate() {
        exports.extend(leb(name.len()));
        exports.extend(name.as_bytes());
        exports.extend([0x00]);
        exports.extend(leb(index));
    }
    section(&mut bytes, 7, &exports);
    let mut code = leb(functions.len());
    for (_, _, body) in functions {
        let mut function = vec![0x00]; // no locals
        function.extend(body);
        code.extend(leb(function.len()));
        code.extend(function);
    }
    section(&mut bytes, 10, &code);
    bytes
}

/// `local.get 0 (; local.get 1); op; end`.
fn body(operation: u8, parameters: u8) -> Vec<u8> {
    let mut code = vec![0x20, 0x00];
    if parameters == 2 {
        code.extend([0x20, 0x01]);
    }
    code.extend([operation, 0x0b]);
    code
}

/// The module: every count and rotate operation, on 32 and 64 bits, as its own export.
fn bits_module() -> Vec<u8> {
    module(&[
        ("clz32", 0, body(0x67, 1)),
        ("ctz32", 0, body(0x68, 1)),
        ("popcnt32", 0, body(0x69, 1)),
        ("rotl32", 1, body(0x77, 2)),
        ("rotr32", 1, body(0x78, 2)),
        ("clz64", 2, body(0x79, 1)),
        ("ctz64", 2, body(0x7a, 1)),
        ("popcnt64", 2, body(0x7b, 1)),
        ("rotl64", 3, body(0x89, 2)),
        ("rotr64", 3, body(0x8a, 2)),
    ])
}

const I32_VALUES: [i32; 16] = [
    0,
    1,
    2,
    3,
    0x7fff_ffff,
    i32::MIN,
    -1,
    0xff,
    0x100,
    0xffff,
    0x1_0000,
    0x1234_5678,
    -123_456_789,
    0x4000_0000,
    0x0000_8000,
    0x0f0f_0f0f,
];

const I32_COUNTS: [i32; 11] = [0, 1, 7, 31, 32, 33, 63, 100, -1, -32, i32::MIN];

const I64_VALUES: [i64; 12] = [
    0,
    1,
    2,
    0x7fff_ffff_ffff_ffff,
    i64::MIN,
    -1,
    0x1_0000_0000,
    0xffff_ffff,
    0x1234_5678_9abc_def0,
    0x0000_0080_0000_0000,
    0x0f0f_0f0f_0f0f_0f0f,
    -987_654_321_012,
];

const I64_COUNTS: [i64; 9] = [0, 1, 31, 32, 63, 64, 65, -1, i64::MIN];

/// The outcome of calling `name` on each argument list, and whether the function ran natively.
fn outcomes(mode: EngineMode, name: &str, calls: &[Vec<Value>]) -> (Vec<String>, bool) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(&bits_module())
        .expect("module instantiates");
    let natively = instance.runs_natively(name);
    let results = calls
        .iter()
        .map(|arguments| format!("{:?}", instance.call(name, arguments.clone())))
        .collect();
    (results, natively)
}

fn calls_for(name: &str) -> Vec<Vec<Value>> {
    let mut calls = Vec::new();
    if name.ends_with("32") {
        for value in I32_VALUES {
            if name.starts_with("rot") {
                for count in I32_COUNTS {
                    calls.push(vec![Value::I32(value), Value::I32(count)]);
                }
            } else {
                calls.push(vec![Value::I32(value)]);
            }
        }
    } else {
        for value in I64_VALUES {
            if name.starts_with("rot") {
                for count in I64_COUNTS {
                    calls.push(vec![Value::I64(value), Value::I64(count)]);
                }
            } else {
                calls.push(vec![Value::I64(value)]);
            }
        }
    }
    calls
}

#[test]
fn count_and_rotate_operations_run_natively_and_agree_with_micro() {
    for name in [
        "clz32", "ctz32", "popcnt32", "rotl32", "rotr32", "clz64", "ctz64", "popcnt64", "rotl64",
        "rotr64",
    ] {
        let calls = calls_for(name);
        let (golden, _) = outcomes(EngineMode::Micro, name, &calls);
        let (native, natively) = outcomes(EngineMode::Jit, name, &calls);
        assert!(natively, "{name} runs as machine code");
        let mismatch = native
            .iter()
            .zip(&golden)
            .zip(&calls)
            .find(|((native, golden), _)| native != golden)
            .map(|((native, golden), call)| format!("{call:?}: native {native}, micro {golden}"));
        assert_eq!(mismatch, None, "{name} disagrees with Micro");
    }
}

#[test]
fn a_zero_input_counts_the_full_width() {
    let (clz, _) = outcomes(EngineMode::Jit, "clz32", &[vec![Value::I32(0)]]);
    assert_eq!(clz, ["Ok([I32(32)])"]);
    let (ctz, _) = outcomes(EngineMode::Jit, "ctz64", &[vec![Value::I64(0)]]);
    assert_eq!(ctz, ["Ok([I64(64)])"]);
    let (popcnt, _) = outcomes(EngineMode::Jit, "popcnt64", &[vec![Value::I64(-1)]]);
    assert_eq!(popcnt, ["Ok([I64(64)])"]);
}
