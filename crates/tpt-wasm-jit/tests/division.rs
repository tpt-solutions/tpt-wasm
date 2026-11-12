// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Compiled division must trap exactly where Micro traps, with the same trap.
//!
//! The operands come from a pool that holds the cases the trap rules turn on: zero, one,
//! minus one, and the two ends of the `i32` range. A case agrees only when the compiled code
//! returns the same value, or the same trap, as Micro.
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

use tpt_wasm_format::{Export, ExportDesc, Function, Module};
use tpt_wasm_jit::compile;
use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{FunctionType, ResultType, Trap, Value, ValueType};

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

    /// An operand: usually one of the edge values, sometimes an arbitrary one.
    fn operand(&mut self) -> i32 {
        const EDGES: [i32; 8] = [0, 1, -1, 2, i32::MIN, i32::MAX, i32::MIN + 1, -7];
        if self.below(3) == 0 {
            self.next() as u32 as i32
        } else {
            EDGES[self.below(EDGES.len() as u64) as usize]
        }
    }
}

fn sleb(mut value: i32) -> Vec<u8> {
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

fn module(params: usize, body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32; params]),
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
    }
}

/// `p0 op p1`, with `op` one of the four division operations.
fn two_operand(op: u8) -> (usize, Vec<u8>) {
    (2, vec![0x20, 0x00, 0x20, 0x01, op, 0x0B])
}

/// `p0 op k`, with the divisor a constant from the edge pool.
fn constant_divisor(op: u8, divisor: i32) -> (usize, Vec<u8>) {
    let mut body = vec![0x20, 0x00, 0x41];
    body.extend(sleb(divisor));
    body.extend([op, 0x0B]);
    (1, body)
}

/// The four operations: `i32.div_s` 6D, `div_u` 6E, `rem_s` 6F, `rem_u` 70.
const DIVISION_OPS: [u8; 4] = [0x6D, 0x6E, 0x6F, 0x70];

fn micro_outcome(module: &Module, args: &[i32]) -> Result<i32, Trap> {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("accepted");
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    let values: Vec<Value> = args.iter().map(|value| Value::I32(*value)).collect();
    match instance.call("run", values) {
        Ok(results) => match results.as_slice() {
            [Value::I32(result)] => Ok(*result),
            other => panic!("expected one i32, got {other:?}"),
        },
        Err(RuntimeError::Trap(trap)) => Err(trap),
        Err(other) => panic!("Micro failed in a way that is not a trap: {other:?}"),
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
fn compiled_division_traps_exactly_where_micro_does() {
    let mut rng = Lcg(0xD171_5104);
    let mut values = 0usize;
    let mut divide_by_zero = 0usize;
    let mut overflow = 0usize;
    let mut cases = Vec::new();
    for op in DIVISION_OPS {
        cases.push(two_operand(op));
        for divisor in [0, 1, -1, 2, i32::MIN, i32::MAX, -7] {
            cases.push(constant_divisor(op, divisor));
        }
    }
    for (params, body) in cases {
        let module = module(params, body);
        let validated = tpt_wasm_validate::validate(module.clone()).expect("validates");
        let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("lowers");
        let function = &verified.module().functions[0];
        let code = compile(function).expect("division is in the subset");
        for _ in 0..200 {
            let args: Vec<i32> = (0..params).map(|_| rng.operand()).collect();
            let expected = micro_outcome(&module, &args);
            let actual = code.call(&args).map_err(|error| match error {
                tpt_wasm_jit::JitError::Trap(trap) => trap,
                other => panic!("the compiled code failed in a way that is not a trap: {other}"),
            });
            assert_eq!(
                actual, expected,
                "args {args:?}: compiled division disagrees with Micro"
            );
            match &expected {
                Ok(_) => values += 1,
                Err(Trap::IntegerDivisionByZero) => divide_by_zero += 1,
                Err(Trap::IntegerOverflow) => overflow += 1,
                Err(other) => panic!("unexpected trap {other:?}"),
            }
        }
    }
    // Guard against a vacuous pass: each outcome must actually occur.
    assert!(values > 200, "values: {values}");
    assert!(divide_by_zero > 100, "division by zero: {divide_by_zero}");
    assert!(overflow > 10, "overflow: {overflow}");
}
