// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The table instructions that take a passive element segment, on the interpreter and on the baseline. A
//! passive segment is read by `table.init` until `elem.drop` discards it, and `table.copy` moves entries
//! between slots. Both engines must give the same results and the same traps.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{Trap, Value};

/// A four-slot funcref table, and a passive segment that holds one function, `f`, which returns 5.
/// Exports: `init(dst, src, len)`, `drop()`, `copy(dst, src, len)`, `size() -> i32`, and `call(index) -> i32`,
/// which calls through the table.
fn module() -> Vec<u8> {
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend([
        1, 0x13, 4, //
        0x60, 0, 1, 0x7f, // 0: () -> i32
        0x60, 3, 0x7f, 0x7f, 0x7f, 0, // 1: (i32, i32, i32) -> ()
        0x60, 0, 0, // 2: () -> ()
        0x60, 1, 0x7f, 1, 0x7f, // 3: (i32) -> i32
    ]);
    // f, init, drop, size, copy, call.
    module.extend([3, 7, 6, 0, 1, 2, 0, 1, 3]);
    module.extend([4, 4, 1, 0x70, 0, 4]); // one funcref table, minimum 4
    module.extend([
        7, 0x24, 5, //
        4, b'i', b'n', b'i', b't', 0, 1, //
        4, b'd', b'r', b'o', b'p', 0, 2, //
        4, b'c', b'o', b'p', b'y', 0, 4, //
        4, b's', b'i', b'z', b'e', 0, 3, //
        4, b'c', b'a', b'l', b'l', 0, 5,
    ]);
    // Passive segment 0: flags 1, an elemkind of funcref, and one function index, 0.
    module.extend([9, 5, 1, 1, 0x00, 1, 0]);
    module.extend([
        10, 0x34, 6, //
        4, 0x00, 0x41, 5, 0x0b, // f
        12, 0x00, 0x20, 0x00, 0x20, 0x01, 0x20, 0x02, 0xfc, 12, 0, 0,
        0x0b, // init: table.init 0 0
        5, 0x00, 0xfc, 13, 0, 0x0b, // drop: elem.drop 0
        5, 0x00, 0xfc, 16, 0, 0x0b, // size: table.size 0
        12, 0x00, 0x20, 0x00, 0x20, 0x01, 0x20, 0x02, 0xfc, 14, 0, 0,
        0x0b, // copy: table.copy 0 0
        7, 0x00, 0x20, 0x00, 0x11, 0, 0, 0x0b, // call: call_indirect type 0 through table 0
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

fn trap_of(error: RuntimeError) -> Trap {
    match error {
        RuntimeError::Trap(trap) => trap,
        other => panic!("expected a trap, got {other:?}"),
    }
}

#[test]
fn the_table_instructions_agree_on_both_engines() {
    for mode in [EngineMode::Micro, EngineMode::Baseline] {
        let mut instance = engine(mode)
            .instantiate_bytes(&module())
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        let args = |values: &[i32]| values.iter().map(|v| Value::I32(*v)).collect::<Vec<_>>();

        assert_eq!(
            instance.call("size", Vec::new()).expect("size"),
            args(&[4]),
            "{mode:?}"
        );
        assert_eq!(
            trap_of(
                instance
                    .call("call", args(&[0]))
                    .expect_err("slot 0 is empty")
            ),
            Trap::NullReference,
            "{mode:?}: an empty slot is a null call"
        );

        instance
            .call("init", args(&[0, 0, 1]))
            .expect("init copies f into slot 0");
        assert_eq!(
            instance.call("call", args(&[0])).expect("call"),
            args(&[5]),
            "{mode:?}"
        );

        instance
            .call("copy", args(&[1, 0, 1]))
            .expect("copy moves f to slot 1");
        assert_eq!(
            instance.call("call", args(&[1])).expect("call"),
            args(&[5]),
            "{mode:?}"
        );

        instance
            .call("init", args(&[2, 0, 0]))
            .expect("a zero-length init is in range");
        assert_eq!(
            trap_of(
                instance
                    .call("call", args(&[2]))
                    .expect_err("slot 2 was not written")
            ),
            Trap::NullReference,
            "{mode:?}"
        );

        instance.call("drop", Vec::new()).expect("elem.drop");
        assert_eq!(
            trap_of(
                instance
                    .call("init", args(&[0, 0, 1]))
                    .expect_err("the segment is dropped")
            ),
            Trap::TableOutOfBounds,
            "{mode:?}: a dropped segment is empty, so a copy of one entry traps"
        );

        assert_eq!(
            trap_of(
                instance
                    .call("call", args(&[4]))
                    .expect_err("slot 4 is past the table")
            ),
            Trap::TableOutOfBounds,
            "{mode:?}"
        );
    }
}
