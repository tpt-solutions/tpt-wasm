// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

// The generators are used only where code can run; elsewhere the test is compiled out.
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

//! Compiled control flow must agree with Micro: loops, conditional branches, `select`,
//! `local.set`/`tee`, comparisons, and shifts, including shift counts past the operand width.
//!
//! Each case comes from a template with random constants and operations. The template
//! fixes the control shape, so every loop is bounded: a loop's trip count is a value masked
//! to 0..=15 before it is compared.

use tpt_wasm_format::{Export, ExportDesc, Function, LocalDecl, Module};
use tpt_wasm_jit::compile;
use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

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

/// `i32.const v`.
fn konst(value: i32) -> Vec<u8> {
    let mut out = vec![0x41];
    out.extend(sleb(value));
    out
}

/// One of the two-operand `i32` operations the templates choose from.
fn random_binary(rng: &mut Lcg) -> u8 {
    // add sub mul and or xor
    [0x6A, 0x6B, 0x6C, 0x71, 0x72, 0x73][rng.below(6) as usize]
}

fn module(params: usize, locals: u32, body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32; params]),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: if locals == 0 {
                Vec::new()
            } else {
                vec![LocalDecl {
                    count: locals,
                    value_type: ValueType::I32,
                }]
            },
            body,
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

/// A counted loop over `p0 & 15` iterations that accumulates `acc = acc * k op i`, with
/// `i` and `acc` as declared locals 1 and 2.
fn loop_program(rng: &mut Lcg) -> (usize, u32, Vec<u8>) {
    let k = rng.i32();
    let op = random_binary(rng);
    let mut body = vec![0x02, 0x40, 0x03, 0x40]; // block; loop
                                                 // if (p0 & 15) >= i, leave the loop: br_if 1
    body.extend(konst(15));
    body.extend([0x20, 0x00, 0x71]); // local.get 0; i32.and
    body.extend([0x20, 0x01, 0x4E]); // local.get 1; i32.ge_s
    body.extend([0x0D, 0x01]); // br_if 1
                               // acc = acc * k op i
    body.extend([0x20, 0x02]);
    body.extend(konst(k));
    body.push(0x6C); // mul
    body.extend([0x20, 0x01, op]);
    body.extend([0x21, 0x02]); // local.set 2
                               // i = i + 1
    body.extend([0x20, 0x01]);
    body.extend(konst(1));
    body.push(0x6A);
    body.extend([0x21, 0x01]);
    body.extend([0x0C, 0x00]); // br 0
    body.push(0x0B); // end loop
    body.push(0x0B); // end block
    body.extend([0x20, 0x02, 0x0B]); // local.get 2; end
    (1, 2, body)
}

/// `if (p0 < c) then (a op p1) else (b op p0)`, with an `i32` result.
fn branch_program(rng: &mut Lcg) -> (usize, u32, Vec<u8>) {
    let threshold = rng.i32();
    let a = rng.i32();
    let b = rng.i32();
    let then_op = random_binary(rng);
    let else_op = random_binary(rng);
    let mut body = vec![0x20, 0x00]; // local.get 0
    body.extend(konst(threshold));
    body.push(0x48); // i32.lt_s
    body.extend([0x04, 0x7F]); // if (result i32)
    body.extend([0x20, 0x01]);
    body.extend(konst(a));
    body.push(then_op);
    body.push(0x05); // else
    body.extend([0x20, 0x00]);
    body.extend(konst(b));
    body.push(else_op);
    body.push(0x0B); // end if
    body.push(0x0B); // end function
    (2, 0, body)
}

/// `select(p0, p1, p0 > k)`, unsigned, so the condition is not the sign of `p0`.
fn select_program(rng: &mut Lcg) -> (usize, u32, Vec<u8>) {
    let k = rng.i32();
    let mut body = vec![0x20, 0x00, 0x20, 0x01]; // p0, p1
    body.extend([0x20, 0x00]);
    body.extend(konst(k));
    body.push(0x4B); // i32.gt_u
    body.push(0x1B); // select
    body.push(0x0B);
    (2, 0, body)
}

/// `local.tee`: `t = p0 op k`, then `t * t`.
fn tee_program(rng: &mut Lcg) -> (usize, u32, Vec<u8>) {
    let k = rng.i32();
    let op = random_binary(rng);
    let mut body = vec![0x20, 0x00];
    body.extend(konst(k));
    body.push(op);
    body.extend([0x22, 0x01]); // local.tee 1
    body.extend([0x20, 0x01, 0x6C]); // local.get 1; i32.mul
    body.push(0x0B);
    (1, 1, body)
}

/// A shift by a count from 0 to 40, so counts at and past the width are exercised.
fn shift_program(rng: &mut Lcg) -> (usize, u32, Vec<u8>) {
    let count = rng.below(41) as i32;
    let op = [0x74u8, 0x75, 0x76][rng.below(3) as usize]; // shl, shr_s, shr_u
    let mut body = vec![0x20, 0x00];
    body.extend(konst(count));
    body.push(op);
    body.push(0x0B);
    (1, 0, body)
}

/// `eqz(p0) + (p0 <=u k)`, combining two comparison results.
fn comparison_program(rng: &mut Lcg) -> (usize, u32, Vec<u8>) {
    let k = rng.i32();
    let mut body = vec![0x20, 0x00, 0x45]; // local.get 0; i32.eqz
    body.extend([0x20, 0x00]);
    body.extend(konst(k));
    body.push(0x4D); // i32.le_u
    body.push(0x6A); // i32.add
    body.push(0x0B);
    (1, 0, body)
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

fn lowered(module: &Module) -> tpt_wasm_ir::IrFunction {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("the case validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the case lowers");
    verified.module().functions[0].clone()
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
fn compiled_control_flow_agrees_with_micro() {
    type Template = fn(&mut Lcg) -> (usize, u32, Vec<u8>);
    let templates: [(&str, Template); 6] = [
        ("loop", loop_program),
        ("branch", branch_program),
        ("select", select_program),
        ("tee", tee_program),
        ("shift", shift_program),
        ("comparison", comparison_program),
    ];
    let mut cases = 0;
    let mut distinct = std::collections::BTreeSet::new();
    for seed in 0..120u64 {
        for (name, template) in templates {
            let mut rng = Lcg(seed * 31 + name.len() as u64);
            let (params, locals, body) = template(&mut rng);
            let module = module(params, locals, body);
            let function = lowered(&module);
            let code =
                compile(&function).unwrap_or_else(|error| panic!("{name}, seed {seed}: {error}"));
            let mut args_rng = Lcg(seed ^ 0xA5A5);
            for _ in 0..4 {
                let args: Vec<i32> = (0..params).map(|_| args_rng.i32()).collect();
                let expected = micro_result(&module, &args);
                let actual = code.call(&args).expect("the arity matches");
                assert_eq!(
                    actual, expected,
                    "{name}, seed {seed}, args {args:?}: compiled code disagrees with Micro"
                );
                distinct.insert(actual);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 120 * 6 * 4, "every case was compiled and run");
    // Guard against a vacuous pass: if every case returned the same value, agreement would
    // prove nothing about the control flow.
    assert!(
        distinct.len() > 200,
        "the templates must produce varied results, got {}",
        distinct.len()
    );
}
