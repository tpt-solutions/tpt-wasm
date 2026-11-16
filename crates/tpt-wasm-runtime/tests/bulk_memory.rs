// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The bulk memory instructions on the interpreter: `memory.init` from a passive segment, `memory.fill`,
//! `memory.copy`, and `data.drop`, with the trap a dropped segment raises.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{Trap, Value};

fn leb(mut value: u32) -> Vec<u8> {
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

fn section(id: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(leb(content.len() as u32));
    out.extend_from_slice(content);
    out
}

/// A function body with no locals, from its instructions, sized as the binary format requires.
fn body(instructions: &[u8]) -> Vec<u8> {
    let mut content = vec![0x00];
    content.extend_from_slice(instructions);
    content.push(0x0b);
    let mut out = leb(content.len() as u32);
    out.extend(content);
    out
}

/// `run() -> i32`: `memory.init` `bc` (offset 1, length 2) to 10, `memory.fill` `A` three times at 20,
/// `memory.copy` the two bytes at 10 to 30, then `data.drop`. Returns 0.
/// `reinit() -> i32`: `memory.init` one byte from the segment, which is now dropped, so it traps.
fn bulk_module() -> Vec<u8> {
    let run = body(&[
        0x41, 0x0a, 0x41, 0x01, 0x41, 0x02, 0xfc, 8, 0,
        0, // memory.init 0 (dst 10, src 1, len 2)
        0x41, 0x14, 0x41, 0xc1, 0x00, 0x41, 0x03, 0xfc, 11,
        0, // memory.fill (dst 20, value 'A', len 3)
        0x41, 0x1e, 0x41, 0x0a, 0x41, 0x02, 0xfc, 10, 0,
        0, // memory.copy (dst 30, src 10, len 2)
        0xfc, 9, 0, // data.drop 0
        0x41, 0x00,
    ]);
    let reinit = body(&[
        0x41, 0x00, 0x41, 0x00, 0x41, 0x01, 0xfc, 8, 0,
        0, // memory.init 0 of one byte, from a dropped segment
        0x41, 0x00,
    ]);
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend(section(1, &[1, 0x60, 0, 1, 0x7f])); // () -> i32
    module.extend(section(3, &[2, 0, 0])); // run and reinit
    module.extend(section(5, &[1, 0x00, 1])); // one memory, one page
    let mut exports = vec![3];
    exports.extend([3, b'r', b'u', b'n', 0x00, 0]);
    exports.extend([6, b'r', b'e', b'i', b'n', b'i', b't', 0x00, 1]);
    exports.extend([6, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0]);
    module.extend(section(7, &exports));
    module.extend(section(12, &[1])); // data count: one segment
    let mut code = vec![2];
    code.extend(run);
    code.extend(reinit);
    module.extend(section(10, &code));
    module.extend(section(11, &[1, 0x01, 3, b'a', b'b', b'c'])); // passive: `abc`
    module
}

fn engine() -> Engine {
    Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("engine")
}

#[test]
fn the_bulk_operations_write_what_they_name_and_a_dropped_segment_traps() {
    let mut instance = engine()
        .instantiate_bytes(&bulk_module())
        .expect("instantiates");
    assert_eq!(
        instance.call("run", Vec::new()).expect("runs"),
        vec![Value::I32(0)]
    );
    assert_eq!(
        instance.read_memory("memory", 10, 2).expect("reads"),
        b"bc".to_vec()
    );
    assert_eq!(
        instance.read_memory("memory", 20, 3).expect("reads"),
        b"AAA".to_vec()
    );
    assert_eq!(
        instance.read_memory("memory", 30, 2).expect("reads"),
        b"bc".to_vec()
    );
    assert_eq!(
        instance.read_memory("memory", 0, 1).expect("reads"),
        vec![0],
        "nothing outside the named ranges is written"
    );
    let error = instance
        .call("reinit", Vec::new())
        .expect_err("the segment is dropped, so it is empty");
    assert!(
        matches!(error, RuntimeError::Trap(Trap::MemoryOutOfBounds)),
        "{error:?}"
    );
}
