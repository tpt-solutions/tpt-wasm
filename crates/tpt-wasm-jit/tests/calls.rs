// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Calls between compiled functions of one module must agree with Micro: recursion, mutual
//! recursion, traps raised deep in a call chain, the call-depth limit, and a caller of a function
//! the compiler refuses, which must itself be left to the baseline.
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
use tpt_wasm_jit::{compile_module, CompiledModule, JitError};
use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{
    FunctionType, Limits, MemoryType, ResourceLimits, ResultType, Trap, Value, ValueType,
};

/// A module with one `i32` signature for every function, all exported as `f<index>`.
///
/// `bodies` gives each function's body; `memory` adds one page of memory.
fn module(
    params: Vec<usize>,
    bodies: Vec<Vec<u8>>,
    results: Vec<ValueType>,
    memory: bool,
) -> Module {
    let types: Vec<FunctionType> = params
        .iter()
        .zip(&results)
        .map(|(count, result)| FunctionType {
            params: ResultType(vec![ValueType::I32; *count]),
            results: ResultType(vec![*result]),
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
        exports,
        ..Module::default()
    }
}

/// Compile every function of the module together, as the runtime does.
fn compiled(module: &Module, max_call_depth: u64) -> CompiledModule {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("the module validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the module lowers");
    compile_module(verified.module(), max_call_depth, true)
}

/// Micro's outcome for the call, as the compiled code reports it.
fn micro(module: &Module, index: usize, args: &[i32], max_call_depth: usize) -> Result<i32, Trap> {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        limits: ResourceLimits {
            max_call_depth,
            ..ResourceLimits::default()
        },
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    let values: Vec<Value> = args.iter().map(|value| Value::I32(*value)).collect();
    match instance.call(&format!("f{index}"), values) {
        Ok(results) => match results.as_slice() {
            [Value::I32(value)] => Ok(*value),
            other => panic!("expected one i32, got {other:?}"),
        },
        Err(RuntimeError::Trap(trap)) => Err(trap),
        Err(other) => panic!("Micro failed in a way that is not a trap: {other:?}"),
    }
}

/// The compiled outcome, mapped to Micro's terms.
fn native(code: &CompiledModule, index: usize, args: &[i32]) -> Result<i32, Trap> {
    let slots: Vec<i64> = args.iter().map(|value| i64::from(*value)).collect();
    match code.call(index, &slots, None) {
        Ok(bits) => Ok(bits as u32 as i32),
        Err(JitError::Trap(trap)) => Err(trap),
        Err(other) => panic!("the compiled code failed without a trap: {other}"),
    }
}

/// Compare every call of every exported function with Micro.
fn agree(
    module: &Module,
    code: &CompiledModule,
    calls: &[(usize, Vec<i32>)],
    max_call_depth: usize,
) -> (usize, usize) {
    let mut values = 0;
    let mut traps = 0;
    for (index, args) in calls {
        let expected = micro(module, *index, args, max_call_depth);
        let actual = native(code, *index, args);
        assert_eq!(actual, expected, "f{index}{args:?}");
        match expected {
            Ok(_) => values += 1,
            Err(_) => traps += 1,
        }
    }
    (values, traps)
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
fn recursive_fibonacci_agrees_with_micro() {
    // fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2)
    let body = vec![
        0x20, 0x00, 0x41, 0x02, 0x48, // local.get 0; i32.const 2; i32.lt_s
        0x04, 0x7F, 0x20, 0x00, // if (result i32) local.get 0
        0x05, 0x20, 0x00, 0x41, 0x01, 0x6B, 0x10, 0x00, // else: fib(n - 1)
        0x20, 0x00, 0x41, 0x02, 0x6B, 0x10, 0x00, 0x6A, // + fib(n - 2)
        0x0B, 0x0B, // end; end
    ];
    let module = module(vec![1], vec![body], vec![ValueType::I32], false);
    let code = compiled(&module, 1_000);
    assert!(code.is_compiled(0), "fib is compiled");
    let calls: Vec<(usize, Vec<i32>)> = (0..18).map(|n| (0, vec![n])).collect();
    let (values, _) = agree(&module, &code, &calls, 1_000);
    assert_eq!(values, calls.len());
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
fn mutually_recursive_functions_agree_with_micro() {
    // even(n) = n == 0 ? 1 : odd(n - 1); odd(n) = n == 0 ? 0 : even(n - 1)
    let even = vec![
        0x20, 0x00, 0x45, 0x04, 0x7F, 0x41, 0x01, // if (result i32) n == 0 then 1
        0x05, 0x20, 0x00, 0x41, 0x01, 0x6B, 0x10, 0x01, // else odd(n - 1)
        0x0B, 0x0B,
    ];
    let odd = vec![
        0x20, 0x00, 0x45, 0x04, 0x7F, 0x41, 0x00, // if (result i32) n == 0 then 0
        0x05, 0x20, 0x00, 0x41, 0x01, 0x6B, 0x10, 0x00, // else even(n - 1)
        0x0B, 0x0B,
    ];
    let module = module(vec![1, 1], vec![even, odd], vec![ValueType::I32; 2], false);
    let code = compiled(&module, 1_000);
    assert!(
        code.is_compiled(0) && code.is_compiled(1),
        "both are compiled"
    );
    let calls: Vec<(usize, Vec<i32>)> =
        (0..12).flat_map(|n| [(0, vec![n]), (1, vec![n])]).collect();
    let (values, _) = agree(&module, &code, &calls, 1_000);
    assert_eq!(values, calls.len());
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
fn a_trap_in_a_callee_reaches_the_caller_unchanged() {
    // f0(x, y) = 100 / y; f1(x, y) = f0(x, y) + 1; f2(x, y) = f1(x, y) * 2
    let divide = vec![0x41, 0x64, 0x20, 0x01, 0x6D, 0x0B];
    let plus_one = vec![0x20, 0x00, 0x20, 0x01, 0x10, 0x00, 0x41, 0x01, 0x6A, 0x0B];
    let doubled = vec![0x20, 0x00, 0x20, 0x01, 0x10, 0x01, 0x41, 0x02, 0x6C, 0x0B];
    let module = module(
        vec![2, 2, 2],
        vec![divide, plus_one, doubled],
        vec![ValueType::I32; 3],
        false,
    );
    let code = compiled(&module, 1_000);
    assert!(code.is_compiled(2), "the chain is compiled");
    let mut calls = Vec::new();
    for index in 0..3 {
        for x in [-7, 0, 1, 100] {
            for y in [-3, -1, 0, 1, 7] {
                calls.push((index, vec![x, y]));
            }
        }
    }
    let (values, traps) = agree(&module, &code, &calls, 1_000);
    assert!(values > 30, "values: {values}");
    assert!(traps > 10, "traps: {traps}");
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
fn the_call_depth_limit_traps_where_micro_does() {
    // down(n) = n == 0 ? 0 : down(n - 1) + 1, which activates n + 1 frames.
    let down = vec![
        0x20, 0x00, 0x45, 0x04, 0x7F, 0x41, 0x00, // if (result i32) n == 0 then 0
        0x05, 0x20, 0x00, 0x41, 0x01, 0x6B, 0x10, 0x00, 0x41, 0x01,
        0x6A, // else down(n - 1) + 1
        0x0B, 0x0B,
    ];
    for limit in [1usize, 2, 5, 9] {
        let module = module(vec![1], vec![down.clone()], vec![ValueType::I32], false);
        let code = compiled(&module, limit as u64);
        let calls: Vec<(usize, Vec<i32>)> = (0..12).map(|n| (0, vec![n])).collect();
        let (values, traps) = agree(&module, &code, &calls, limit);
        assert!(
            values > 0 && traps > 0,
            "limit {limit}: values {values}, traps {traps}"
        );
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
fn the_measured_stack_of_each_activation_fits_its_estimate() {
    // The budget is checked against the estimate, so the estimate must bound what the encoder
    // really reserves. Each case is compiled, and every compiled function is measured from its own
    // prologue and compared with the estimate computed from its IR.
    let lowered = |module: &Module| {
        let validated = tpt_wasm_validate::validate(module.clone()).expect("validates");
        tpt_wasm_ir::lower_and_verify(&validated)
            .expect("lowers")
            .module()
            .clone()
    };
    let fib = vec![
        0x20, 0x00, 0x41, 0x02, 0x48, 0x04, 0x7F, 0x20, 0x00, 0x05, 0x20, 0x00, 0x41, 0x01, 0x6B,
        0x10, 0x00, 0x20, 0x00, 0x41, 0x02, 0x6B, 0x10, 0x00, 0x6A, 0x0B, 0x0B,
    ];
    let mut caller = Vec::new();
    for _ in 0..200 {
        caller.extend([0x41, 0x01, 0x1A]);
    }
    caller.extend([0x10, 0x01, 0x0B]);
    // f0(a, b, c) = f1(a, b, c) + f1(a, b, c), with f1 = a + b + c.
    let three = vec![
        0x20, 0x00, 0x20, 0x01, 0x20, 0x02, 0x10, 0x01, 0x20, 0x00, 0x20, 0x01, 0x20, 0x02, 0x10,
        0x01, 0x6A, 0x0B,
    ];
    let sum = vec![0x20, 0x00, 0x20, 0x01, 0x6A, 0x20, 0x02, 0x6A, 0x0B];
    let cases = [
        module(vec![1], vec![fib], vec![ValueType::I32], false),
        module(vec![3, 3], vec![three, sum], vec![ValueType::I32; 2], false),
        module(
            vec![0, 0],
            vec![caller, vec![0x41, 0x03, 0x0B]],
            vec![ValueType::I32; 2],
            false,
        ),
    ];
    let mut measured_any = 0;
    let mut largest = 0;
    for case in &cases {
        let ir = lowered(case);
        for max_depth in [10u64, 1_000] {
            let code = compile_module(&ir, max_depth, true);
            for (index, function) in ir.functions.iter().enumerate() {
                let Some(measured) = code.measured_stack_bytes(index) else {
                    continue;
                };
                let estimate = tpt_wasm_jit::stack_estimate(function);
                assert!(
                    measured <= estimate,
                    "f{index} at depth {max_depth}: measured {measured}, estimated {estimate}"
                );
                measured_any += 1;
                largest = largest.max(measured);
            }
        }
    }
    // fib at both depths, the pair at both depths, and at the shallow depth the caller and its
    // leaf: nine measurements. Pinned, so a change in what is compiled shows up here.
    assert_eq!(measured_any, 9);
    // The caller holds two hundred values, so its frame is at least 1600 bytes of slots. If the
    // prologue were read wrongly, the largest measurement would not reach that, and the upper
    // bound above would be checking nothing.
    assert!(largest >= 1600, "largest measured frame: {largest}");
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
fn a_deep_caller_with_a_large_frame_is_left_to_the_baseline() {
    // f0 pushes and drops two hundred constants, then calls f1, which returns 3. A frame of f0
    // has a slot for each of those values, so a chain of its calls as deep as the limit outgrows
    // the native stack budget, and f0 is not compiled. The same f0 at a shallow limit is compiled.
    let mut caller = Vec::new();
    for _ in 0..200 {
        caller.extend([0x41, 0x01, 0x1A]); // i32.const 1; drop
    }
    caller.extend([0x10, 0x01, 0x0B]); // call 1; end
    let leaf = vec![0x41, 0x03, 0x0B];
    let module = module(
        vec![0, 0],
        vec![caller, leaf],
        vec![ValueType::I32; 2],
        false,
    );
    let deep = compiled(&module, 1_000);
    assert!(
        !deep.is_compiled(0),
        "the deep caller is left to the baseline"
    );
    assert!(deep.is_compiled(1), "a leaf is still compiled");
    let shallow = compiled(&module, 10);
    assert!(
        shallow.is_compiled(0),
        "the same caller is compiled at a shallow limit"
    );
    assert_eq!(native(&shallow, 0, &[]), Ok(3));
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
fn a_large_caller_does_not_hold_back_a_small_caller_that_never_reaches_it() {
    // f0 is the large caller: it pushes two hundred constants and calls the leaf f2. f1 is a small
    // caller of the same leaf. The stack bound is per caller, so only f0 is left to the baseline.
    let mut large = Vec::new();
    for _ in 0..200 {
        large.extend([0x41, 0x01, 0x1A]); // i32.const 1; drop
    }
    large.extend([0x10, 0x02, 0x0B]); // call 2; end
    let small = vec![0x10, 0x02, 0x0B]; // call 2; end
    let leaf = vec![0x41, 0x03, 0x0B];
    let module = module(
        vec![0, 0, 0],
        vec![large, small, leaf],
        vec![ValueType::I32; 3],
        false,
    );
    let compiled = compiled(&module, 1_000);
    assert!(
        !compiled.is_compiled(0),
        "the large caller is left to the baseline"
    );
    assert!(
        compiled.is_compiled(1),
        "the small caller is compiled, since it reaches only the leaf"
    );
    assert_eq!(native(&compiled, 1, &[]), Ok(3));
}

#[test]
fn a_caller_of_a_refused_function_is_not_compiled() {
    // f0() = memory.grow, which the compiler refuses; f1() = f0(); f2() = 7.
    let memory_grow = vec![0x41, 0x01, 0x40, 0x00, 0x0B]; // i32.const 1; memory.grow; end
    let calls_refused = vec![0x10, 0x00, 0x0B];
    let seven = vec![0x41, 0x07, 0x0B];
    let mut module = module(
        vec![0, 0, 0],
        vec![memory_grow, calls_refused, seven],
        vec![ValueType::I32; 3],
        true,
    );
    // A memory with no maximum is never reserved, so growing it is refused.
    module.memories[0].memory_type.limits.max = None;
    let code = compiled(&module, 1_000);
    assert!(!code.is_compiled(0), "memory.grow is refused");
    assert!(
        !code.is_compiled(1),
        "a caller of a refused function is left out"
    );
    assert!(
        code.is_compiled(2),
        "an unrelated function is still compiled"
    );
    assert_eq!(native(&code, 2, &[]), Ok(7));
}
