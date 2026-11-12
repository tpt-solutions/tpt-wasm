// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Compiled `i64` code must agree with Micro: arithmetic and its traps, comparisons, width
//! conversions, sign extensions, `select`, and the integer memory accesses of every width.
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
use tpt_wasm_jit::{compile, CompiledFunction, JitError};
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
        self.0 >> 3
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// An `i64` operand: an edge value, a small value, or an arbitrary one.
    fn operand(&mut self) -> i64 {
        const EDGES: [i64; 8] = [0, 1, -1, 2, i64::MIN, i64::MAX, i64::MIN + 1, -7];
        match self.below(4) {
            0 => self.next() as i64,
            1 => EDGES[self.below(EDGES.len() as u64) as usize],
            _ => self.below(200) as i64 - 100,
        }
    }

    /// An address: inside the page or near its end.
    fn address(&mut self) -> i64 {
        match self.below(3) {
            0 => PAGE as i64 - 1 - self.below(16) as i64,
            1 => PAGE as i64 + self.below(16) as i64,
            _ => self.below(PAGE as u64) as i64,
        }
    }
}

fn sleb(mut value: i64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

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

fn module(params: usize, result: ValueType, body: Vec<u8>, memory: bool) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I64; params]),
            results: ResultType(vec![result]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            body,
        }],
        memories: if memory {
            vec![Memory {
                memory_type: MemoryType {
                    limits: Limits {
                        min: 1,
                        max: Some(1),
                    },
                    memory64: false,
                },
            }]
        } else {
            Vec::new()
        },
        exports: {
            let mut exports = vec![Export {
                name: "run".into(),
                desc: ExportDesc::Function(0),
            }];
            if memory {
                exports.push(Export {
                    name: "mem".into(),
                    desc: ExportDesc::Memory(0),
                });
            }
            exports
        },
        ..Module::default()
    }
}

/// A two-parameter `i64` operation: `p0 op p1`.
fn binary(op: u8) -> (usize, ValueType, Vec<u8>) {
    (2, ValueType::I64, vec![0x20, 0x00, 0x20, 0x01, op, 0x0B])
}

/// A comparison of `i64` parameters, widened to an `i64` result.
fn compare(op: u8) -> (usize, ValueType, Vec<u8>) {
    (
        2,
        ValueType::I64,
        vec![0x20, 0x00, 0x20, 0x01, op, 0xAD, 0x0B], // i64.extend_i32_u
    )
}

/// The i64 operations, as opcodes: add sub mul div_s div_u rem_s rem_u and or xor shl shr_s shr_u.
const ARITHMETIC: [u8; 13] = [
    0x7C, 0x7D, 0x7E, 0x7F, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
];
/// The comparisons: eq ne lt_s lt_u gt_s gt_u le_s le_u ge_s ge_u.
const COMPARISONS: [u8; 10] = [0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A];

/// Memory stores of an `i64` value: store, store8, store16, store32.
const STORES: [u8; 4] = [0x37, 0x3C, 0x3D, 0x3E];
/// Memory loads into an `i64` value: 8s, 8u, 16s, 16u, 32s, 32u, and the full 64.
const LOADS: [u8; 7] = [0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x29];

fn micro(module: &Module, args: &[i64], result: ValueType) -> (Result<Value, Trap>, Vec<u8>) {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("accepted");
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    let values: Vec<Value> = args.iter().map(|value| Value::I64(*value)).collect();
    let outcome = match instance.call("run", values) {
        Ok(results) => match results.as_slice() {
            [value] => {
                assert!(
                    matches!(
                        (value, result),
                        (Value::I64(_), ValueType::I64) | (Value::I32(_), ValueType::I32)
                    ),
                    "Micro's result type"
                );
                Ok(value.clone())
            }
            other => panic!("expected one result, got {other:?}"),
        },
        Err(RuntimeError::Trap(trap)) => Err(trap),
        Err(other) => panic!("Micro failed in a way that is not a trap: {other:?}"),
    };
    let memory = if module.memories.is_empty() {
        Vec::new()
    } else {
        instance
            .read_memory("mem", 0, PAGE)
            .expect("reads the page")
    };
    (outcome, memory)
}

fn compiled(module: &Module) -> CompiledFunction {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("the case validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the case lowers");
    compile(&verified.module().functions[0]).expect("the case is in the subset")
}

