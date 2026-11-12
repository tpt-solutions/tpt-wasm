// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

// The generator is only used where code can run; elsewhere the test module is empty.
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
    allow(dead_code)
)]

//! The compiled code must agree with Micro, the golden machine, on every function it accepts.
//!
//! Each case is a generated straight-line `i32` function: a stack program of constants,
//! parameter reads, and the non-trapping integer operations, ending in one value. The
//! same module runs through Micro and through the JIT on the same arguments, and the
//! results must be equal.

use tpt_wasm_format::{Export, ExportDesc, Function, Module};
use tpt_wasm_jit::{compile, JitError};
use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

/// A small deterministic generator, so a failing case can be replayed from its seed.
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

    fn i32(&mut self) -> i32 {
        self.next() as u32 as i32
    }
}

/// The signed LEB128 encoding of an `i32`, as an `i32.const` immediate.
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

/// A body that leaves exactly one `i32` on the stack, built from a seed.
fn straight_line_body(seed: u64, params: usize) -> Vec<u8> {
    const BINARY: [u8; 6] = [0x6a, 0x6b, 0x6c, 0x71, 0x72, 0x73]; // add sub mul and or xor
    let mut rng = Lcg(seed);
    let mut body = Vec::new();
    let mut depth = 0usize;
    let steps = 1 + rng.below(14);
    for _ in 0..steps {
        let push = depth < 2 || rng.below(2) == 0;
        if push {
            // A parameter read when there is one; otherwise a constant.
            if params > 0 && rng.below(3) == 0 {
                body.push(0x20); // local.get
                body.push(rng.below(params as u64) as u8);
            } else {
                body.push(0x41); // i32.const
                body.extend(sleb(rng.i32()));
            }
            depth += 1;
        } else {
            body.push(BINARY[rng.below(BINARY.len() as u64) as usize]);
            depth -= 1;
        }
    }
    // Fold the stack down to one value.
    while depth > 1 {
        body.push(BINARY[rng.below(BINARY.len() as u64) as usize]);
        depth -= 1;
    }
    body.push(0x0b); // end
    body
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

/// The IR of the module's one function, as the compiler sees it.
fn lowered(module: &Module) -> tpt_wasm_ir::IrFunction {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("the case validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the case lowers");
    verified.module().functions[0].clone()
}

fn micro_result(module: &Module, args: &[i32]) -> i32 {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("accepted");
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    let values: Vec<Value> = args.iter().map(|value| Value::I32(*value)).collect();
    match instance.call("run", values).expect("runs").as_slice() {
        [Value::I32(result)] => *result,
        other => panic!("expected one i32, got {other:?}"),
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
fn compiled_code_agrees_with_micro_on_generated_straight_line_functions() {
    let mut compiled_cases = 0;
    for seed in 0..600u64 {
        for params in 0..=4 {
            let body = straight_line_body(seed.wrapping_mul(7919) + params as u64, params);
            let module = module(params, body);
            let function = lowered(&module);
            let code = compile(&function)
                .unwrap_or_else(|error| panic!("seed {seed}, {params} params: {error}"));
            let mut rng = Lcg(seed ^ 0x5eed);
            for _ in 0..3 {
                let args: Vec<i32> = (0..params).map(|_| rng.i32()).collect();
                let expected = micro_result(&module, &args);
                let actual = code.call(&args).expect("the arity matches");
                assert_eq!(
                    actual, expected,
                    "seed {seed}, {params} params, args {args:?}: compiled code disagrees with Micro"
                );
            }
            compiled_cases += 1;
        }
    }
    assert_eq!(
        compiled_cases,
        600 * 5,
        "every generated case was compiled and run"
    );
}

#[test]
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
fn more_parameters_than_registers_are_read_through_the_argument_pointer() {
    // Arguments are read from an array, so arity is not limited by the argument registers.
    let body = vec![0x20, 0x04, 0x0b]; // local.get 4: the fifth argument
    let function = lowered(&module(5, body));
    let code = compile(&function).expect("five parameters compile");
    assert_eq!(code.call(&[1, 2, 3, 4, 99]), Ok(99));
}

#[test]
fn a_call_with_the_wrong_arity_is_refused() {
    let body = vec![0x20, 0x00, 0x0b];
    let function = lowered(&module(1, body));
    let Ok(code) = compile(&function) else {
        // On a target without a code generator, compilation itself is refused.
        return;
    };
    assert_eq!(
        code.call(&[]),
        Err(JitError::ArityMismatch {
            expected: 1,
            actual: 0
        })
    );
}
