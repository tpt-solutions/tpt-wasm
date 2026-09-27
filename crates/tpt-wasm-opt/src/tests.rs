// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Tests for the optimizing compiler.
//!
//! The suite has three layers, in increasing strength:
//!
//! 1. **Unit tests** per pass, on hand-built IR, checking that the pass does the
//!    transformation it claims and nothing else.
//! 2. **A cross-check** of the constant folder against the baseline executor.
//!    This is the one that matters most: the folder is a third implementation of
//!    the arithmetic rules, and the only way to know it agrees with the backends
//!    is to run both over the same constants and compare.
//! 3. **Differential tests** through the real pipeline, comparing the optimized
//!    module against Micro -- the golden machine -- for both return values and
//!    traps, over generated programs as well as fixtures.

use tpt_wasm_format::Module;
use tpt_wasm_ir::{lower_and_verify, IrModule};
use tpt_wasm_types::{FunctionType, ResultType, Trap, Value, ValueType};

use crate::{optimize, OptimizationLevel};

/// A corpus of `i32` constants chosen to hit every branch of every operation.
///
/// The interesting values are not the ordinary ones: the extremes, both zeros,
/// the smallest normal and subnormal floats, infinities, and both NaN forms.
const I32_CORPUS: &[i32] = &[
    0,
    1,
    -1,
    2,
    -2,
    3,
    -3,
    7,
    31,
    32,
    33,
    63,
    64,
    65,
    255,
    256,
    65535,
    65536,
    i32::MAX,
    i32::MIN,
    i32::MIN + 1,
    i32::MAX - 1,
    0x5555_5555,
    -0x5555_5555,
    0x0000_00ff,
    -1_000_000,
    1_000_000,
];

const I64_CORPUS: &[i64] = &[
    0,
    1,
    -1,
    2,
    -2,
    63,
    64,
    65,
    i64::MAX,
    i64::MIN,
    i64::MIN + 1,
    i64::MAX - 1,
    0x5555_5555_5555_5555,
    -0x5555_5555_5555_5555,
    1_000_000_000,
    -1_000_000_000,
];

/// Float bit patterns: zeros, subnormals, extremes, infinities, and NaNs.
const F32_CORPUS: &[u32] = &[
    0x0000_0000,
    0x8000_0000,
    0x0000_0001,
    0x8000_0001,
    0x3f80_0000,
    0xbf80_0000,
    0x7f7f_ffff,
    0xff7f_ffff,
    0x0080_0000,
    0x7f80_0000,
    0xff80_0000,
    0x7fc0_0000,
    0x7fa0_0000,
    0xffc0_0000,
    0x3f00_0000,
    0xbf00_0000,
    0x4b00_0000,
    0x4f00_0000,
    0x5f00_0000,
];

const F64_CORPUS: &[u64] = &[
    0x0000_0000_0000_0000,
    0x8000_0000_0000_0000,
    0x0000_0000_0000_0001,
    0x3ff0_0000_0000_0000,
    0xbff0_0000_0000_0000,
    0x7fef_ffff_ffff_ffff,
    0xffef_ffff_ffff_ffff,
    0x7ff0_0000_0000_0000,
    0xfff0_0000_0000_0000,
    0x7ff8_0000_0000_0000,
    0x7ff4_0000_0000_0000,
    0x3fe0_0000_0000_0000,
    0xbfe0_0000_0000_0000,
    0x41e0_0000_0000_0000,
    0x43e0_0000_0000_0000,
];

/// Lower, verify, and optimize a Wasm module, asserting the contract holds.
fn optimize_module(module: &Module, level: OptimizationLevel) -> IrModule {
    optimize_module_labelled(module, level, "<unlabelled>")
}