/// Run a compiled `i64` function and map its outcome to the value Micro would report.
fn jit_outcome(
    code: &CompiledFunction,
    args: &[i64],
    memory: Option<&mut [u8]>,
    result: ValueType,
) -> Result<Value, Trap> {
    let outcome = match (result, memory) {
        (ValueType::I64, None) => code.call_i64(args).map(Value::I64),
        (ValueType::I64, Some(memory)) => code.call_i64_with_memory(args, memory).map(Value::I64),
        (ValueType::I32, None) => code
            .call(&args.iter().map(|v| *v as i32).collect::<Vec<_>>())
            .map(Value::I32),
        (ValueType::I32, Some(memory)) => code
            .call_with_memory(&args.iter().map(|v| *v as i32).collect::<Vec<_>>(), memory)
            .map(Value::I32),
        _ => unreachable!("the templates return i32 or i64"),
    };
    match outcome {
        Ok(value) => Ok(value),
        Err(JitError::Trap(trap)) => Err(trap),
        Err(other) => panic!("the compiled code failed without a trap: {other}"),
    }
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
fn compiled_i64_arithmetic_agrees_with_micro() {
    let mut rng = Lcg(0x6464_6464);
    let mut values = 0usize;
    let mut traps = 0usize;
    let mut cases: Vec<(usize, ValueType, Vec<u8>, &str)> = Vec::new();
    for op in ARITHMETIC {
        let (params, result, body) = binary(op);
        cases.push((params, result, body, "arithmetic"));
    }
    for op in COMPARISONS {
        let (params, result, body) = compare(op);
        cases.push((params, result, body, "comparison"));
    }
    // eqz, wrap then extend, the sign extensions, and select.
    cases.push((1, ValueType::I64, vec![0x20, 0x00, 0x50, 0xAD, 0x0B], "eqz"));
    cases.push((
        1,
        ValueType::I64,
        vec![0x20, 0x00, 0xA7, 0xAC, 0x0B],
        "wrap-extend",
    ));
    cases.push((
        1,
        ValueType::I64,
        vec![0x20, 0x00, 0xA7, 0xAD, 0x0B],
        "wrap-extend-u",
    ));
    for ext in [0xC2u8, 0xC3, 0xC4] {
        cases.push((
            1,
            ValueType::I64,
            vec![0x20, 0x00, ext, 0x0B],
            "sign-extend",
        ));
    }
    let mut select = vec![0x20, 0x00, 0x20, 0x01, 0x20, 0x00];
    select.push(0x42);
    select.extend(sleb(0x0F0F_0F0F));
    select.extend([0x56, 0x1B, 0x0B]); // gt_u, select
    cases.push((2, ValueType::I64, select, "select"));

    for (params, result, body, name) in cases {
        let module = module(params, result, body, false);
        let code = compiled(&module);
        for _ in 0..150 {
            let args: Vec<i64> = (0..params).map(|_| rng.operand()).collect();
            let (expected, _) = micro(&module, &args, result);
            let actual = jit_outcome(&code, &args, None, result);
            assert_eq!(actual, expected, "{name}, args {args:?}");
            match expected {
                Ok(_) => values += 1,
                Err(_) => traps += 1,
            }
        }
    }
    // Guard against a vacuous pass: values and traps must both occur.
    assert!(values > 1000, "values: {values}");
    assert!(traps > 20, "traps: {traps}");
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
fn compiled_i64_memory_agrees_with_micro_at_every_width() {
    let mut rng = Lcg(0x0326_40E5);
    let mut inside = 0usize;
    let mut outside = 0usize;
    for case in 0..200 {
        let store = STORES[rng.below(STORES.len() as u64) as usize];
        let load = LOADS[rng.below(LOADS.len() as u64) as usize];
        let offset_a = rng.below(64) as u32;
        let offset_b = if rng.below(4) == 0 {
            rng.below(70_000) as u32
        } else {
            rng.below(64) as u32
        };
        // p0 is the address, p1 the value. Both memory operands take an i32 address.
        let mut body = vec![0x20, 0x00, 0xA7, 0x20, 0x01, store, 0x00];
        body.extend(uleb(offset_a));
        body.extend([0x20, 0x00, 0xA7, load, 0x00]);
        body.extend(uleb(offset_b));
        body.push(0x0B);
        let module = module(2, ValueType::I64, body, true);
        let code = compiled(&module);
        for _ in 0..6 {
            let args = [rng.address(), rng.operand()];
            let (expected, expected_memory) = micro(&module, &args, ValueType::I64);
            let mut memory = vec![0u8; PAGE];
            let actual = jit_outcome(&code, &args, Some(&mut memory), ValueType::I64);
            assert_eq!(
                actual, expected,
                "case {case}: store {store:#x}, load {load:#x}, args {args:?}"
            );
            assert!(
                memory == expected_memory,
                "case {case}: the memory contents differ"
            );
            match expected {
                Ok(_) => inside += 1,
                Err(_) => outside += 1,
            }
        }
    }
    assert!(inside > 300, "in-bounds accesses: {inside}");
    assert!(outside > 50, "out-of-bounds accesses: {outside}");
}

#[test]
fn an_f32_reinterpretation_is_compiled_and_not_refused() {
    // f32.const 1.0; i32.reinterpret_f32: a bit copy, which the native tier compiles. The
    // differential tests in float.rs check its values; this checks that it is not refused.
    let body = vec![0x43, 0x00, 0x00, 0x80, 0x3F, 0xBC, 0x0B];
    let validated = tpt_wasm_validate::validate(Module {
        types: vec![FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            body,
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    })
    .expect("validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("lowers");
    assert!(
        !matches!(
            compile(&verified.module().functions[0]),
            Err(JitError::Unsupported(_))
        ),
        "a bit reinterpretation is in the subset"
    );
}
