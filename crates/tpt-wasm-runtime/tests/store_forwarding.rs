// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The optimizer's store-to-load forwarding and redundant-store removal, checked against Micro.
//!
//! Each module writes and reads one memory at address 0 and returns the loaded word. The optimizing
//! engine must agree with Micro on every one.

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

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

fn section(id: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(leb(contents.len() as u32));
    out.extend_from_slice(contents);
    out
}

/// A module with one memory of one page and `f(i32, i32) -> i32`, exported as `f`, whose body is
/// `code` followed by the closing `end`.
fn module(code: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(0x01, &[0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f]));
    bytes.extend(section(0x03, &[0x01, 0x00]));
    bytes.extend(section(0x05, &[0x01, 0x00, 0x01]));
    bytes.extend(section(0x07, &[0x01, 0x01, b'f', 0x00, 0x00]));
    let mut inner = vec![0x00];
    inner.extend_from_slice(code);
    inner.push(0x0b);
    let mut body = leb(inner.len() as u32);
    body.extend(inner);
    let mut code_section = vec![0x01];
    code_section.extend(body);
    bytes.extend(section(0x0a, &code_section));
    bytes
}

/// Run `f(a, b)` on Micro and on the optimizing engine, and check both give `expected`.
fn check(name: &str, code: &[u8], a: i32, b: i32, expected: i32) {
    for mode in [EngineMode::Micro, EngineMode::Optimizing] {
        let engine = Engine::new(Config {
            engine_mode: mode,
            ..Config::default()
        })
        .expect("engine");
        let mut instance = engine
            .instantiate_bytes(&module(code))
            .expect("instantiates");
        let result = instance
            .call("f", vec![Value::I32(a), Value::I32(b)])
            .expect("runs");
        assert_eq!(result, vec![Value::I32(expected)], "{name} on {mode:?}");
    }
}

#[test]
fn a_store_after_a_store_of_a_dynamic_value_is_kept() {
    // mem[0] = a; mem[0] = b; return mem[0]. The second store is not a repeat of the first.
    let code = [
        0x41, 0x00, 0x20, 0x00, 0x36, 0x02, 0x00, // i32.store (a)
        0x41, 0x00, 0x20, 0x01, 0x36, 0x02, 0x00, // i32.store (b)
        0x41, 0x00, 0x28, 0x02, 0x00, // i32.load
    ];
    check("two dynamic stores", &code, 7, 9, 9);
}

#[test]
fn a_store_that_repeats_an_older_value_after_another_store_is_kept() {
    // mem[0] = 1; mem[0] = 2; mem[0] = 1; return mem[0]. The last store writes 1 again, but it is
    // not redundant: the store of 2 came between.
    let code = [
        0x41, 0x00, 0x41, 0x01, 0x36, 0x02, 0x00, //
        0x41, 0x00, 0x41, 0x02, 0x36, 0x02, 0x00, //
        0x41, 0x00, 0x41, 0x01, 0x36, 0x02, 0x00, //
        0x41, 0x00, 0x28, 0x02, 0x00,
    ];
    check("a repeat after an intervening store", &code, 0, 0, 1);
}

#[test]
fn a_narrow_store_inside_an_earlier_wide_store_is_seen_by_the_load() {
    // mem[0..4] = 1 (little-endian), then mem[1] = 0x22 (one byte), then return the word at 0:
    // bytes 01 22 00 00, which is 0x2201 = 8705. A load that forwarded the older word would give 1.
    let code = [
        0x41, 0x00, 0x41, 0x01, 0x36, 0x02, 0x00, //
        0x41, 0x00, 0x41, 0x22, 0x3a, 0x00, 0x01, //
        0x41, 0x00, 0x28, 0x02, 0x00,
    ];
    check("a partial overwrite", &code, 0, 0, 0x2201);
}

/// Like [`module`], with two memories of one page, the second named by the multi-memory flag on each access.
fn two_memory_module(code: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(0x01, &[0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f]));
    bytes.extend(section(0x03, &[0x01, 0x00]));
    bytes.extend(section(0x05, &[0x02, 0x00, 0x01, 0x00, 0x01]));
    bytes.extend(section(0x07, &[0x01, 0x01, b'f', 0x00, 0x00]));
    let mut inner = vec![0x00];
    inner.extend_from_slice(code);
    inner.push(0x0b);
    let mut body = leb(inner.len() as u32);
    body.extend(inner);
    let mut code_section = vec![0x01];
    code_section.extend(body);
    bytes.extend(section(0x0a, &code_section));
    bytes
}

#[test]
fn a_store_to_one_memory_is_not_forwarded_to_a_load_of_another() {
    // memory 1 [0] = 7; memory 0 [0] = 9; return memory 1 [0]. The load reads memory 1, which holds 7. The
    // store to memory 0 at the same address must not be taken for a store to memory 1.
    let code = [
        0x41, 0x00, 0x41, 0x07, 0x36, 0x42, 0x01,
        0x00, // i32.store: flags 0x42, memory 1, offset 0
        0x41, 0x00, 0x41, 0x09, 0x36, 0x02, 0x00, // i32.store, memory 0
        0x41, 0x00, 0x28, 0x42, 0x01, 0x00, // i32.load: flags 0x42, memory 1, offset 0
    ];
    for mode in [EngineMode::Micro, EngineMode::Optimizing] {
        let engine = Engine::new(Config {
            engine_mode: mode,
            ..Config::default()
        })
        .expect("engine");
        let mut instance = engine
            .instantiate_bytes(&two_memory_module(&code))
            .expect("instantiates");
        let result = instance
            .call("f", vec![Value::I32(0), Value::I32(0)])
            .expect("runs");
        assert_eq!(result, vec![Value::I32(7)], "on {mode:?}");
    }
}