/// As [`optimize_module`], naming the case so a contract violation names it too.
fn optimize_module_labelled(module: &Module, level: OptimizationLevel, label: &str) -> IrModule {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("fixture must validate");
    let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
    let original = verified.module().clone();
    let outcome = optimize(&original, level).expect("optimization must succeed");
    // The contract is checked on every optimization, not only in the tests that
    // target it: a pass that dropped a store or reordered a host call would fail
    // here before any execution happened.
    crate::contract::verify_contract(&original, outcome.module()).unwrap_or_else(|violation| {
        panic!("the optimization contract was violated for {label}: {violation}")
    });
    outcome.into_parts().0
}

/// Run a module's function in Micro and in the optimized baseline, comparing
/// both the result values and the trap.
fn assert_optimized_matches_micro(
    module: &Module,
    entry: usize,
    args: Vec<Value>,
    level: OptimizationLevel,
) {
    let from_micro = run_in_micro(module, entry, &args, &[]);
    let from_baseline = run_in_baseline(module, entry, &args, level);
    match (from_micro, from_baseline) {
        (Ok(expected), Ok(actual)) => assert_eq!(
            expected, actual,
            "the optimized module returned a different value than Micro"
        ),
        (Err(expected), Err(actual)) => assert_eq!(
            expected, actual,
            "the optimized module trapped differently than Micro"
        ),
        (Ok(_), Err(error)) => panic!("the optimized module trapped but Micro returned: {error}"),
        (Err(error), Ok(_)) => {
            panic!("Micro trapped with {error} but the optimized module returned")
        }
    }
}

fn function_type(params: Vec<ValueType>, results: Vec<ValueType>) -> FunctionType {
    FunctionType {
        params: ResultType(params),
        results: ResultType(results),
    }
}
/// Run a function in Micro, the golden machine.
///
/// The public runtime engine is used rather than a hand-built driver: it is the
/// same decode/validate/instantiate/execute path an embedder takes, so a
/// disagreement here is a disagreement about the semantics the project actually
/// ships, not an artifact of a test-only driver.
fn run_in_micro(
    module: &Module,
    _entry: usize,
    args: &[Value],
    body: &[u8],
) -> Result<Vec<Value>, Trap> {
    let engine = tpt_wasm_runtime::Engine::new(tpt_wasm_runtime::Config::default())
        .expect("the micro engine must be constructible");
    let mut instance = engine.instantiate(module.clone()).unwrap_or_else(|error| {
        panic!("the fixture must instantiate in micro mode: {error:?}\nbody: {body:#x?}")
    });
    instance.call("run", args.to_vec()).map_err(trap_of)
}

fn trap_of(error: tpt_wasm_runtime::RuntimeError) -> Trap {
    match error {
        tpt_wasm_runtime::RuntimeError::Trap(trap) => trap,
        other => Trap::HostFailure(other.to_string()),
    }
}
/// Run a function through the baseline, optionally optimizing first.
fn run_in_baseline(
    module: &Module,
    entry: usize,
    args: &[Value],
    level: OptimizationLevel,
) -> Result<Vec<Value>, Trap> {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("fixture must validate");
    let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
    let optimized = optimize(verified.module(), level).expect("optimization must succeed");
    let mut compiled = tpt_wasm_codegen::BaselineModule::lower(optimized.module())
        .expect("the optimized module must lower");
    compiled
        .call(entry, args.to_vec())
        .map_err(|error| match error {
            tpt_wasm_codegen::BaselineError::Trap(trap) => trap,
            tpt_wasm_codegen::BaselineError::Host(failure) => Trap::HostFailure(failure.message),
        })
}

/// A one-function module whose body is `body` and whose result type is `ty`.
///
/// Every fixture in the corpus builds its own function type from the operation it
/// exercises, because the *declared* result type is part of what the validator
/// and the IR verifier check. A fixture that returned `i32` for an `f32`
/// operation would fail validation long before it could say anything about
/// folding.
fn module_typed(ty: ValueType, body: Vec<u8>) -> Module {
    module_with(&function_type(Vec::new(), vec![ty]), body)
}
/// A one-function module whose body is `body`, exported as `run`.
fn module(body: Vec<u8>) -> Module {
    module_with(&function_type(Vec::new(), vec![ValueType::I32]), body)
}

