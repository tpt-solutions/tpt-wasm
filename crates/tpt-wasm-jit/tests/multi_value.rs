// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Functions with several results, called directly and through calls, must agree with Micro on every
//! result and on the traps. A function with two or more results writes them to the argument block of its
//! call, so these tests also check that they are compiled, not left to the baseline.
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
use tpt_wasm_jit::{compile_module, CompiledModule};
use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{FunctionType, ResourceLimits, ResultType, Trap, Value, ValueType};

/// A module whose functions have the given signatures, one per entry, all exported as `f<index>`.
fn module(signatures: &[(Vec<ValueType>, Vec<ValueType>)], bodies: Vec<Vec<u8>>) -> Module {
    let types: Vec<FunctionType> = signatures
        .iter()
        .map(|(params, results)| FunctionType {
            params: ResultType(params.clone()),
            results: ResultType(results.clone()),
        })
        .collect();
    let functions: Vec<Function> = bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| Function {
            type_index: index as u32,
            locals: Vec::new(),
            body,
        })
        .collect();
    let exports: Vec<Export> = (0..functions.len())
        .map(|index| Export {
            name: format!("f{index}"),
            desc: ExportDesc::Function(index as u32),
        })
        .collect();
    Module {
        types,
        functions,
        exports,
        ..Module::default()
    }
}

/// The module's functions, compiled together as the runtime compiles them.
fn compiled(module: &Module) -> CompiledModule {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("the module validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the module lowers");
    compile_module(verified.module(), 1_000, true)
}

/// Micro's results for `f<index>` on `args`, as words: an `i32` is its value's low 32 bits, zero-extended,
/// and a float is its bits.
fn micro(module: &Module, index: usize, args: &[Value]) -> Result<Vec<u64>, Trap> {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        limits: ResourceLimits::default(),
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    match instance.call(&format!("f{index}"), args.to_vec()) {
        Ok(values) => Ok(values.iter().map(word_of_value).collect()),
        Err(RuntimeError::Trap(trap)) => Err(trap),
        Err(other) => panic!("Micro failed in a way that is not a trap: {other:?}"),
    }
}

fn word_of_value(value: &Value) -> u64 {
    match value {
        Value::I32(value) => u64::from(*value as u32),
        Value::I64(value) => *value as u64,
        Value::F32(bits) => u64::from(*bits),
        Value::F64(bits) => *bits,
        other => panic!("a non-numeric result: {other:?}"),
    }
}

/// The words of `f<index>` on the same arguments, run natively, and the trap if it traps. The
/// argument words are the arguments' words, and the results are compared by their types' widths below.
fn native(code: &CompiledModule, index: usize, args: &[Value]) -> Result<Vec<u64>, Trap> {
    let words: Vec<i64> = args
        .iter()
        .map(|value| word_of_value(value) as i64)
        .collect();
    match code.call_with_host_results(index, &words, None, &mut [], None) {
        Ok(results) => Ok(results),
        Err(tpt_wasm_jit::JitError::Trap(trap)) => Err(trap),
        Err(other) => panic!("the compiled code failed without a trap: {other}"),
    }
}

/// A result word, masked to the width of its type: an `i32` has only its low 32 bits defined.
fn masked(value_type: ValueType, word: u64) -> u64 {
    match value_type {
        ValueType::I32 | ValueType::F32 => word & 0xFFFF_FFFF,
        _ => word,
    }
}

/// Compare every call with Micro, and check that the function runs natively.
fn agree(
    module: &Module,
    code: &CompiledModule,
    index: usize,
    results: &[ValueType],
    calls: &[Vec<Value>],
) -> usize {
    assert!(
        code.is_compiled(index),
        "f{index} must be compiled, not left to the baseline"
    );
    let mut values = 0;
    for args in calls {
        let expected = micro(module, index, args);
        let actual = native(code, index, args);
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(
                    expected.len(),
                    results.len(),
                    "f{index}{args:?}: result count"
                );
                for ((expected, actual), value_type) in expected.iter().zip(&actual).zip(results) {
                    assert_eq!(
                        masked(*value_type, *actual),
                        masked(*value_type, *expected),
                        "f{index}{args:?}: result words"
                    );
                }
                values += 1;
            }
            (Err(expected), Err(actual)) => assert_eq!(actual, expected, "f{index}{args:?}: trap"),
            (expected, actual) => panic!("f{index}{args:?}: Micro {expected:?}, native {actual:?}"),
        }
    }
    values
}

fn i32_pairs() -> Vec<Vec<Value>> {
    let edge = [0, 1, -1, 7, -5, 3, i32::MAX, i32::MIN];
    let mut pairs = Vec::new();
    for a in edge {
        for b in edge {
            pairs.push(vec![Value::I32(a), Value::I32(b)]);
        }
    }
    pairs
}

#[test]
fn a_pair_and_its_consumers_agree_with_micro() {
    let i32s = ValueType::I32;
    let signatures = vec![
        // f0(a, b) = (a + b, a - b)
        (vec![i32s, i32s], vec![i32s, i32s]),
        // f1(a, b) = (a + b) * (a - b): one result, from the two results of f0.
        (vec![i32s, i32s], vec![i32s]),
        // f2(a, b): forwards both results of f0.
        (vec![i32s, i32s], vec![i32s, i32s]),
        // f3(a) = (a, a, a): three results.
        (vec![i32s], vec![i32s, i32s, i32s]),
        // f4(a, b) = (a / b, 0): traps as division does, after the result is computed.
        (vec![i32s, i32s], vec![i32s, i32s]),
    ];
    let bodies = vec![
        // f0: local.get 0; local.get 1; i32.add; local.get 0; local.get 1; i32.sub; end
        vec![
            0x20, 0x00, 0x20, 0x01, 0x6A, 0x20, 0x00, 0x20, 0x01, 0x6B, 0x0B,
        ],
        // f1: local.get 0; local.get 1; call 0; i32.mul; end
        vec![0x20, 0x00, 0x20, 0x01, 0x10, 0x00, 0x6C, 0x0B],
        // f2: local.get 0; local.get 1; call 0; end
        vec![0x20, 0x00, 0x20, 0x01, 0x10, 0x00, 0x0B],
        // f3: local.get 0 three times; end
        vec![0x20, 0x00, 0x20, 0x00, 0x20, 0x00, 0x0B],
        // f4: local.get 0; local.get 1; i32.div_s; i32.const 0; end
        vec![0x20, 0x00, 0x20, 0x01, 0x6D, 0x41, 0x00, 0x0B],
    ];
    let module = module(&signatures, bodies);
    let code = compiled(&module);
    let pairs = i32_pairs();
    let single = vec![
        vec![Value::I32(0)],
        vec![Value::I32(7)],
        vec![Value::I32(-3)],
    ];
    let pair_results = [i32s, i32s];

    let mut checked = 0;
    checked += agree(&module, &code, 0, &pair_results, &pairs);
    checked += agree(&module, &code, 1, &[i32s], &pairs);
    checked += agree(&module, &code, 2, &pair_results, &pairs);
    checked += agree(&module, &code, 3, &[i32s, i32s, i32s], &single);
    checked += agree(&module, &code, 4, &pair_results, &pairs);
    // 64 pairs for each of f0, f1 and f2; for f4, 64 pairs less the 9 that trap (the zero divisors and
    // MIN / -1); and the 3 single arguments of f3: 64 * 3 + 55 + 3.
    assert_eq!(checked, 250, "the number of calls compared with Micro");
}
