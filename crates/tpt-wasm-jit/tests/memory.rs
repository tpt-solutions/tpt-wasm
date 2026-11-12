// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Compiled memory access must agree with Micro: the same loaded values, the same traps at the
//! same accesses, and the same memory contents after the call.
//!
//! Each case stores a value at one address and loads from another, with random store and load
//! widths and signedness, and random offsets. Addresses are drawn near the end of the one page
//! of memory as often as inside it, so out-of-bounds accesses are common.
#![cfg_attr(
    not(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )),
    allow(dead_code, unused_imports)
)]

use tpt_wasm_format::{Export, ExportDesc, Function, Memory, Module};
use tpt_wasm_jit::compile;
use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{FunctionType, Limits, MemoryType, ResultType, Trap, Value, ValueType};

const PAGE: usize = 65_536;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// An address: inside the page, or near its end, where an access straddles the limit.
    fn address(&mut self) -> i32 {
        match self.below(4) {
            0 => (PAGE as i64 - 1 - self.below(8) as i64) as i32,
            1 => (PAGE as i64 + self.below(16) as i64) as i32,
            _ => self.below(PAGE as u64) as i32,
        }
    }
}

/// An unsigned LEB128 encoding, for a memory access offset.
fn uleb(mut value: u32) -> Vec<u8> {
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

/// A memory access with alignment 0 and the given offset.
fn access(opcode: u8, offset: u32) -> Vec<u8> {
    let mut out = vec![opcode, 0x00];
    out.extend(uleb(offset));
    out
}

/// The store opcodes: `i32.store`, `i32.store8`, `i32.store16`.
const STORES: [u8; 3] = [0x36, 0x3A, 0x3B];
/// The load opcodes: `i32.load`, `load8_s`, `load8_u`, `load16_s`, `load16_u`.
const LOADS: [u8; 5] = [0x28, 0x2C, 0x2D, 0x2E, 0x2F];

/// `store(p0 + offset_a) = p1; return load(p0 + offset_b)`, with random widths.
fn body(store: u8, load: u8, offset_a: u32, offset_b: u32) -> Vec<u8> {
    let mut body = vec![0x20, 0x00, 0x20, 0x01]; // address, value
    body.extend(access(store, offset_a));
    body.extend([0x20, 0x00]); // address
    body.extend(access(load, offset_b));
    body.push(0x0B);
    body
}

fn module(body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32, ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            body,
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
                memory64: false,
            },
        }],
        exports: vec![
            Export {
                name: "run".into(),
                desc: ExportDesc::Function(0),
            },
            Export {
                name: "mem".into(),
                desc: ExportDesc::Memory(0),
            },
        ],
        ..Module::default()
    }
}

/// Micro's result and the page after the call, with the memory starting zeroed.
fn micro(module: &Module, args: [i32; 2]) -> (Result<i32, Trap>, Vec<u8>) {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("accepted");
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    let outcome = match instance.call("run", vec![Value::I32(args[0]), Value::I32(args[1])]) {
        Ok(values) => match values.as_slice() {
            [Value::I32(result)] => Ok(*result),
            other => panic!("expected one i32, got {other:?}"),
        },
        Err(RuntimeError::Trap(trap)) => Err(trap),
        Err(other) => panic!("Micro failed in a way that is not a trap: {other:?}"),
    };
    let memory = instance
        .read_memory("mem", 0, PAGE)
        .expect("reads the page");
    (outcome, memory)
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
#[test]
fn compiled_memory_access_agrees_with_micro() {
    let mut rng = Lcg(0x3E30_5EED);
    let mut values = 0usize;
    let mut out_of_bounds = 0usize;
    for case in 0..240 {
        let store = STORES[rng.below(STORES.len() as u64) as usize];
        let load = LOADS[rng.below(LOADS.len() as u64) as usize];
        let offset_a = if rng.below(3) == 0 {
            rng.below(70_000) as u32
        } else {
            rng.below(64) as u32
        };
        let offset_b = if rng.below(3) == 0 {
            rng.below(70_000) as u32
        } else {
            rng.below(64) as u32
        };
        let module = module(body(store, load, offset_a, offset_b));
        let validated = tpt_wasm_validate::validate(module.clone()).expect("validates");
        let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("lowers");
        let code = compile(&verified.module().functions[0])
            .unwrap_or_else(|error| panic!("case {case}: {error}"));

        for _ in 0..6 {
            let args = [rng.address(), rng.next() as u32 as i32];
            let (expected, expected_memory) = micro(&module, args);
            let mut memory = vec![0u8; PAGE];
            let actual = code
                .call_with_memory(&args, &mut memory)
                .map_err(|error| match error {
                    tpt_wasm_jit::JitError::Trap(trap) => trap,
                    other => panic!("the compiled code failed without a trap: {other}"),
                });
            assert_eq!(
                actual, expected,
                "case {case} (store {store:#x}, load {load:#x}), args {args:?}: results differ"
            );
            assert!(
                memory == expected_memory,
                "case {case}, args {args:?}: the memory contents differ"
            );
            match expected {
                Ok(_) => values += 1,
                Err(Trap::MemoryOutOfBounds) => out_of_bounds += 1,
                Err(other) => panic!("unexpected trap {other:?}"),
            }
        }
    }
    // Guard against a vacuous pass: both kinds of outcome must occur.
    assert!(values > 300, "in-bounds results: {values}");
    assert!(out_of_bounds > 100, "out-of-bounds traps: {out_of_bounds}");
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
#[test]
fn a_call_without_memory_traps_on_any_access() {
    let module = module(body(0x36, 0x28, 0, 0));
    let validated = tpt_wasm_validate::validate(module).expect("validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("lowers");
    let code = compile(&verified.module().functions[0]).expect("compiles");
    assert_eq!(
        code.call(&[0, 1]),
        Err(tpt_wasm_jit::JitError::Trap(Trap::MemoryOutOfBounds))
    );
}
