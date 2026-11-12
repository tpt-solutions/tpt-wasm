//! Calls between native functions through `EngineMode::Jit`, checked against Micro.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::{ResourceLimits, Value};

/// A module with one exported function, `fib`, which calls itself:
/// `fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2)`.
const MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f, // type section: (i32) -> i32
    0x03, 0x02, 0x01, 0x00, // function section
    0x07, 0x07, 0x01, 0x03, 0x66, 0x69, 0x62, 0x00, 0x00, // export "fib"
    0x0a, 0x1e, 0x01, 0x1c, 0x00, // code section: one body of 28 bytes, no locals
    0x20, 0x00, 0x41, 0x02, 0x48, // local.get 0; i32.const 2; i32.lt_s
    0x04, 0x7f, 0x20, 0x00, // if (result i32) local.get 0
    0x05, 0x20, 0x00, 0x41, 0x01, 0x6b, 0x10, 0x00, // else fib(n - 1)
    0x20, 0x00, 0x41, 0x02, 0x6b, 0x10, 0x00, 0x6a, // + fib(n - 2)
    0x0b, 0x0b, // end; end
];

fn run(mode: EngineMode, depth: usize, calls: &[i32]) -> (Vec<String>, bool) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        limits: ResourceLimits {
            max_call_depth: depth,
            ..ResourceLimits::default()
        },
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    let natively = instance.runs_natively("fib");
    let outcomes = calls
        .iter()
        .map(|n| format!("{:?}", instance.call("fib", vec![Value::I32(*n)])))
        .collect();
    (outcomes, natively)
}

#[test]
fn recursive_calls_run_natively_and_agree_with_micro() {
    let calls: Vec<i32> = (0..20).collect();
    let (golden, _) = run(EngineMode::Micro, 1_000, &calls);
    let (native, natively) = run(EngineMode::Jit, 1_000, &calls);
    assert!(natively, "fib runs as machine code");
    assert_eq!(native, golden, "the native tier disagrees with Micro");
    // The range stops at 19, and fib(19) = 4181.
    assert!(native.iter().any(|outcome| outcome == "Ok([I32(4181)])"));
}

#[test]
fn the_call_depth_limit_traps_as_it_does_on_micro() {
    // fib(n) activates n - 1 frames below the entry at its deepest, so with a depth of 4 the
    // fifth frame, reached from fib(5), is the first to trap.
    let calls: Vec<i32> = (0..8).collect();
    let (golden, _) = run(EngineMode::Micro, 4, &calls);
    let (native, natively) = run(EngineMode::Jit, 4, &calls);
    assert!(natively);
    assert_eq!(native, golden, "the depth limit disagrees with Micro");
    assert!(native
        .iter()
        .any(|outcome| outcome.contains("CallDepthExceeded")));
    assert!(native.iter().any(|outcome| outcome == "Ok([I32(2)])"));
}