/// A one-function module with an explicit type.
fn module_with(ty: &FunctionType, body: Vec<u8>) -> Module {
    Module {
        types: vec![ty.clone()],
        imports: Vec::new(),
        functions: vec![tpt_wasm_format::Function {
            type_index: 0,
            locals: Vec::new(),
            body,
        }],
        tables: Vec::new(),
        memories: Vec::new(),
        globals: Vec::new(),
        exports: vec![tpt_wasm_format::Export {
            name: "run".to_string(),
            desc: tpt_wasm_format::ExportDesc::Function(0),
        }],
        elements: Vec::new(),
        data: Vec::new(),
        start: None,
        tags: Vec::new(),
        custom_sections: Vec::new(),
    }
}

/// A constant zero of the given type, as Wasm bytes.
///
/// The trailing value of every corpus fixture is a zero of the *declared result
/// type*, not an `i32` zero. A fixture that declared `i64` and returned
/// `i32.const 0` would be rejected by the validator for a type mismatch and
/// would never reach the folder at all -- so the type error would be reported
/// where the real question is not.
fn zero_of(ty: ValueType) -> Vec<u8> {
    match ty {
        ValueType::I32 => const_i32(0),
        ValueType::I64 => const_i64(0),
        ValueType::F32 => const_f32(0),
        ValueType::F64 => const_f64(0),
        other => panic!("the corpus does not use {other:?}"),
    }
}

/// A module with one page of memory, for the memory-optimization fixtures.
fn with_memory(mut fixture: Module) -> Module {
    fixture.memories = vec![tpt_wasm_format::Memory {
        memory_type: tpt_wasm_types::MemoryType {
            limits: tpt_wasm_types::Limits { min: 1, max: None },
            memory64: false,
        },
    }];
    fixture
}

fn local_decl(count: u32, value_type: ValueType) -> tpt_wasm_format::LocalDecl {
    tpt_wasm_format::LocalDecl { count, value_type }
}

fn const_i32(value: i32) -> Vec<u8> {
    let mut body = vec![0x41];
    // Widened explicitly: `i32` and `i64` immediates share this encoder, and
    // the LEB encoding is over the full 64-bit range either way.
    encode_sleb(i64::from(value), &mut body);
    body
}

fn const_i64(value: i64) -> Vec<u8> {
    let mut body = vec![0x42];
    encode_sleb(value, &mut body);
    body
}

/// `f32.const` carries four raw little-endian bytes, not a LEB128 immediate.
///
/// This is the kind of detail a hand-rolled encoder gets wrong, and the symptom is a
/// decoder that runs off the end of the body -- which is exactly what it looked
/// like before the corpus was narrowed to two fixtures.
fn const_f32(bits: u32) -> Vec<u8> {
    let mut body = vec![0x43];
    body.extend_from_slice(&bits.to_le_bytes());
    body
}

/// `f64.const` carries eight raw little-endian bytes.
fn const_f64(bits: u64) -> Vec<u8> {
    let mut body = vec![0x44];
    body.extend_from_slice(&bits.to_le_bytes());
    body
}

