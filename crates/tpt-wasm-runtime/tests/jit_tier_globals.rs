//! Globals read and written by native functions through `EngineMode::Jit`, checked against Micro:
//! the results, the traps, and the value the global holds afterwards, including after a trap.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// Append a section with `content`, which must be shorter than 128 bytes so one byte gives its length.
fn section(bytes: &mut Vec<u8>, id: u8, content: &[u8]) {
    bytes.push(id);
    bytes.push(content.len() as u8);
    bytes.extend(content);
}

/// A module with a mutable `i32` global `counter` that starts at zero, and one exported function
/// `name` of type `(i32) -> i32` whose body is `body`.
fn module(name: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // type: (i32) -> i32
    section(&mut bytes, 1, &[0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]);
    // one function, of type 0
    section(&mut bytes, 3, &[0x01, 0x00]);
    // global: mutable i32, initialised to 0
    section(&mut bytes, 6, &[0x01, 0x7f, 0x01, 0x41, 0x00, 0x0b]);
    // exports: the function, and the global as "counter"
    let mut exports = vec![0x02, name.len() as u8];
    exports.extend(name.as_bytes());
    exports.extend([0x00, 0x00, 0x07]);
    exports.extend(b"counter");
    exports.extend([0x03, 0x00]);
    section(&mut bytes, 7, &exports);
    // one body: no locals, then the code
    let mut body_bytes = vec![0x00];
    body_bytes.extend(body);
    let mut code = vec![0x01, body_bytes.len() as u8];
    code.extend(body_bytes);
    section(&mut bytes, 10, &code);
    bytes
}

/// `bump(x)`: `counter = counter + x`, and the new counter.
fn bump() -> Vec<u8> {
    module(
        "bump",
        &[
            0x23, 0x00, // global.get 0
            0x20, 0x00, // local.get 0
            0x6a, // i32.add
            0x24, 0x00, // global.set 0
            0x23, 0x00, // global.get 0
            0x0b,
        ],
    )
}

/// `divide(x)`: `counter = x`, then `1 / x`. A zero traps after the store.
fn divide() -> Vec<u8> {
    module(
        "divide",
        &[
            0x20, 0x00, // local.get 0
            0x24, 0x00, // global.set 0
            0x41, 0x01, // i32.const 1
            0x20, 0x00, // local.get 0
            0x6d, // i32.div_s
            0x0b,
        ],
    )
}

/// The outcome of each call, the counter afterwards, and whether the function ran natively.
fn run(mode: EngineMode, bytes: &[u8], name: &str, args: &[i32]) -> (Vec<String>, String, bool) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(bytes)
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
fn a_native_function_updates_a_global_as_micro_does() {
    let bytes = bump();
    let (golden, golden_counter, _) = run(EngineMode::Micro, &bytes, "bump", &[1, 2, 39]);
    let (native, native_counter, natively) = run(EngineMode::Jit, &bytes, "bump", &[1, 2, 39]);
    assert!(
        natively,
        "bump runs as machine code, reading and writing the global"
    );
    assert_eq!(native, golden, "the results agree with Micro");
    assert_eq!(
        native_counter, golden_counter,
        "the global agrees with Micro"
    );
    assert_eq!(native_counter, "Ok(I32(42))");
    assert_eq!(native.last().map(String::as_str), Some("Ok([I32(42)])"));
}

#[test]
fn a_global_set_before_a_trap_keeps_its_value_as_on_micro() {
    let bytes = divide();
    let calls = [5, 0, 4];
    let (golden, golden_counter, _) = run(EngineMode::Micro, &bytes, "divide", &calls);
    let (native, native_counter, natively) = run(EngineMode::Jit, &bytes, "divide", &calls);
    assert!(natively, "divide runs as machine code");
    assert_eq!(native, golden, "the results and the trap agree with Micro");
    assert_eq!(
        native_counter, golden_counter,
        "the global agrees with Micro"
    );
    assert_eq!(
        native[1], "Err(Trap(IntegerDivisionByZero))",
        "a zero divisor traps"
    );
    assert_eq!(
        native_counter, "Ok(I32(4))",
        "the last store is the one that stays"
    );
}
