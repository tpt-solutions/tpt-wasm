// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Block merging must not change what a value read in a later block returns.
//!
//! The module is `func(x: i32) -> i32 { (block (result i32) (i32.const 4)) ; (if (result i32)
//! x (then 2) (else 3)) ; i32.add }`. The `block` result is a merge block with one
//! predecessor, so the merge pass may fold it into its predecessor. Its value is then read
//! after the `if`, in a block the merged block does not contain. Every engine must return
//! `4 + (x ? 2 : 3)`.

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

const MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
    0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f, // type: (i32) -> i32
    0x03, 0x02, 0x01, 0x00, // function section: one function of type 0
    0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, // export "run"
    0x0a, 0x14, 0x01, 0x12, // code section: one body of 18 bytes
    0x00, // no locals
    0x02, 0x7f, // block (result i32)
    0x41, 0x04, //   i32.const 4
    0x0b, //              end
    0x20, 0x00, // local.get 0
    0x04, 0x7f, // if (result i32)
    0x41, 0x02, //   i32.const 2
    0x05, //        else
    0x41, 0x03, //   i32.const 3
    0x0b, //        end
    0x6a, // i32.add
    0x0b, // end
];

fn run(mode: EngineMode, x: i32) -> Vec<Value> {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(MODULE)
        .expect("module instantiates");
    instance
        .call("run", vec![Value::I32(x)])
        .expect("run returns")
}

#[test]
fn a_merged_block_value_read_after_an_if_is_the_same_on_every_engine() {
    for x in [0, 1] {
        let expected = Value::I32(4 + if x != 0 { 2 } else { 3 });
        assert_eq!(
            run(EngineMode::Micro, x),
            vec![expected.clone()],
            "Micro, x = {x}"
        );
        for mode in [EngineMode::Baseline, EngineMode::Optimizing] {
            assert_eq!(run(mode, x), vec![expected.clone()], "{mode:?}, x = {x}");
        }
    }
}
