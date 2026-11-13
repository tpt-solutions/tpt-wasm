//! The `unreachable` instruction in native code traps as it does on Micro.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// A module with one exported function `trap` of type `(i32) -> i32` whose body is `unreachable`.
const MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f, // type section: (i32) -> i32
    0x03, 0x02, 0x01, 0x00, // function section
    0x07, 0x08, 0x01, 0x04, 0x74, 0x72, 0x61, 0x70, 0x00, 0x00, // export "trap"
    0x0a, 0x05, 0x01, 0x03, 0x00, 0x00, 0x0b, // code: no locals; unreachable; end
];

fn outcome(mode: EngineMode) -> (String, bool) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    let natively = instance.runs_natively("trap");
    let result = format!("{:?}", instance.call("trap", vec![Value::I32(1)]));
    (result, natively)
}

#[test]
fn unreachable_traps_natively_as_it_does_on_micro() {
    let (golden, _) = outcome(EngineMode::Micro);
    let (native, natively) = outcome(EngineMode::Jit);
    assert!(natively, "the function runs as machine code");
    assert_eq!(native, golden, "the trap agrees with Micro");
    assert_eq!(native, "Err(Trap(Unreachable))");
}
