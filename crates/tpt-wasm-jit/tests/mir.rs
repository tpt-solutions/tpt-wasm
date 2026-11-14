// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The machine IR must compute what Micro computes.
//!
//! Each case is a random valid module: a function of two `i32` parameters with two more `i32`
//! locals, whose body is a random expression over constants, the parameters, locals, arithmetic,
//! comparisons, `select`, and `if`/`else` with values. The module is validated, lowered through
//! the verified IR, and lowered again to machine IR. The machine IR is then interpreted, and its
//! result must equal Micro's on the same arguments, for several argument pairs, so both branches
//! of each `if` are exercised.

use tpt_wasm_format::{Export, ExportDesc, Function, LocalDecl, Module};
use tpt_wasm_jit::mir;
use tpt_wasm_runtime::{Config, Engine};
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
        match self.below(6) {
            0 => 0,
            1 => -1,
            2 => i32::MIN,
            3 => i32::MAX,
            _ => self.next() as u32 as i32,
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

/// Append code that pushes one `i32`. Locals 0 and 1 are the parameters; 2 and 3 are declared.
fn expression(rng: &mut Lcg, depth: u32, out: &mut Vec<u8>) {
    if depth == 0 || rng.below(4) == 0 {
        if rng.below(2) == 0 {
            out.extend([0x20, rng.below(4) as u8]);
        } else {
            out.push(0x41);
            out.extend(sleb(rng.i32()));
        }
        return;
    }
    match rng.below(8) {
        0 | 1 => {
            expression(rng, depth - 1, out);
            expression(rng, depth - 1, out);
            const BINARY: [u8; 9] = [0x6a, 0x6b, 0x6c, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76];
            out.push(BINARY[rng.below(BINARY.len() as u64) as usize]);
        }
        2 => {
            expression(rng, depth - 1, out);
            expression(rng, depth - 1, out);
            // i32.eq through i32.ge_u.
            out.push(0x46 + rng.below(10) as u8);
        }
        3 => {
            expression(rng, depth - 1, out);
            out.push(0x45); // i32.eqz
        }
        4 => {
            expression(rng, depth - 1, out);
            expression(rng, depth - 1, out);
            expression(rng, depth - 1, out);
            out.push(0x1b); // select
        }
        5 => {
            expression(rng, depth - 1, out);
            out.extend([0x04, 0x7f]); // if (result i32)
            expression(rng, depth - 1, out);
            out.push(0x05); // else
            expression(rng, depth - 1, out);
            out.push(0x0b); // end
        }
        6 => {
            expression(rng, depth - 1, out);
            out.extend([0x22, 2 + rng.below(2) as u8]); // local.tee
        }
        _ => {
            expression(rng, depth - 1, out);
            let local = 2 + rng.below(2) as u8;
            out.extend([0x21, local]); // local.set
            out.extend([0x20, local]); // local.get
        }
    }
}

fn module_of(body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32, ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: vec![LocalDecl {
                count: 2,
                value_type: ValueType::I32,
            }],
            body,
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

#[test]
fn the_machine_ir_agrees_with_micro_on_random_control_flow() {
    let cases = 300;
    let mut rng = Lcg(0x5eed_1001);
    let mut runs = 0usize;
    let mut distinct = std::collections::BTreeSet::new();
    for case in 0..cases {
        let mut body = Vec::new();
        expression(&mut rng, 4, &mut body);
        body.push(0x0b);
        let module = module_of(body);

        let validated = tpt_wasm_validate::validate(module.clone())
            .unwrap_or_else(|error| panic!("case {case}: the module must validate: {error:?}"));
        let verified = tpt_wasm_ir::lower_and_verify(&validated)
            .unwrap_or_else(|error| panic!("case {case}: the IR must lower: {error:?}"));
        let function = &verified.module().functions[0];
        let machine = mir::lower(function)
            .unwrap_or_else(|error| panic!("case {case}: the machine IR must lower: {error:?}"));

        let engine = Engine::new(Config::default()).expect("the engine is constructible");
        let mut instance = engine
            .instantiate(module)
            .expect("a generated module instantiates");

        for _ in 0..4 {
            let a = rng.i32();
            let b = rng.i32();
            let expected = match instance
                .call("run", vec![Value::I32(a), Value::I32(b)])
                .expect("the function returns")
                .as_slice()
            {
                [Value::I32(value)] => *value,
                other => panic!("case {case}: Micro returned {other:?}"),
            };
            let actual = mir::run(&machine, &[a, b], 1_000_000)
                .unwrap_or_else(|error| panic!("case {case}: machine IR trapped: {error:?}"));
            assert_eq!(
                actual,
                vec![expected],
                "case {case}: machine IR disagrees with Micro on ({a}, {b})"
            );
            distinct.insert(expected);
            runs += 1;
        }
    }
    assert!(
        distinct.len() > 50,
        "the expressions must produce varied results, got {} distinct",
        distinct.len()
    );
    eprintln!("machine IR: {runs} runs over {cases} modules agree with Micro");
}
