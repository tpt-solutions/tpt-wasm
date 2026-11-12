//! The native tier with float signatures, checked against Micro.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// A module with two exports:
///
/// - `add(a, b)`: `f64` in, `f64` out, `f64.add`.
/// - `lt(a, b)`: `f32` in, `i32` out, `f32.lt`.
const MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x0d, 0x02, // type section: two types
    0x60, 0x02, 0x7c, 0x7c, 0x01, 0x7c, // (f64, f64) -> f64
    0x60, 0x02, 0x7d, 0x7d, 0x01, 0x7f, // (f32, f32) -> i32
    0x03, 0x03, 0x02, 0x00, 0x01, // function section
    0x07, 0x0c, 0x02, // export section: two entries
    0x03, 0x61, 0x64, 0x64, 0x00, 0x00, // "add"
    0x02, 0x6c, 0x74, 0x00, 0x01, // "lt"
    0x0a, 0x11, 0x02, // code section: two bodies
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0xa0, 0x0b, // add: f64.add
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x5d, 0x0b, // lt: f32.lt
];

/// The calls, with edge operands: signed zeros, infinities, a NaN, and a subnormal.
fn calls() -> Vec<(&'static str, Vec<Value>)> {
    let f64_edges: [u64; 7] = [
        0.0f64.to_bits(),
        (-0.0f64).to_bits(),
        1.5f64.to_bits(),
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        f64::NAN.to_bits(),
        1u64, // the smallest subnormal
    ];
    let f32_edges: [u32; 6] = [
        0.0f32.to_bits(),
        (-0.0f32).to_bits(),
        (-2.5f32).to_bits(),
        f32::INFINITY.to_bits(),
        f32::NAN.to_bits(),
        1.0f32.to_bits(),
    ];
    let mut out = Vec::new();
    for a in f64_edges {
        for b in f64_edges {
            out.push(("add", vec![Value::F64(a), Value::F64(b)]));
        }
    }
    for a in f32_edges {
        for b in f32_edges {
            out.push(("lt", vec![Value::F32(a), Value::F32(b)]));
        }
    }
    out
}

/// A result as text, with every NaN written the same way: payloads are not part of the answer.
fn describe(outcome: &Result<Vec<Value>, tpt_wasm_runtime::RuntimeError>) -> String {
    match outcome {
        Ok(values) => values
            .iter()
            .map(|value| format!("{value:?}"))
            .collect::<Vec<_>>()
            .join(","),
        Err(error) => format!("error {error:?}"),
    }
}

fn outcomes(mode: EngineMode) -> (Vec<String>, Vec<bool>) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    let natively: Vec<bool> = ["add", "lt"]
        .iter()
        .map(|name| instance.runs_natively(name))
        .collect();
    let results = calls()
        .into_iter()
        .map(|(name, args)| describe(&instance.call(name, args)))
        .collect();
    (results, natively)
}

#[test]
fn float_results_match_micro_and_run_natively() {
    let (golden, _) = outcomes(EngineMode::Micro);
    let (native, runs_natively) = outcomes(EngineMode::Jit);
    assert_eq!(native, golden, "the native tier disagrees with Micro");
    assert_eq!(
        runs_natively,
        vec![true, true],
        "both functions are compiled"
    );
    // The NaN and the infinity cases must be present, or the comparison is weaker than it looks.
    // inf - inf and the NaN inputs give the canonical NaN, 0x7ff8_0000_0000_0000.
    assert!(native
        .iter()
        .any(|outcome| outcome.contains("F64(9221120237041090560)")));
    assert!(native.iter().any(|outcome| outcome == "I32(1)"));
    assert!(native.iter().any(|outcome| outcome == "I32(0)"));
}
