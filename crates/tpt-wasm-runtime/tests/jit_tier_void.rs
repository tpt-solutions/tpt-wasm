//! Functions that return nothing, run natively through `EngineMode::Jit`, checked against Micro: the
//! empty result, the global each one leaves, and a call from one such function to another.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// A module with a mutable `i32` global `counter` and two functions of type `(i32) -> ()`:
/// `set(x)` stores `x` in the global, and `twice(x)` calls `set(x)` twice.
fn module() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // type: (i32) -> ()
    bytes.extend([0x01, 0x05, 0x01, 0x60, 0x01, 0x7f, 0x00]);
    // two functions, both of type 0
    bytes.extend([0x03, 0x03, 0x02, 0x00, 0x00]);
    // global: mutable i32, initialised to 0
    bytes.extend([0x06, 0x06, 0x01, 0x7f, 0x01, 0x41, 0x00, 0x0b]);
    // exports: "set", "twice", and the global as "counter"
    let mut exports = vec![0x03];
    exports.extend([0x03]);
    exports.extend(b"set");
    exports.extend([0x00, 0x00]);
    exports.extend([0x05]);
    exports.extend(b"twice");
    exports.extend([0x00, 0x01]);
    exports.extend([0x07]);
    exports.extend(b"counter");
    exports.extend([0x03, 0x00]);
    bytes.push(0x07);
    bytes.push(exports.len() as u8);
    bytes.extend(exports);
    // code: set = local.get 0; global.set 0. twice = call set; call set.
    let set = [0x00, 0x20, 0x00, 0x24, 0x00, 0x0b];
    let twice = [0x00, 0x20, 0x00, 0x10, 0x00, 0x20, 0x00, 0x10, 0x00, 0x0b];
    bytes.extend([0x0a, (1 + 1 + set.len() + 1 + twice.len()) as u8, 0x02]);
    bytes.push(set.len() as u8);
    bytes.extend(set);
    bytes.push(twice.len() as u8);
    bytes.extend(twice);
    bytes
}

/// The outcome of each call, the counter afterwards, and whether the function ran natively.
fn run(mode: EngineMode, name: &str, args: &[i32]) -> (Vec<String>, String, bool) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(&module())
        .expect("module instantiates");
    let natively = instance.runs_natively(name);
    let outcomes = args
        .iter()
        .map(|arg| format!("{:?}", instance.call(name, vec![Value::I32(*arg)])))
        .collect();
    let counter = format!("{:?}", instance.global("counter"));
    (outcomes, counter, natively)
}

#[test]
fn a_function_with_no_result_runs_natively_and_returns_nothing() {
    let (golden, golden_counter, _) = run(EngineMode::Micro, "set", &[7, -3]);
    let (native, native_counter, natively) = run(EngineMode::Jit, "set", &[7, -3]);
    assert!(natively, "set runs as machine code");
    assert_eq!(native, golden, "the results agree with Micro");
    assert_eq!(
        native,
        ["Ok([])", "Ok([])"],
        "a function with no result returns nothing"
    );
    assert_eq!(
        native_counter, golden_counter,
        "the global agrees with Micro"
    );
    assert_eq!(native_counter, "Ok(I32(-3))");
}

#[test]
fn a_call_between_functions_with_no_result_runs_natively() {
    let (golden, golden_counter, _) = run(EngineMode::Micro, "twice", &[9, 2]);
    let (native, native_counter, natively) = run(EngineMode::Jit, "twice", &[9, 2]);
    assert!(natively, "twice, which calls set, runs as machine code");
    assert_eq!(native, golden, "the results agree with Micro");
    assert_eq!(
        native_counter, golden_counter,
        "the global agrees with Micro"
    );
    assert_eq!(native_counter, "Ok(I32(2))");
}
