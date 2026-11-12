// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_wasm_format::{Export, ExportDesc, Function, LocalDecl, Module};
use tpt_wasm_jit::{compile, JitError};
use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{FunctionType, ResultType, Trap, Value, ValueType};

/// Reads choices from the fuzzer's bytes. Past the end it returns zero, so every input
/// describes some program.
struct Bytes<'a> {
    data: &'a [u8],
    position: usize,
}

impl Bytes<'_> {
    fn byte(&mut self) -> u8 {
        let value = self.data.get(self.position).copied().unwrap_or(0);
        self.position += 1;
        value
    }

    fn word(&mut self) -> i32 {
        i32::from_le_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }

    fn exhausted(&self) -> bool {
        self.position >= self.data.len()
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

/// The two-operand `i32` operations, as opcodes: arithmetic, division, shifts, comparisons.
const BINARY: [u8; 21] = [
    0x6A, 0x6B, 0x6C, 0x71, 0x72, 0x73, // add sub mul and or xor
    0x6D, 0x6E, 0x6F, 0x70, // div_s div_u rem_s rem_u
    0x74, 0x75, 0x76, // shl shr_s shr_u
    0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, // eq ne lt_s lt_u gt_s gt_u le_s le_u
];

/// Build a function body that is a stack program over the parameters and locals.
///
/// The stack discipline is tracked so that a program is well formed by construction. A
/// program that still fails validation for another reason is skipped by the caller.
fn program(bytes: &mut Bytes<'_>, params: usize, locals: usize) -> Vec<u8> {
    let variables = params + locals;
    let mut body = Vec::new();
    let mut depth = 0usize;
    let steps = bytes.byte() % 40;
    for _ in 0..steps {
        if bytes.exhausted() {
            break;
        }
        match bytes.byte() % 10 {
            0..=2 => {
                body.push(0x41);
                body.extend(sleb(bytes.word()));
                depth += 1;
            }
            3 if variables > 0 => {
                body.push(0x20);
                body.push(u32::from(bytes.byte()).rem_euclid(variables as u32) as u8);
                depth += 1;
            }
            4 if depth >= 2 => {
                body.push(BINARY[bytes.byte() as usize % BINARY.len()]);
                depth -= 1;
            }
            5 if depth >= 1 => {
                body.push(0x45); // i32.eqz
            }
            6 if depth >= 3 => {
                body.push(0x1B); // select
                depth -= 2;
            }
            7 if depth >= 1 && variables > 0 => {
                body.push(0x21); // local.set
                body.push(u32::from(bytes.byte()).rem_euclid(variables as u32) as u8);
                depth -= 1;
            }
            8 if depth >= 1 && variables > 0 => {
                body.push(0x22); // local.tee
                body.push(u32::from(bytes.byte()).rem_euclid(variables as u32) as u8);
            }
            9 if depth >= 1 => {
                body.push(0x1A); // drop
                depth -= 1;
            }
            _ => {}
        }
    }
    // Fold whatever remains down to one value, and make sure there is one.
    if depth == 0 {
        body.push(0x41);
        body.extend(sleb(0));
        depth = 1;
    }
    while depth > 1 {
        body.push(BINARY[bytes.byte() as usize % BINARY.len()]);
        depth -= 1;
    }
    body.push(0x0B);
    body
}

fn module(params: usize, locals: usize, body: Vec<u8>) -> Module {
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
                    count: locals as u32,
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

fn micro(module: &Module, args: &[i32]) -> Result<i32, Trap> {
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

fuzz_target!(|data: &[u8]| {
    let mut bytes = Bytes { data, position: 0 };
    let params = usize::from(bytes.byte() % 5);
    let locals = usize::from(bytes.byte() % 3);
    let body = program(&mut bytes, params, locals);
    let module = module(params, locals, body);

    // Programs that do not validate are not the compiler's input, so they are skipped.
    let Ok(validated) = tpt_wasm_validate::validate(module.clone()) else {
        return;
    };
    let verified = tpt_wasm_ir::lower_and_verify(&validated)
        .expect("a validated program lowers and verifies");
    let function = &verified.module().functions[0];

    // Every program here is inside the subset, so the compiler must accept it.
    let code = match compile(function) {
        Ok(code) => code,
        Err(JitError::UnsupportedTarget) => return,
        Err(error) => panic!("the compiler refused an in-subset program: {error}"),
    };
    for _ in 0..4 {
        let args: Vec<i32> = (0..params).map(|_| bytes.word()).collect();
        let expected = micro(&module, &args);
        let actual = match code.call(&args) {
            Ok(value) => Ok(value),
            Err(JitError::Trap(trap)) => Err(trap),
            Err(error) => panic!("the compiled code failed without a trap: {error}"),
        };
        if actual != expected {
            panic!("args {args:?}: compiled code gives {actual:?}, Micro gives {expected:?}");
        }
    }
});