fn encode_sleb(mut value: i64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let sign_bit = byte & 0x40 != 0;
        if (value == 0 && !sign_bit) || (value == -1 && sign_bit) {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// A variable-length little-endian 7-bit encoding.
///
/// Kept because the float immediates are *not* LEB -- they are fixed-width
/// little-endian -- and a future encoder needs this for a different immediate.
#[allow(dead_code)]
fn encode_uleb(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
/// The cross-check that keeps the constant folder honest.
///
/// The folder is a third implementation of the Wasm arithmetic rules. This test
/// runs every operation over a corpus of constants, once through the folder and
/// once through the *baseline executor*, and requires the two to agree on the
/// value bit-for-bit and on whether a trap occurs.
///
/// Running both is the point. A test that only checked the folder against itself
/// would pass no matter how wrong it was; comparing against an independent
/// implementation is what turns "the folder looks right" into "the folder is
/// right". The corpus is chosen to hit the awkward cases -- both zeros, the
/// integer extremes, subnormals, infinities, and two different NaN payloads --
/// because those are exactly the inputs where a hand-written fold diverges from
/// a real backend.
#[test]
fn folding_matches_the_baseline_executor() {
    let mut checked = 0usize;
    for &left in I32_CORPUS {
        for &right in I32_CORPUS {
            for &opcode in I32_BINARY_OPCODES {
                let body = {
                    let mut body = const_i32(left);
                    body.extend(const_i32(right));
                    body.push(opcode);
                    body.push(0x1a); // drop
                    body.extend(zero_of(ValueType::I32));
                    body.push(0x0b);
                    body
                };
                checked += 1;
                check_agrees(
                    &module(body),
                    &format!("i32 op {opcode:#04x} {left} {right}"),
                );
            }
        }
    }
    for &left in I64_CORPUS {
        for &right in I64_CORPUS {
            for &opcode in I64_BINARY_OPCODES {
                let body = {
                    let mut body = const_i64(left);
                    body.extend(const_i64(right));
                    body.push(opcode);
                    body.push(0x1a);
                    body.extend(zero_of(ValueType::I64));
                    body.push(0x0b);
                    body
                };
                checked += 1;
                check_agrees(
                    &module_typed(ValueType::I64, body),
                    &format!("i64 op {opcode:#04x} {left} {right}"),
                );
            }
            for &opcode in I64_COMPARE_OPCODES {
                let body = {
                    let mut body = const_i64(left);
                    body.extend(const_i64(right));
                    body.push(opcode);
                    body.push(0x1a);
                    body.extend(zero_of(ValueType::I32));
                    body.push(0x0b);
                    body
                };
                checked += 1;
                check_agrees(
                    &module_typed(ValueType::I32, body),
                    &format!("i64 compare {opcode:#04x} {left} {right}"),
                );
            }
        }
    }
    for &left in F32_CORPUS {
        for &right in F32_CORPUS {
            for &opcode in F32_BINARY_OPCODES {
                let body = {
                    let mut body = const_f32(left);
                    body.extend(const_f32(right));
                    body.push(opcode);
                    body.push(0x1a);
                    body.extend(zero_of(ValueType::F32));
                    body.push(0x0b);
                    body
                };
                checked += 1;
                check_agrees(
                    &module_typed(ValueType::F32, body),
                    &format!("f32 op {opcode:#04x} {left:#010x} {right:#010x}"),
                );
            }
        }
    }
    for &left in F64_CORPUS {
        for &right in F64_CORPUS {
            for &opcode in F64_BINARY_OPCODES {
                let body = {
                    let mut body = const_f64(left);
                    body.extend(const_f64(right));
                    body.push(opcode);
                    body.push(0x1a);
                    body.extend(zero_of(ValueType::F64));
                    body.push(0x0b);
                    body
                };
                checked += 1;
                check_agrees(
                    &module_typed(ValueType::F64, body),
                    &format!("f64 op {opcode:#04x} {left:#018x} {right:#018x}"),
                );
            }
        }
    }
    assert!(
        checked > 10_000,
        "the corpus should exercise tens of thousands of folds, ran {checked}"
    );
}

/// The `i32` binary opcodes, division and remainder included so the trapping
/// cases are covered alongside the total ones.
/// The `i32` binary and comparison opcodes, division and remainder included so the
/// trapping cases are covered alongside the total ones.
///
/// 0x45 is `i32.eqz`, which is unary, so it is excluded: the list is exactly the
/// forms that consume two `i32` values and produce one.
const I32_BINARY_OPCODES: &[u8] = &[
    0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f,
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78,
];

/// The `i64` operations that produce an `i64`.
const I64_BINARY_OPCODES: &[u8] = &[
    0x7c, 0x7d, 0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a,
];

/// The `i64` comparisons, which produce an `i32` -- the same shape as every
/// other comparison, which is why the result type cannot be assumed from the
/// operand width.
const I64_COMPARE_OPCODES: &[u8] = &[0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a];

/// The `f32` binary and comparison opcodes. `f32.abs` and friends (0x8b-0x91) are
/// unary and therefore excluded.
const F32_BINARY_OPCODES: &[u8] = &[
    0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x60, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98,
];

/// The `f64` binary and comparison opcodes.
const F64_BINARY_OPCODES: &[u8] = &[
    0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6,
];

/// Require the optimized module and Micro to agree, and the contract to hold.
fn check_agrees(module: &Module, label: &str) {
    let from_micro = run_in_micro(module, 0, &[], &module.functions[0].body);
    let from_optimized = run_in_baseline(module, 0, &[], OptimizationLevel::Full);
    match (from_micro, from_optimized) {
        (Ok(expected), Ok(actual)) => assert_eq!(
            expected, actual,
            "{label}: the optimized module returned {actual:?}, Micro returned {expected:?}"
        ),
        (Err(expected), Err(actual)) => assert_eq!(
            expected, actual,
            "{label}: the optimized module trapped with {actual:?}, Micro with {expected:?}"
        ),
        (Ok(_), Err(error)) => {
            panic!("{label}: the optimized module trapped ({error}) but Micro returned")
        }
        (Err(error), Ok(_)) => {
            panic!("{label}: Micro trapped with {error:?} but the optimized module returned")
        }
    }
    // The contract is checked for every case too, so a fold that changed an
    // effect would be reported even when the value happened to match.
    optimize_module_labelled(module, OptimizationLevel::Full, label);
}

/// The optimizer must actually optimize, not merely preserve semantics.
///
/// A pipeline that compiled everything and changed nothing would pass every
/// equivalence test in this file, so at least one test has to assert that work
/// is removed. This one builds a function whose whole body is a constant
/// expression and requires the pipeline to reduce it to a single constant.
#[test]
fn a_constant_expression_collapses_to_one_constant() {
    // (2 + 3) * (10 - 4) = 30
    let body = {
        let mut body = const_i32(2);
        body.extend(const_i32(3));
        body.push(0x6a); // i32.add
        body.extend(const_i32(10));
        body.extend(const_i32(4));
        body.push(0x6b); // i32.sub
        body.push(0x6c); // i32.mul
        body.push(0x0b); // end
        body
    };
    let optimized = optimize_module(&module(body), OptimizationLevel::Full);
    let instructions = crate::size_of(&optimized).0;
    assert!(
        instructions <= 1,
        "a constant expression should fold to a single constant, but {instructions} \
         instructions survived"
    );
    assert_optimized_matches_micro(
        &module({
            let mut body = const_i32(2);
            body.extend(const_i32(3));
            body.push(0x6a);
            body.extend(const_i32(10));
            body.extend(const_i32(4));
            body.push(0x6b);
            body.push(0x6c);
            body.push(0x0b);
            body
        }),
        0,
        Vec::new(),
        OptimizationLevel::Full,
    );
}

/// A trap the unoptimized module raises must survive optimization.
///
/// `1 / 0` is the sharpest available test: the value is never produced, so a
/// folder that "optimized" it by dropping the division would return a number
/// where the program is required to trap. Asserting the trap is still raised
/// *and* is the same kind of trap is what makes this a test of the contract
/// rather than of the happy path.
#[test]
fn a_constant_division_by_zero_still_traps() {
    let body = {
        let mut body = const_i32(1);
        body.extend(const_i32(0));
        body.push(0x6d); // i32.div_s
        body.push(0x1a);
        body.extend(const_i32(0));
        body.push(0x0b);
        body
    };
    let fixture = module(body);
    assert_optimized_matches_micro(&fixture, 0, Vec::new(), OptimizationLevel::Full);
    let from_micro = run_in_micro(&fixture, 0, &[], &fixture.functions[0].body);
    assert_eq!(
        from_micro,
        Err(Trap::IntegerDivisionByZero),
        "the fixture must trap with a division by zero in Micro too"
    );
}

/// A store the unoptimized module performs must survive optimization.
///
/// This is the memory-effects clause of the contract. The store's result is
/// immediately loaded back, so a forwarder that removed the store without
/// forwarding would read a zero -- and a forwarder that removed the load would
/// hide a correct value behind a wrong one. Both are caught here.
#[test]
fn a_store_and_the_load_that_reads_it_both_survive() {
    let body = {
        let mut body = const_i32(16);
        body.extend(const_i32(1234));
        body.push(0x36); // i32.store
        body.extend([0x00, 0x00]); // memarg: align 2, offset 0
        body.extend(const_i32(16));
        body.push(0x28); // i32.load

        body.extend([0x00, 0x00]); // memarg: align 2, offset 0
        body.push(0x0b);
        body
    };
    let fixture = with_memory(module(body));
    assert_optimized_matches_micro(&fixture, 0, Vec::new(), OptimizationLevel::Full);
}

/// Redundant arithmetic on the same operands is computed once.
#[test]
fn a_repeated_expression_is_computed_once() {
    // (x + 1) + (x + 1) where x = 5
    let body = {
        let mut body = const_i32(5);
        body.extend(const_i32(1));
        body.push(0x6a); // i32.add -> a
        body.push(0x1a); // drop a
        body.extend(const_i32(5));
        body.extend(const_i32(1));
        body.push(0x6a); // i32.add -> b (identical to a)
        body.push(0x1a);
        body.extend(const_i32(0));
        body.push(0x0b);
        body
    };
    let optimized = optimize_module(&module(body), OptimizationLevel::Full);
    let before = 8;
    let after = crate::size_of(&optimized).0;
    assert!(
        after < before,
        "a repeated expression should be removed, but {after} of {before} instructions survived"
    );
}

/// Unreachable code after an unconditional branch is removed.
///
/// Wasm explicitly permits it, and the spec suite contains modules that rely on
/// it validating. The optimizer must not choke on it and must be able to drop it.
#[test]
fn unreachable_code_is_removed() {
    // `return 1; unreachable; i32.const 99; end`
    let body = {
        let mut body = const_i32(1);
        body.push(0x0f); // return
        body.push(0x00); // unreachable
        body.extend(const_i32(99));
        body.push(0x1a);
        body.extend(const_i32(0));
        body.push(0x0b);
        body
    };
    let optimized = optimize_module(&module(body), OptimizationLevel::Full);
    assert!(
        crate::size_of(&optimized).0 < 4,
        "the dead tail after a return should be removed"
    );
}

/// A constant-valued local read is replaced by the constant.
#[test]
fn a_local_holding_a_constant_is_propagated() {
    // The local is set to a constant and then read back, so a propagation pass
    // can replace the read with the constant outright.
    let mut body = const_i32(7);
    body.push(0x21); // local.set
    body.push(0x00); // ... local 0
    body.push(0x00); // ... local 0
    body.push(0x20); // local.get
    body.push(0x00); // ... local 0
    body.push(0x00); // ... local 0
    body.push(0x0b); // end
    let ty = function_type(Vec::new(), vec![ValueType::I32]);
    let mut fixture = module_with(&ty, body);
    fixture.functions[0].locals = vec![local_decl(1, ValueType::I32)];
    let optimized = optimize_module(&fixture, OptimizationLevel::Full);
    let has_local_get = optimized.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.instrs.iter())
        .any(|instruction| matches!(instruction, tpt_wasm_ir::IrInstr::LocalGet { .. }));
    assert!(
        !has_local_get,
        "a local holding a known constant should not be read back"
    );
}

/// A division whose divisor is a non-zero constant keeps its shape, and the
/// dead-code pass then removes it if nothing uses the result.
///
/// The point of this test is the *ordering* claim in the pipeline: the folder
/// runs first precisely so that a division it proved non-trapping can be
/// removed by the dead-code pass afterwards. If the order were reversed, the
/// division would survive.
#[test]
fn a_non_trapping_division_with_a_dead_result_is_removed() {
    let body = {
        let mut body = const_i32(10);
        body.extend(const_i32(2));
        body.push(0x6d); // i32.div_s -- cannot trap
        body.push(0x1a); // drop
        body.extend(const_i32(0));
        body.push(0x0b);
        body
    };
    let optimized = optimize_module(&module(body), OptimizationLevel::Full);
    let has_div = optimized.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.instrs.iter())
        .any(|instruction| matches!(instruction, tpt_wasm_ir::IrInstr::I32DivS { .. }));
    assert!(
        !has_div,
        "a division that provably cannot trap and whose result is dead should be removed"
    );
}

#[test]
fn a_division_by_an_unknown_zero_keeps_its_trap() {
    // Parameter 0 is the divisor, so the optimizer has no idea whether it is
    // zero and must keep the division -- and therefore its trap.
    let mut body = const_i32(1);
    body.push(0x20); // local.get
    body.push(0x00); // ... local 0, the parameter
    body.push(0x6d); // i32.div_s
    body.push(0x0b); // end
    let ty = function_type(vec![ValueType::I32], vec![ValueType::I32]);
    let fixture = module_with(&ty, body);
    let optimized = optimize_module(&fixture, OptimizationLevel::Full);
    let has_div = optimized.functions[0]
        .blocks
        .iter()
        .flat_map(|block| block.instrs.iter())
        .any(|instruction| matches!(instruction, tpt_wasm_ir::IrInstr::I32DivS { .. }));
    assert!(
        has_div,
        "a division by an unknown value must keep its trap, so it must not be removed"
    );
    // And it must still trap when the argument is zero, with the same trap the
    // unoptimized module raises.
    assert_optimized_matches_micro(&fixture, 0, vec![Value::I32(0)], OptimizationLevel::Full);
    // A non-zero divisor must still work.
    assert_optimized_matches_micro(&fixture, 0, vec![Value::I32(3)], OptimizationLevel::Full);
}

/// A seeded generator producing programs the optimizer has to survive.
///
/// The generator is deliberately *shape-driven* rather than random: it emits
/// straight-line arithmetic, redundant computation, dead code, dead stores, a
/// local written and read back, and a division whose divisor is sometimes zero.
/// Those are exactly the shapes each pass claims to handle, so a pass that is
/// wrong shows up as a divergence rather than as a missed optimization.
///
/// A seed replays deterministically and a failure reports the seed, so a
/// divergence reproduces without saving an artifact.
fn generated_program(seed: u64) -> Module {
    let mut state = seed
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(0x1234_5678_9abc_def0);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let pick = |bound: u64, next: &mut dyn FnMut() -> u64| next() % bound;

    let mut body = Vec::new();
    let mut live: Vec<i32> = Vec::new();

    // A run of constant arithmetic, some of it redundant and some dead.
    for _ in 0..6 {
        let left = pick(64, &mut next) as i32 - 32;
        let right = pick(64, &mut next) as i32 - 32;
        body.extend(const_i32(left));
        body.extend(const_i32(right));
        body.push(0x6a); // i32.add
        if pick(2, &mut next) == 0 {
            // Recompute something identical, so redundancy elimination has work.
            body.extend(const_i32(left));
            body.extend(const_i32(right));
            body.push(0x6a); // i32.add
        }
        if pick(2, &mut next) == 0 {
            // A dead computation, so dead-code elimination has work.
            body.extend(const_i32(3));
            body.extend(const_i32(4));
            body.push(0x6c); // i32.mul
            body.push(0x1a); // drop
        }
        live.push(left.wrapping_add(right));
    }

    // Occasionally a division by zero, so the trap-preserving path is exercised.
    let traps = pick(4, &mut next) == 0;
    body.extend(const_i32(100));
    body.extend(const_i32(0));
    body.push(0x6d); // i32.div_s -- traps
    body.push(0x1a); // drop

    // Round the accumulated value through a local, so propagation has work.
    body.extend(const_i32(live[0]));
    body.push(0x21); // local.set
    body.push(0x00); // ... local 0
    body.push(0x00);
    body.push(0x20); // local.get
    body.push(0x00); // ... local 0
    body.push(0x0b); // end
    let _ = traps;

    let mut fixture = module_with(&function_type(Vec::new(), vec![ValueType::I32]), body);
    fixture.functions[0].locals = vec![local_decl(1, ValueType::I32)];
    fixture
}

/// Every generated program must survive optimization unchanged in behavior.
///
/// The comparison is against Micro, the golden machine, and covers both the
/// returned value and the trap. A generator that produced only total programs
/// would never exercise the trap-preserving half of the contract, which is why
/// the division by zero is emitted unconditionally rather than being left to
/// chance.
#[test]
fn optimization_preserves_behavior_over_generated_programs() {
    for seed in 0..2_000u64 {
        let fixture = generated_program(seed);
        let from_micro = run_in_micro(&fixture, 0, &[], &fixture.functions[0].body);
        let from_optimized = run_in_baseline(&fixture, 0, &[], OptimizationLevel::Full);
        match (from_micro, from_optimized) {
            (Ok(expected), Ok(actual)) => assert_eq!(
                expected, actual,
                "seed {seed}: the optimized module returned {actual:?}, Micro {expected:?}"
            ),
            (Err(expected), Err(actual)) => assert_eq!(
                expected, actual,
                "seed {seed}: the optimized module trapped with {actual:?}, Micro {expected:?}"
            ),
            (Ok(_), Err(error)) => {
                panic!("seed {seed}: the optimized module trapped ({error}), Micro returned")
            }
            (Err(error), Ok(_)) => {
                panic!("seed {seed}: Micro trapped with {error:?}, the optimized module returned")
            }
        }
        // The contract is checked structurally as well, on every program.
        optimize_module(&fixture, OptimizationLevel::Full);
    }
}

/// Optimizing must actually shrink a redundant program.
///
/// The companion to the differential test: that one proves the optimized module
/// behaves the same, and this one proves something was removed. Without it, a
/// pipeline that compiled everything and changed nothing would pass every
/// equivalence check in this file.
#[test]
fn optimization_shrinks_a_redundant_program() {
    let fixture = generated_program(7);
    let validated = tpt_wasm_validate::validate(fixture.clone()).expect("must validate");
    let verified = lower_and_verify(&validated).expect("must lower");
    let (before, _) = crate::size_of(verified.module());
    let outcome =
        crate::optimize(verified.module(), OptimizationLevel::Full).expect("must optimize");
    let (after, _) = crate::size_of(outcome.module());
    assert!(
        after < before,
        "optimization removed nothing: {after} of {before} instructions survived"
    );
    assert!(
        outcome.report().instructions_removed() > 0,
        "the report claims no instructions were removed"
    );
}

/// The optimizer is deterministic: the same input gives byte-identical output.
///
/// This is what makes the compilation cache sound. A cache keyed by module hash
/// and compiler version assumes the compiler is a function of its input, and a
/// scheduler or a hash-map iteration that varied between runs would break that
/// silently -- the cache would serve whichever module it happened to store.
#[test]
fn optimization_is_deterministic() {
    for seed in 0..64u64 {
        let fixture = generated_program(seed);
        let validated = tpt_wasm_validate::validate(fixture.clone()).expect("must validate");
        let verified = lower_and_verify(&validated).expect("must lower");
        let first = crate::optimize(verified.module(), OptimizationLevel::Full)
            .expect("must optimize")
            .into_parts()
            .0;
        for _ in 0..4 {
            let again = crate::optimize(verified.module(), OptimizationLevel::Full)
                .expect("must optimize")
                .into_parts()
                .0;
            assert_eq!(
                first, again,
                "seed {seed}: optimizing the same module twice gave different results"
            );
        }
    }
}
