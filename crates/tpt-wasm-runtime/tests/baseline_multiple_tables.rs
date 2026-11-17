// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A module with two tables, each seeded by its own element segment. A `call_indirect` names the table it
//! reads, so each call reaches the function seeded into that table, on both the interpreter and the baseline.

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// Functions: `f` returns 5 and sits in table 0, `g` returns 6 and sits in table 1. `c1` calls table 1 slot
/// 0, and `c0` calls table 0 slot 0.
fn two_tables() -> Vec<u8> {
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend([1, 5, 1, 0x60, 0, 1, 0x7f]); // type 0: () -> i32
    module.extend([3, 5, 4, 0, 0, 0, 0]); // f, g, c1, c0, all type 0
    module.extend([4, 7, 2, 0x70, 0, 1, 0x70, 0, 1]); // two funcref tables, minimum 1
    module.extend([
        7, 11, 2, //
        2, b'c', b'1', 0, 2, //
        2, b'c', b'0', 0, 3,
    ]);
    module.extend([
        9, 15, 2, //
        0, 0x41, 0, 0x0b, 1, 0, // table 0 slot 0 is f
        2, 1, 0x41, 0, 0x0b, 0, 1, 1, // table 1 slot 0 is g
    ]);
    module.extend([
        10, 27, 4, //
        4, 0x00, 0x41, 5, 0x0b, // f returns 5
        4, 0x00, 0x41, 6, 0x0b, // g returns 6
        7, 0x00, 0x41, 0, 0x11, 0, 1, 0x0b, // c1: call_indirect type 0, table 1
        7, 0x00, 0x41, 0, 0x11, 0, 0, 0x0b, // c0: call_indirect type 0, table 0
    ]);
    module
}

fn engine(mode: EngineMode) -> Engine {
    Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine")
}

#[test]
fn each_call_indirect_reads_the_table_it_names_on_both_engines() {
    for mode in [EngineMode::Micro, EngineMode::Baseline] {
        let mut instance = engine(mode)
            .instantiate_bytes(&two_tables())
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        assert_eq!(
            instance.call("c1", Vec::new()).expect("runs"),
            vec![Value::I32(6)],
            "{mode:?}: table 1 holds g"
        );
        assert_eq!(
            instance.call("c0", Vec::new()).expect("runs"),
            vec![Value::I32(5)],
            "{mode:?}: table 0 holds f"
        );
    }
}
