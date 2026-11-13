//! The native tier of `EngineMode::Jit`, checked against Micro and the baseline.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::{ResourceLimits, Value};

/// A module with four exports, all over `i32`:
///
/// - `add(a, b)` and `div(a, b)` take integers and return one, so the JIT compiles them.
/// - `load(addr)` reads memory 0, so it is compiled too.
/// - `store(addr, value)` returns nothing. The compiler refuses a function with no result,
///   so `store` runs on the baseline, and a native `load` must see what it wrote.
const MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x11, 0x03, // type section: three types
    0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // (i32, i32) -> i32
    0x60, 0x01, 0x7f, 0x01, 0x7f, // (i32) -> i32
    0x60, 0x02, 0x7f, 0x7f, 0x00, // (i32, i32) -> ()
    0x03, 0x05, 0x04, 0x00, 0x00, 0x01, 0x02, // function section
    0x05, 0x03, 0x01, 0x00, 0x01, // memory section: one page, no maximum
    0x07, 0x1c, 0x04, // export section: four entries
    0x03, 0x61, 0x64, 0x64, 0x00, 0x00, // "add"
    0x03, 0x64, 0x69, 0x76, 0x00, 0x01, // "div"
    0x04, 0x6c, 0x6f, 0x61, 0x64, 0x00, 0x02, // "load"
    0x05, 0x73, 0x74, 0x6f, 0x72, 0x65, 0x00, 0x03, // "store"
    0x0a, 0x23, 0x04, // code section: four bodies
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b, // add: local.get 0; local.get 1; i32.add
    0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6d, 0x0b, // div: ...; i32.div_s
    0x07, 0x00, 0x20, 0x00, 0x28, 0x02, 0x00, 0x0b, // load: local.get 0; i32.load
    0x09, 0x00, 0x20, 0x00, 0x20, 0x01, 0x36, 0x02, 0x00, 0x0b, // store: ...; i32.store
];

/// Run the calls in order on one instance, and report each outcome as text, so a value and a
/// trap can be compared the same way.
fn outcomes(mode: EngineMode, limits: ResourceLimits, calls: &[(&str, Vec<i32>)]) -> Vec<String> {
    let engine = Engine::new(Config {
        engine_mode: mode,
        limits,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    calls
        .iter()
        .map(|(name, args)| {
            let values = args.iter().map(|value| Value::I32(*value)).collect();
            format!("{:?}", instance.call(name, values))
        })
        .collect()
}

#[test]
fn integer_results_and_traps_match_micro() {
    let values = [0, 7, -7, 1, -1, 3, i32::MIN, i32::MAX];
    let mut calls = Vec::new();
    for a in values {
        for b in values {
            calls.push(("add", vec![a, b]));
            calls.push(("div", vec![a, b]));
        }
    }
    let golden = outcomes(EngineMode::Micro, ResourceLimits::default(), &calls);
    let baseline = outcomes(EngineMode::Baseline, ResourceLimits::default(), &calls);
    let native = outcomes(EngineMode::Jit, ResourceLimits::default(), &calls);
    assert_eq!(native, golden, "the native tier disagrees with Micro");
    assert_eq!(baseline, golden, "the baseline disagrees with Micro");
    // The traps are part of the comparison, so the sequence has to include them.
    assert!(native
        .iter()
        .any(|outcome| outcome.contains("IntegerDivisionByZero")));
    assert!(native
        .iter()
        .any(|outcome| outcome.contains("IntegerOverflow")));
}

#[test]
fn a_native_load_sees_what_a_baseline_store_wrote() {
    let calls = [
        ("store", vec![0, 0x0102_0304]),
        ("load", vec![0]),
        ("load", vec![4]),
        ("store", vec![65_532, -1]),
        ("load", vec![65_532]),
        ("load", vec![65_533]),
        ("store", vec![65_536, 1]),
        ("load", vec![-1]),
    ];
    let golden = outcomes(EngineMode::Micro, ResourceLimits::default(), &calls);
    let native = outcomes(EngineMode::Jit, ResourceLimits::default(), &calls);
    assert_eq!(native, golden, "the mixed module disagrees with Micro");
    assert!(native
        .iter()
        .any(|outcome| outcome.contains("MemoryOutOfBounds")));
}

#[test]
fn only_the_functions_the_compiler_accepts_run_natively() {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Jit,
        ..Config::default()
    })
    .expect("engine");
    let instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    assert!(instance.runs_natively("add"));
    assert!(instance.runs_natively("div"));
    assert!(instance.runs_natively("load"));
    // A store returns nothing; functions with no result compile too.
    assert!(instance.runs_natively("store"));
    assert!(!instance.runs_natively("no_such_export"));
}

#[test]
fn a_step_limit_keeps_every_call_on_the_baseline() {
    let limits = ResourceLimits {
        max_execution_steps: Some(1_000_000),
        ..ResourceLimits::default()
    };
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Jit,
        limits,
        ..Config::default()
    })
    .expect("engine");
    let instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    assert!(!instance.runs_natively("add"));
}

#[test]
fn native_calls_report_no_steps_and_no_memory_growth() {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Jit,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    instance
        .call("add", vec![Value::I32(2), Value::I32(3)])
        .expect("add");
    assert_eq!(instance.last_call_steps(), 0);
    assert_eq!(instance.last_call_memory_pages_added(), 0);
}
