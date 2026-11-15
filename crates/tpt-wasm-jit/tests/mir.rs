// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The machine IR, and the native code generated from it, must compute what Micro computes.
//!
//! Each case is a random valid module: a function of two `i32` parameters with two more `i32`
//! locals, whose body is a random expression over constants, the parameters, locals, arithmetic
//! (including division and remainder, which trap), comparisons, `select`, and `if`/`else` with
//! values. The module is validated, lowered through the verified IR, and lowered again to machine
//! IR. Micro, the machine IR, and (on x86-64) native code must then agree on each argument pair:
//! on the value, or on the trap. Several pairs per module exercise both branches of each `if`.
//!
//! Directed tests pin the edge cases the WebAssembly rules name, and the calls between the
//! functions of a module, including recursion up to the call-depth limit.

use std::collections::BTreeSet;

use tpt_wasm_format::{Export, ExportDesc, Function, LocalDecl, Memory, Module};
use tpt_wasm_jit::mir::{self, Precision};
use tpt_wasm_runtime::{Config, Engine, RuntimeError};
use tpt_wasm_types::{FunctionType, Limits, MemoryType, ResultType, Trap, Value, ValueType};

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
use tpt_wasm_jit::LinearMemory;

/// One page of linear memory, which every generated module declares.
const PAGE: usize = 65_536;

/// The most the test memories may grow to: four pages, which is also each module's maximum, so
/// Micro and the native reservation agree on where growth fails.
const CAPACITY: usize = 4 * PAGE;

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
    match rng.below(9) {
        0 | 1 => {
            expression(rng, depth - 1, out);
            expression(rng, depth - 1, out);
            // i32.add, sub, mul, the four division operators (which trap), the bitwise, shift,
            // and rotate operators.
            const BINARY: [u8; 15] = [
                0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77,
                0x78,
            ];
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
            // i32.eqz, i32.clz, i32.ctz, i32.popcnt.
            const UNARY: [u8; 4] = [0x45, 0x67, 0x68, 0x69];
            out.push(UNARY[rng.below(UNARY.len() as u64) as usize]);
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
        7 => {
            // A store of a value at an address masked into the first 256 bytes, then a load from
            // a constant address there. The load sees earlier stores, and it never leaves memory.
            expression(rng, depth - 1, out); // the store's address
            out.extend([0x41, 0xFF, 0x01, 0x71]); // i32.const 255; i32.and
            expression(rng, depth - 1, out); // the value
            const STORES: [u8; 3] = [0x36, 0x3a, 0x3b]; // i32.store, store8, store16
            out.extend([STORES[rng.below(STORES.len() as u64) as usize], 0x00, 0x00]);
            out.push(0x41); // the load's address
            out.extend(sleb(rng.below(256) as i32));
            const LOADS: [u8; 5] = [0x28, 0x2c, 0x2d, 0x2e, 0x2f]; // i32.load, 8_s, 8_u, 16_s, 16_u
            out.push(LOADS[rng.below(LOADS.len() as u64) as usize]);
            out.extend([0x00, rng.below(8) as u8]); // align 0, offset
        }
        _ => {
            expression(rng, depth - 1, out);
            let local = 2 + rng.below(2) as u8;
            out.extend([0x21, local]); // local.set
            out.extend([0x20, local]); // local.get
        }
    }
}

/// A module of functions that all take two `i32`s, return one, and declare two more locals. The
/// bodies are in index order, each ending with its `end` opcode; function 0 is exported as `run`.
/// The module has one memory of one page, with four pages as its maximum.
fn module_with(bodies: Vec<Vec<u8>>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32, ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: bodies
            .into_iter()
            .map(|body| Function {
                type_index: 0,
                locals: vec![LocalDecl {
                    count: 2,
                    value_type: ValueType::I32,
                }],
                body,
            })
            .collect(),
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(4),
                },
                memory64: false,
            },
        }],
        ..Module::default()
    }
}

/// A module with one function, `run`, whose body is `body`.
fn module_of(body: Vec<u8>) -> Module {
    module_with(vec![body])
}

/// A `sleb128` encoding of a 64-bit constant.
fn sleb64(mut value: i64) -> Vec<u8> {
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

impl Lcg {
    fn i64(&mut self) -> i64 {
        match self.below(6) {
            0 => 0,
            1 => -1,
            2 => i64::MIN,
            3 => i64::MAX,
            _ => ((self.next() << 33) ^ self.next()) as i64,
        }
    }
}

/// Append code that pushes one `i64`. Locals 0 and 1 are the parameters; 2 and 3 are declared. The
/// expression uses no conversions: a comparison's `i32` result only ever chooses between `i64`s.
fn wide_expression(rng: &mut Lcg, depth: u32, out: &mut Vec<u8>) {
    if depth == 0 || rng.below(4) == 0 {
        if rng.below(2) == 0 {
            out.extend([0x20, rng.below(4) as u8]);
        } else {
            out.push(0x42); // i64.const
            out.extend(sleb64(rng.i64()));
        }
        return;
    }
    match rng.below(9) {
        7 => {
            wide_expression(rng, depth - 1, out);
            out.push(0x79 + rng.below(3) as u8); // i64.clz, ctz, popcnt
        }
        6 => {
            // A store of an expression's value at a constant address, then a load from it. The
            // store's width and the load's extension vary, so every width and extension is exercised.
            const STORES: [u8; 4] = [0x37, 0x3C, 0x3D, 0x3E]; // i64.store, store8, store16, store32
            const LOADS: [u8; 7] = [0x29, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35];
            let address = rng.below(256) as i32;
            out.push(0x41); // i32.const: the store's address
            out.extend(sleb(address));
            wide_expression(rng, depth - 1, out);
            out.extend([STORES[rng.below(STORES.len() as u64) as usize], 0x00, 0x00]);
            out.push(0x41); // i32.const: the load's address
            out.extend(sleb(address));
            out.extend([LOADS[rng.below(LOADS.len() as u64) as usize], 0x00, 0x00]);
        }
        0..=2 => {
            wide_expression(rng, depth - 1, out);
            wide_expression(rng, depth - 1, out);
            // i64.add through i64.rotr: 0x7c to 0x8a, which include the trapping divisions.
            out.push(0x7C + rng.below(15) as u8);
        }
        3 => {
            // select(a, b, a comparison): the comparison is an `i32` that picks one of the two.
            wide_expression(rng, depth - 1, out);
            wide_expression(rng, depth - 1, out);
            wide_expression(rng, depth - 1, out);
            wide_expression(rng, depth - 1, out);
            out.push(0x51 + rng.below(10) as u8); // i64.eq through i64.ge_u
            out.push(0x1b); // select
        }
        4 => {
            // if (result i64) with an `i32` condition from a comparison.
            wide_expression(rng, depth - 1, out);
            wide_expression(rng, depth - 1, out);
            out.push(0x51 + rng.below(10) as u8);
            out.extend([0x04, 0x7e]); // if (result i64)
            wide_expression(rng, depth - 1, out);
            out.push(0x05); // else
            wide_expression(rng, depth - 1, out);
            out.push(0x0b); // end
        }
        5 => {
            wide_expression(rng, depth - 1, out);
            out.extend([0x22, 2 + rng.below(2) as u8]); // local.tee
        }
        _ => {
            wide_expression(rng, depth - 1, out);
            let local = 2 + rng.below(2) as u8;
            out.extend([0x21, local]); // local.set
            out.extend([0x20, local]); // local.get
        }
    }
}

/// A module of one function, `run`, taking two `i64`s and returning one, with two `i64` locals. It
/// has one memory and no table.
fn wide_module_of(body: Vec<u8>) -> Module {
    typed_module_of(ValueType::I64, body)
}

/// A module of one function, `run`, taking two values of type `kind` and returning one, with two
/// locals of that type. It has one memory and no table.
fn typed_module_of(kind: ValueType, body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![kind, kind]),
            results: ResultType(vec![kind]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: vec![LocalDecl {
                count: 2,
                value_type: kind,
            }],
            body,
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(4),
                },
                memory64: false,
            },
        }],
        ..Module::default()
    }
}

/// The Micro value of a word of type `kind`: an `i64`, or an `f64` from its bits.
fn word_value(kind: ValueType, word: i64) -> Value {
    match kind {
        ValueType::I32 => Value::I32(word as i32),
        ValueType::F64 => Value::F64(word as u64),
        ValueType::F32 => Value::F32(word as u32),
        _ => Value::I64(word),
    }
}

/// The word of a Micro value of a word type: an `i64`, or the bits of an `f64`.
fn value_word(value: &Value) -> Option<i64> {
    match value {
        Value::I64(word) => Some(*word),
        Value::F64(bits) => Some(*bits as i64),
        Value::F32(bits) => Some(i64::from(*bits)),
        Value::I32(value) => Some(i64::from(*value)),
        _ => None,
    }
}

/// As [`three_ways`], for a module whose `run` takes and returns `i64`s. Each argument pair is run by
/// Micro, the machine IR and native code, and all three must agree, on the value or on the trap.
fn three_ways64(label: &str, module: Module, pairs: &[(i64, i64)]) -> Vec<Result<i64, Trap>> {
    three_ways_typed(label, module, pairs, ValueType::I64)
}

/// As [`three_ways64`], for a module whose `run` takes and returns words of type `kind`: `i64`s, or
/// `f64`s by their bits.
fn three_ways_typed(
    label: &str,
    module: Module,
    pairs: &[(i64, i64)],
    kind: ValueType,
) -> Vec<Result<i64, Trap>> {
    three_ways_io(label, module, pairs, kind, kind, &[])
}

/// The result word a result of type `result` is compared as: an `i32` or `f32` is its low 32 bits, since
/// the upper half of such a word is not defined; any other word is compared whole.
fn result_word(result: ValueType, word: i64) -> i64 {
    match result {
        ValueType::I32 | ValueType::F32 => word & 0xFFFF_FFFF,
        _ => word,
    }
}

/// As [`three_ways_typed`], for a module whose `run` takes two values of type `arg` and returns one of
/// type `result`, the conversions being the case that needs the two to differ.
fn three_ways_io(
    label: &str,
    module: Module,
    pairs: &[(i64, i64)],
    arg: ValueType,
    result: ValueType,
    globals: &[u64],
) -> Vec<Result<i64, Trap>> {
    let validated = tpt_wasm_validate::validate(module.clone())
        .unwrap_or_else(|error| panic!("{label}: the module must validate: {error:?}"));
    let verified = tpt_wasm_ir::lower_and_verify(&validated)
        .unwrap_or_else(|error| panic!("{label}: the IR must lower: {error:?}"));
    let limit = Config::default().limits.max_call_depth as u64;
    let machines: Vec<mir::MirFunction> = verified
        .module()
        .functions
        .iter()
        .map(|function| {
            mir::lower(function)
                .unwrap_or_else(|error| panic!("{label}: the machine IR must lower: {error:?}"))
        })
        .collect();
    let indirect = mir::Indirect::for_module(verified.module());
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let native = tpt_wasm_jit::compile_module(verified.module(), limit, true);

    let engine = Engine::new(Config::default()).expect("the engine is constructible");
    let mut instance = engine
        .instantiate(module)
        .expect("a generated module instantiates");

    let mut memory = vec![0_u8; PAGE];
    let mut machine_globals = globals.to_vec();
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let mut native_memory = vec![0_u8; CAPACITY];
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let mut native_length = PAGE;
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let mut native_globals = globals.to_vec();

    let mut outcomes = Vec::new();
    for &(a, b) in pairs {
        let expected: Result<i64, Trap> =
            match instance.call("run", vec![word_value(arg, a), word_value(arg, b)]) {
                Ok(values) => match values.as_slice() {
                    [value] => Ok(result_word(
                        result,
                        value_word(value)
                            .unwrap_or_else(|| panic!("{label}: Micro returned {value:?}")),
                    )),
                    other => panic!("{label}: Micro returned {other:?}"),
                },
                Err(RuntimeError::Trap(trap)) => Err(trap),
                Err(other) => panic!("{label}: Micro failed without a trap: {other}"),
            };
        let actual: Result<i64, Trap> = match mir::run(
            &machines,
            0,
            &[a, b],
            &mut memory,
            CAPACITY,
            &mut machine_globals,
            None,
            &indirect,
            limit,
            1_000_000,
        ) {
            Ok(values) => match values.as_slice() {
                [value] => Ok(result_word(result, *value)),
                other => panic!("{label}: machine IR returned {other:?}"),
            },
            Err(mir::MirError::Trap(trap)) => Err(trap),
            Err(mir::MirError::HostFailed) => panic!("{label}: a host call without a host"),
        };
        assert_eq!(
            actual, expected,
            "{label}: machine IR disagrees with Micro on ({a}, {b})"
        );
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ))]
        {
            let mut linear = LinearMemory {
                bytes: &mut native_memory,
                len: native_length as u64,
            };
            let outcome =
                native.call_with_memory(0, &[a, b], Some(&mut linear), &mut native_globals);
            native_length = linear.len as usize;
            let compiled: Result<i64, Trap> = match outcome {
                Ok(bits) => Ok(result_word(result, bits as i64)),
                Err(tpt_wasm_jit::JitError::Trap(trap)) => Err(trap),
                Err(other) => panic!("{label}: native call failed: {other:?}"),
            };
            assert_eq!(
                compiled, expected,
                "{label}: native code disagrees with Micro on ({a}, {b})"
            );
        }
        outcomes.push(expected);
    }
    outcomes
}

/// Run `module`'s `run` function on each argument pair three ways: Micro, the machine IR, and, on
/// x86-64, native code generated from the machine IR. All three must agree on each pair, as a value
/// or as the trap raised. Returns each pair's outcome.
///
/// The machine IR and the native code run every function of the module, so calls between them are
/// checked too. The call-depth limit is Micro's own, so a call past it traps the same way in each.
fn three_ways(label: &str, module: Module, pairs: &[(i32, i32)]) -> Vec<Result<i32, Trap>> {
    three_ways_with(label, module, pairs, &[])
}

/// As [`three_ways`], for a module whose globals start at `globals`, given as the runtime gives them
/// to compiled code: each entry is a 64-bit value, an `i32` in its low half. Micro starts its own
/// globals from the module's initializers, which `globals` must match.
fn three_ways_with(
    label: &str,
    module: Module,
    pairs: &[(i32, i32)],
    globals: &[u64],
) -> Vec<Result<i32, Trap>> {
    let validated = tpt_wasm_validate::validate(module.clone())
        .unwrap_or_else(|error| panic!("{label}: the module must validate: {error:?}"));
    let verified = tpt_wasm_ir::lower_and_verify(&validated)
        .unwrap_or_else(|error| panic!("{label}: the IR must lower: {error:?}"));
    let limit = Config::default().limits.max_call_depth as u64;
    let machines: Vec<mir::MirFunction> = verified
        .module()
        .functions
        .iter()
        .map(|function| {
            mir::lower(function)
                .unwrap_or_else(|error| panic!("{label}: the machine IR must lower: {error:?}"))
        })
        .collect();
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    // The table is private to the module, as the test modules are, so its indirect calls compile.
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let native = tpt_wasm_jit::compile_module(verified.module(), limit, true);
    let indirect = mir::Indirect::for_module(verified.module());

    let engine = Engine::new(Config::default()).expect("the engine is constructible");
    let mut instance = engine
        .instantiate(module)
        .expect("a generated module instantiates");

    // Each backend keeps its own memory, as Micro's instance does, so the three persist their
    // state across the argument pairs in the same way.
    let mut memory = vec![0_u8; PAGE];
    let mut machine_globals = globals.to_vec();
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let mut native_memory = vec![0_u8; CAPACITY];
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let mut native_length = PAGE;
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))]
    let mut native_globals = globals.to_vec();

    let mut outcomes = Vec::new();
    for &(a, b) in pairs {
        let expected: Result<i32, Trap> =
            match instance.call("run", vec![Value::I32(a), Value::I32(b)]) {
                Ok(values) => match values.as_slice() {
                    [Value::I32(value)] => Ok(*value),
                    other => panic!("{label}: Micro returned {other:?}"),
                },
                Err(RuntimeError::Trap(trap)) => Err(trap),
                Err(other) => panic!("{label}: Micro failed without a trap: {other}"),
            };
        let actual: Result<i32, Trap> = match mir::run(
            &machines,
            0,
            &[i64::from(a), i64::from(b)],
            &mut memory,
            CAPACITY,
            &mut machine_globals,
            None,
            &indirect,
            limit,
            1_000_000,
        ) {
            Ok(values) => match values.as_slice() {
                [value] => Ok(*value as i32),
                other => panic!("{label}: machine IR returned {other:?}"),
            },
            Err(mir::MirError::Trap(trap)) => Err(trap),
            // These modules import nothing, so no host call can fail.
            Err(mir::MirError::HostFailed) => {
                panic!("{label}: a host call in a module without one")
            }
        };
        assert_eq!(
            actual, expected,
            "{label}: machine IR disagrees with Micro on ({a}, {b})"
        );
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ))]
        {
            // The live length comes back whether or not the call trapped: a grow before a trap
            // stands, as the runtime keeps it.
            let mut linear = LinearMemory {
                bytes: &mut native_memory,
                len: native_length as u64,
            };
            let outcome = native.call_with_memory(
                0,
                &[i64::from(a), i64::from(b)],
                Some(&mut linear),
                &mut native_globals,
            );
            native_length = linear.len as usize;
            let compiled: Result<i32, Trap> = match outcome {
                Ok(bits) => Ok(bits as u32 as i32),
                Err(tpt_wasm_jit::JitError::Trap(trap)) => Err(trap),
                Err(other) => panic!("{label}: native call failed: {other:?}"),
            };
            assert_eq!(
                compiled, expected,
                "{label}: native code disagrees with Micro on ({a}, {b})"
            );
            assert!(
                memory[..] == native_memory[..native_length],
                "{label}: native memory disagrees with the machine IR's after ({a}, {b})"
            );
            assert_eq!(
                machine_globals, native_globals,
                "{label}: native globals disagree with the machine IR's after ({a}, {b})"
            );
        }
        outcomes.push(expected);
    }
    outcomes
}

#[test]
fn calls_between_functions_agree_with_micro() {
    // run(a, b) = f1(b, a) - f1(a, b), and f1(x, y) = 3x - y, so run(a, b) = 4b - 4a.
    let run = vec![
        0x20, 1, 0x20, 0, 0x10, 1, 0x20, 0, 0x20, 1, 0x10, 1, 0x6b, 0x0b,
    ];
    let helper = vec![0x20, 0, 0x41, 3, 0x6c, 0x20, 1, 0x6b, 0x0b];
    let outcomes = three_ways(
        "calls",
        module_with(vec![run, helper]),
        &[(1, 2), (5, -3), (0, 0)],
    );
    assert_eq!(outcomes, vec![Ok(4), Ok(-32), Ok(0)]);
}

#[test]
fn recursion_agrees_with_micro_including_the_depth_limit() {
    // run(n, _) = f1(n, 0), and f1(n, _) = if n == 0 { 0 } else { f1(n - 1, 0) + 1 }, so f1 counts
    // down through its own frames, one per step.
    let run = vec![0x20, 0, 0x41, 0, 0x10, 1, 0x0b];
    let recurse = vec![
        0x20, 0, 0x45, // local.get 0; i32.eqz
        0x04, 0x7f, // if (result i32)
        0x41, 0x00, // i32.const 0
        0x05, // else
        0x20, 0, 0x41, 0x01, 0x6b, // local.get 0; i32.const 1; i32.sub
        0x41, 0x00, 0x10, 0x01, // i32.const 0; call 1
        0x41, 0x01, 0x6a, // i32.const 1; i32.add
        0x0b, // end
        0x0b, // end of the function
    ];
    let limit = i32::try_from(Config::default().limits.max_call_depth).expect("the limit fits");
    let outcomes = three_ways(
        "recursion",
        module_with(vec![run, recurse]),
        &[(3, 0), (100, 0), (limit - 1, 0), (limit, 0), (limit + 1, 0)],
    );
    assert_eq!(outcomes[0], Ok(3));
    assert_eq!(outcomes[1], Ok(100));
    // A chain as long as the limit is more frames than the limit allows, so it traps.
    assert_eq!(outcomes[4], Err(Trap::CallDepthExceeded));
}

/// A binary module with one mutable `i32` global, starting at 7. `run(a, _)` adds `a` to the global,
/// stores the sum back, and returns it.
const GLOBAL_MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // magic and version
    0x01, 0x07, 0x01, 0x60, 0x02, 0x7F, 0x7F, 0x01, 0x7F, // type: (i32, i32) -> i32
    0x03, 0x02, 0x01, 0x00, // function section: one function of type 0
    0x06, 0x06, 0x01, 0x7F, 0x01, 0x41, 0x07, 0x0B, // global: mutable i32, initialized to 7
    0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, // export "run" is function 0
    0x0A, 0x0D, 0x01, 0x0B, 0x00, // code section: one body of 11 bytes, no locals
    0x23, 0x00, // global.get 0
    0x20, 0x00, // local.get 0
    0x6A, // i32.add
    0x24, 0x00, // global.set 0
    0x23, 0x00, // global.get 0
    0x0B, // end
];

#[test]
fn globals_agree_with_micro_across_calls() {
    let module = tpt_wasm_decode::decode(GLOBAL_MODULE).expect("the module decodes");
    // The global starts at 7 and each call adds its argument, so the sums are 8, 18 and 15.
    let outcomes = three_ways_with("globals", module, &[(1, 0), (10, 0), (-3, 0)], &[7]);
    assert_eq!(outcomes, vec![Ok(8), Ok(18), Ok(15)]);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn compile_machine_refuses_a_global_without_a_globals_array() {
    let module = tpt_wasm_decode::decode(GLOBAL_MODULE).expect("the module decodes");
    let validated = tpt_wasm_validate::validate(module).expect("the module validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the IR lowers");
    let function = &verified.module().functions[0];
    assert!(matches!(
        tpt_wasm_jit::compile_machine(function),
        Err(tpt_wasm_jit::JitError::Unsupported(_))
    ));
}

/// A binary module that imports `env.f`, a function of two `i32`s returning one. `run(a, b)` returns
/// `f(a, b) + 1`.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
const HOST_MODULE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // magic and version
    0x01, 0x07, 0x01, 0x60, 0x02, 0x7F, 0x7F, 0x01, 0x7F, // type: (i32, i32) -> i32
    0x02, 0x09, 0x01, 0x03, b'e', b'n', b'v', 0x01, b'f', 0x00, 0x00, // import env.f: type 0
    0x03, 0x02, 0x01, 0x00, // one defined function, of type 0
    0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, // export "run" is function 1
    0x0A, 0x0D, 0x01, 0x0B, 0x00, // code section: one body of 11 bytes, no locals
    0x20, 0x00, // local.get 0
    0x20, 0x01, // local.get 1
    0x10, 0x00, // call 0: the import
    0x41, 0x01, // i32.const 1
    0x6A, // i32.add
    0x0B, // end
];

/// Records each host call's import and argument words, and computes `a * 7 + b`, which fails when `b`
/// is zero.
#[derive(Default)]
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
struct Affine {
    trace: Vec<(u32, Vec<i64>)>,
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
impl tpt_wasm_jit::HostCalls for Affine {
    fn arity(&self, _import: u32) -> (usize, usize) {
        (2, 1)
    }

    fn call(
        &mut self,
        import: u32,
        args: &[i64],
        results: &mut [i64],
        _memory: Option<&mut [u8]>,
    ) -> bool {
        self.trace.push((import, args.to_vec()));
        let (a, b) = (args[0] as i32, args[1] as i32);
        if b == 0 {
            return false;
        }
        results[0] = i64::from(a.wrapping_mul(7).wrapping_add(b));
        true
    }
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
#[test]
fn host_calls_agree_between_the_interpreter_and_native_code() {
    let module = tpt_wasm_decode::decode(HOST_MODULE).expect("the module decodes");
    let validated = tpt_wasm_validate::validate(module).expect("the module validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the IR lowers");
    let limit = Config::default().limits.max_call_depth as u64;
    let machines: Vec<mir::MirFunction> = verified
        .module()
        .functions
        .iter()
        .map(|function| mir::lower(function).expect("the machine IR lowers"))
        .collect();
    let indirect = mir::Indirect::for_module(verified.module());
    let native = tpt_wasm_jit::compile_module(verified.module(), limit, false);

    // Each call gets its own host, so each trace is that call's alone.
    for (a, b) in [(2, 3), (-1, 10), (5, 0)] {
        let mut interpreted_host = Affine::default();
        let mut memory = vec![0_u8; PAGE];
        let mut globals = Vec::new();
        let interpreted = mir::run(
            &machines,
            0,
            &[i64::from(a), i64::from(b)],
            &mut memory,
            CAPACITY,
            &mut globals,
            Some(&mut interpreted_host),
            &indirect,
            limit,
            1_000_000,
        );

        let mut native_host = Affine::default();
        let compiled = native.call_with_host(
            0,
            &[i64::from(a), i64::from(b)],
            None,
            &mut [],
            Some(&mut native_host),
        );

        // The host sees the same calls, in the same order, with the same words.
        assert_eq!(
            native_host.trace, interpreted_host.trace,
            "the host calls differ for ({a}, {b})"
        );
        match (interpreted, compiled) {
            (Ok(values), Ok(bits)) => {
                let values: Vec<i32> = values.iter().map(|word| *word as i32).collect();
                assert_eq!(
                    values,
                    vec![bits as u32 as i32],
                    "results differ for ({a}, {b})"
                );
                // f(a, b) = 7a + b, and run adds one.
                assert_eq!(values, vec![a * 7 + b + 1], "wrong result for ({a}, {b})");
            }
            // A zero second argument makes the host refuse the call; both backends fail the run.
            (Err(mir::MirError::HostFailed), Err(tpt_wasm_jit::JitError::Unsupported(_))) => {
                assert_eq!(b, 0, "only the zero argument fails");
            }
            (interpreted, compiled) => {
                panic!("the backends disagree on ({a}, {b}): {interpreted:?} and {compiled:?}")
            }
        }
    }
}

/// Run one `i64` operator, `op`, on `run(a, b) = a op b`.
fn wide_op(op: u8, pairs: &[(i64, i64)]) -> Vec<Result<i64, Trap>> {
    three_ways64(
        "op",
        wide_module_of(vec![0x20, 0, 0x20, 1, op, 0x0b]),
        pairs,
    )
}

/// A body that stores the `i64` argument `a` at `address` with `store`, then loads it back from the
/// same address with `load`.
fn wide_memory_body(store: u8, load: u8, address: i32) -> Vec<u8> {
    let mut body = vec![0x41];
    body.extend(sleb(address));
    body.extend([0x20, 0]); // local.get 0: the value
    body.extend([store, 0x00, 0x00]);
    body.push(0x41);
    body.extend(sleb(address));
    body.extend([load, 0x00, 0x00]);
    body.push(0x0B);
    body
}

#[test]
fn i64_memory_agrees_with_micro_at_its_widths_and_extensions() {
    // Each store writes the low bytes of the value, and each load reads them back, extended as its
    // opcode says, so the expected values are the stored bytes with that extension applied.
    let values = [
        (0x0123_4567_89AB_CDEF, 0),
        (-1, 0),
        (0x80, 0),
        (0x8000_0000, 0),
        (0xFFFF, 0),
    ];
    let quad = three_ways64(
        "quad",
        wide_module_of(wide_memory_body(0x37, 0x29, 16)),
        &values,
    );
    assert_eq!(quad[0], Ok(0x0123_4567_89AB_CDEF));
    assert_eq!(quad[1], Ok(-1));

    let signed8 = three_ways64(
        "8_s",
        wide_module_of(wide_memory_body(0x3C, 0x30, 16)),
        &values,
    );
    assert_eq!(signed8[2], Ok(-128));
    let unsigned8 = three_ways64(
        "8_u",
        wide_module_of(wide_memory_body(0x3C, 0x31, 16)),
        &values,
    );
    assert_eq!(unsigned8[1], Ok(255));
    assert_eq!(unsigned8[2], Ok(128));

    let signed16 = three_ways64(
        "16_s",
        wide_module_of(wide_memory_body(0x3D, 0x32, 16)),
        &values,
    );
    assert_eq!(signed16[4], Ok(-1));
    let unsigned16 = three_ways64(
        "16_u",
        wide_module_of(wide_memory_body(0x3D, 0x33, 16)),
        &values,
    );
    assert_eq!(unsigned16[4], Ok(65_535));

    // Stored as a word, the high half of the value is not kept: the signed load sign-extends it,
    // the unsigned load zero-extends it, and a quad load of the same bytes sees zeros above it.
    let signed32 = three_ways64(
        "32_s",
        wide_module_of(wide_memory_body(0x3E, 0x34, 16)),
        &values,
    );
    assert_eq!(signed32[3], Ok(-2_147_483_648));
    let unsigned32 = three_ways64(
        "32_u",
        wide_module_of(wide_memory_body(0x3E, 0x35, 16)),
        &values,
    );
    assert_eq!(unsigned32[3], Ok(2_147_483_648));
    let word_then_quad = three_ways64(
        "32_quad",
        wide_module_of(wide_memory_body(0x3E, 0x29, 16)),
        &values,
    );
    assert_eq!(word_then_quad[1], Ok(0xFFFF_FFFF));

    // A quad access that runs past the page traps, as an access of any width does.
    let past = three_ways64(
        "past",
        wide_module_of(wide_memory_body(0x37, 0x29, 65_530)),
        &values,
    );
    assert_eq!(past[0], Err(Trap::MemoryOutOfBounds));
}

/// A module whose `run` takes two values of type `arg` and returns one of type `result`, with two locals
/// of type `arg`.
fn io_module_of(arg: ValueType, result: ValueType, body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![arg, arg]),
            results: ResultType(vec![result]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: vec![LocalDecl {
                count: 2,
                value_type: arg,
            }],
            body,
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(4),
                },
                memory64: false,
            },
        }],
        ..Module::default()
    }
}

/// Run one conversion, `op`, on `run(a, _) = op(a)`, from a value of type `arg` to one of type `result`.
fn convert(
    op: u8,
    arg: ValueType,
    result: ValueType,
    pairs: &[(i64, i64)],
) -> Vec<Result<i64, Trap>> {
    three_ways_io(
        "convert",
        io_module_of(arg, result, vec![0x20, 0, op, 0x0B]),
        pairs,
        arg,
        result,
        &[],
    )
}

#[test]
fn integer_conversions_agree_with_micro() {
    // Wrapping keeps the low half; extending sign- or zero-extends it.
    let wrapped = convert(
        0xA7,
        ValueType::I64,
        ValueType::I32,
        &[(0x1_2345_6789, 0), (-1, 0)],
    ); // i32.wrap_i64
    assert_eq!(wrapped, vec![Ok(0x2345_6789), Ok(0xFFFF_FFFF)]);
    let extended_s = convert(
        0xAC,
        ValueType::I32,
        ValueType::I64,
        &[(-1, 0), (0x8000_0000, 0), (5, 0)],
    ); // i64.extend_i32_s
    assert_eq!(extended_s, vec![Ok(-1), Ok(-2_147_483_648), Ok(5)]);
    let extended_u = convert(0xAD, ValueType::I32, ValueType::I64, &[(-1, 0), (5, 0)]); // i64.extend_i32_u
    assert_eq!(extended_u, vec![Ok(0xFFFF_FFFF), Ok(5)]);

    // Sign extension of a narrower width.
    let s8 = convert(
        0xC0,
        ValueType::I32,
        ValueType::I32,
        &[(0x80, 0), (0x7F, 0)],
    ); // i32.extend8_s
    assert_eq!(s8, vec![Ok(0xFFFF_FF80), Ok(0x7F)]);
    let s8_64 = convert(0xC2, ValueType::I64, ValueType::I64, &[(0x80, 0)]); // i64.extend8_s
    assert_eq!(s8_64, vec![Ok(-128)]);
    let s32 = convert(
        0xC4,
        ValueType::I64,
        ValueType::I64,
        &[(0x8000_0000, 0), (0x7FFF_FFFF, 0)],
    ); // i64.extend32_s
    assert_eq!(s32, vec![Ok(-2_147_483_648), Ok(2_147_483_647)]);
}

#[test]
fn reinterpretations_agree_with_micro_and_keep_the_bits() {
    let to_bits = convert(
        0xBC,
        ValueType::F32,
        ValueType::I32,
        &[(bits32(-0.0), 0), (i64::from(PAYLOAD_NAN_F32), 0)],
    ); // i32.reinterpret_f32
    assert_eq!(
        to_bits,
        vec![Ok(0x8000_0000), Ok(i64::from(PAYLOAD_NAN_F32))]
    );
    let from_bits = convert(
        0xBE,
        ValueType::I32,
        ValueType::F32,
        &[(-1, 0), (i64::from(PAYLOAD_NAN_F32), 0)],
    ); // f32.reinterpret_i32
    assert_eq!(
        from_bits,
        vec![Ok(0xFFFF_FFFF), Ok(i64::from(PAYLOAD_NAN_F32))]
    );
    let f64_bits = convert(0xBD, ValueType::F64, ValueType::I64, &[(PAYLOAD_NAN, 0)]); // i64.reinterpret_f64
    assert_eq!(f64_bits, vec![Ok(PAYLOAD_NAN)]);
    let f64_from = convert(0xBF, ValueType::I64, ValueType::F64, &[(PAYLOAD_NAN, 0)]); // f64.reinterpret_i64
    assert_eq!(f64_from, vec![Ok(PAYLOAD_NAN)]);
}

#[test]
fn integer_to_float_conversions_agree_with_micro() {
    // Each is rounded once from the integer, as Rust's `as` rounds, which Micro uses.
    let u64_values = [
        0_i64,
        1,
        -1,
        i64::MIN,
        (1_i64 << 53) + 1,
        i64::MIN + (1 << 39) + 1,
    ];
    let pairs: Vec<(i64, i64)> = u64_values.iter().map(|value| (*value, 0)).collect();
    let expected_f64: Vec<Result<i64, Trap>> = u64_values
        .iter()
        .map(|value| Ok((*value as u64 as f64).to_bits() as i64))
        .collect();
    assert_eq!(
        convert(0xBA, ValueType::I64, ValueType::F64, &pairs),
        expected_f64
    ); // f64.convert_i64_u
    let expected_f32: Vec<Result<i64, Trap>> = u64_values
        .iter()
        .map(|value| Ok(i64::from((*value as u64 as f32).to_bits())))
        .collect();
    assert_eq!(
        convert(0xB5, ValueType::I64, ValueType::F32, &pairs),
        expected_f32
    ); // f32.convert_i64_u

    let signed = convert(0xB4, ValueType::I64, ValueType::F32, &pairs); // f32.convert_i64_s
    let expected_signed: Vec<Result<i64, Trap>> = u64_values
        .iter()
        .map(|value| Ok(i64::from((*value as f32).to_bits())))
        .collect();
    assert_eq!(signed, expected_signed);

    assert_eq!(
        convert(0xB8, ValueType::I32, ValueType::F64, &[(-1, 0), (7, 0)]), // f64.convert_i32_u
        vec![Ok(bits(4_294_967_295.0)), Ok(bits(7.0))]
    );
    assert_eq!(
        convert(0xB7, ValueType::I32, ValueType::F64, &[(-7, 0)]), // f64.convert_i32_s
        vec![Ok(bits(-7.0))]
    );
    assert_eq!(
        convert(0xB2, ValueType::I32, ValueType::F32, &[(-7, 0)]), // f32.convert_i32_s
        vec![Ok(bits32(-7.0))]
    );
    assert_eq!(
        convert(0xB3, ValueType::I32, ValueType::F32, &[(-1, 0)]), // f32.convert_i32_u
        vec![Ok(bits32(u32::MAX as f32))]
    );
}

#[test]
fn float_to_float_conversions_agree_with_micro_and_canonicalize_nans() {
    let demoted = convert(
        0xB6,
        ValueType::F64,
        ValueType::F32,
        &[
            (bits(1.000_000_000_1), 0),
            (PAYLOAD_NAN, 0),
            (bits(f64::MAX), 0),
        ],
    ); // f32.demote_f64
    assert_eq!(
        demoted,
        vec![
            Ok(bits32(1.000_000_000_1_f64 as f32)),
            Ok(i64::from(0x7fc0_0000_u32)), // a NaN gives the canonical one
            Ok(bits32(f32::INFINITY)),
        ]
    );
    let promoted = convert(
        0xBB,
        ValueType::F32,
        ValueType::F64,
        &[(i64::from(PAYLOAD_NAN_F32), 0), (bits32(1.5), 0)],
    ); // f64.promote_f32
    assert_eq!(promoted, vec![Ok(CANONICAL_NAN), Ok(bits(1.5))]);
}

#[test]
fn float_truncations_agree_with_micro_and_trap_as_micro_does() {
    // The truncated value must be in range; a NaN or an infinity is out of range.
    let signed32 = convert(
        0xAA, // i32.trunc_f64_s
        ValueType::F64,
        ValueType::I32,
        &[
            (bits(2.9), 0),
            (bits(-2.9), 0),
            (bits(-2_147_483_648.5), 0),
            (bits(2_147_483_648.0), 0),
            (PAYLOAD_NAN, 0),
            (bits(f64::INFINITY), 0),
            (bits(-0.5), 0),
        ],
    );
    assert_eq!(
        signed32,
        vec![
            Ok(2),
            Ok(i64::from(-2_i32 as u32)),
            Ok(i64::from(i32::MIN as u32)),
            Err(Trap::InvalidConversion),
            Err(Trap::InvalidConversion),
            Err(Trap::InvalidConversion),
            Ok(0),
        ]
    );
    let unsigned32 = convert(
        0xAB, // i32.trunc_f64_u
        ValueType::F64,
        ValueType::I32,
        &[
            (bits(-0.9), 0),
            (bits(4_294_967_295.9), 0),
            (bits(4_294_967_296.0), 0),
            (bits(-1.0), 0),
        ],
    );
    assert_eq!(
        unsigned32,
        vec![
            Ok(0),
            Ok(0xFFFF_FFFF),
            Err(Trap::InvalidConversion),
            Err(Trap::InvalidConversion)
        ]
    );
    let signed32_f32 = convert(
        0xA8, // i32.trunc_f32_s
        ValueType::F32,
        ValueType::I32,
        &[
            (bits32(-2_147_483_648.0), 0),
            (bits32(2_147_483_520.0), 0),
            (bits32(2_147_483_648.0), 0),
        ],
    );
    assert_eq!(
        signed32_f32,
        vec![
            Ok(i64::from(i32::MIN as u32)),
            Ok(2_147_483_520),
            Err(Trap::InvalidConversion),
        ]
    );
    // The 64-bit unsigned truncation shifts values from 2^63 up, so the top bit must come back.
    let unsigned64 = convert(
        0xB1, // i64.trunc_f64_u
        ValueType::F64,
        ValueType::I64,
        &[
            (bits(9_223_372_036_854_775_808.0), 0),
            (bits(18_446_744_073_709_549_568.0), 0),
            (bits(18_446_744_073_709_551_616.0), 0),
            (bits(-0.9), 0),
        ],
    );
    assert_eq!(
        unsigned64,
        vec![
            Ok(9_223_372_036_854_775_808_u64 as i64),
            Ok(18_446_744_073_709_549_568.0_f64 as u64 as i64),
            Err(Trap::InvalidConversion),
            Ok(0),
        ]
    );
    let signed64 = convert(
        0xB0, // i64.trunc_f64_s
        ValueType::F64,
        ValueType::I64,
        &[
            (bits(-9_223_372_036_854_775_808.0), 0),
            (bits(9_223_372_036_854_775_808.0), 0),
        ],
    );
    assert_eq!(signed64, vec![Ok(i64::MIN), Err(Trap::InvalidConversion)]);
    let unsigned64_f32 = convert(
        0xAF, // i64.trunc_f32_u
        ValueType::F32,
        ValueType::I64,
        &[(bits32(1.0e19), 0), (bits32(-1.0), 0)],
    );
    assert_eq!(
        unsigned64_f32,
        vec![Ok(1.0e19_f32 as u64 as i64), Err(Trap::InvalidConversion)]
    );
}

/// The bits of an `f64`, as a word.
fn bits(value: f64) -> i64 {
    value.to_bits() as i64
}

/// A signalling NaN with a payload: arithmetic on it must give the canonical NaN, and the sign
/// operations must keep the payload.
const PAYLOAD_NAN: i64 = 0x7ff0_0000_0000_0001;

/// The canonical NaN that Micro gives an arithmetic NaN result.
const CANONICAL_NAN: i64 = 0x7ff8_0000_0000_0000;

/// Run one `f64` operator on `run(a, b) = a op b`.
fn float_op(op: u8, pairs: &[(i64, i64)]) -> Vec<Result<i64, Trap>> {
    three_ways_typed(
        "f64",
        typed_module_of(ValueType::F64, vec![0x20, 0, 0x20, 1, op, 0x0B]),
        pairs,
        ValueType::F64,
    )
}

/// Run one `f64` comparison, `op`, selecting between `1.0` and `0.0` so the result is an `f64`.
fn float_compare(op: u8, pairs: &[(i64, i64)]) -> Vec<Result<i64, Trap>> {
    let mut body = vec![0x44];
    body.extend(1.0_f64.to_bits().to_le_bytes());
    body.push(0x44);
    body.extend(0.0_f64.to_bits().to_le_bytes());
    body.extend([0x20, 0, 0x20, 1, op, 0x1B, 0x0B]);
    three_ways_typed(
        "f64.compare",
        typed_module_of(ValueType::F64, body),
        pairs,
        ValueType::F64,
    )
}

#[test]
fn f64_arithmetic_agrees_with_micro_and_canonicalizes_nans() {
    let pairs = [
        (bits(1.5), bits(2.25)),
        (bits(f64::INFINITY), bits(f64::NEG_INFINITY)),
        (PAYLOAD_NAN, bits(1.0)),
        (bits(0.0), bits(0.0)),
        (bits(-0.0), bits(0.0)),
    ];
    let add = float_op(0xA0, &pairs);
    assert_eq!(add[0], Ok(bits(3.75)));
    assert_eq!(add[1], Ok(CANONICAL_NAN)); // inf + -inf is a NaN
    assert_eq!(add[2], Ok(CANONICAL_NAN)); // a NaN operand gives the canonical NaN
    let sub = float_op(0xA1, &pairs);
    assert_eq!(sub[1], Ok(bits(f64::INFINITY))); // inf - -inf is inf, not a NaN
    let mul = float_op(0xA2, &pairs);
    assert_eq!(mul[0], Ok(bits(3.375)));
    let div = float_op(0xA3, &pairs);
    assert_eq!(div[3], Ok(CANONICAL_NAN)); // 0 / 0
    assert_eq!(div[4], Ok(CANONICAL_NAN)); // -0 / 0

    // sqrt of a negative number is a NaN, canonical; the sign operations keep the payload.
    let sqrt = three_ways_typed(
        "f64.sqrt",
        typed_module_of(ValueType::F64, vec![0x20, 0, 0x9F, 0x0B]),
        &[(bits(4.0), 0), (bits(-1.0), 0), (PAYLOAD_NAN, 0)],
        ValueType::F64,
    );
    assert_eq!(
        sqrt,
        vec![Ok(bits(2.0)), Ok(CANONICAL_NAN), Ok(CANONICAL_NAN)]
    );
    let neg = three_ways_typed(
        "f64.neg",
        typed_module_of(ValueType::F64, vec![0x20, 0, 0x9A, 0x0B]),
        &[(PAYLOAD_NAN, 0)],
        ValueType::F64,
    );
    assert_eq!(neg, vec![Ok(PAYLOAD_NAN ^ i64::MIN)]);
    let abs = three_ways_typed(
        "f64.abs",
        typed_module_of(ValueType::F64, vec![0x20, 0, 0x99, 0x0B]),
        &[(PAYLOAD_NAN ^ i64::MIN, 0), (bits(-0.0), 0)],
        ValueType::F64,
    );
    assert_eq!(abs, vec![Ok(PAYLOAD_NAN), Ok(bits(0.0))]);
}

#[test]
fn f64_comparisons_agree_with_micro_including_nans() {
    // Index 0 compares a NaN with a number; the others are ordered.
    let pairs = [
        (bits(f64::NAN), bits(1.0)),
        (bits(1.0), bits(2.0)),
        (bits(2.0), bits(1.0)),
        (bits(1.0), bits(1.0)),
    ];
    let one = bits(1.0);
    let zero = 0;
    // The comparison of each pair, as 1 or 0: a NaN is unequal to everything, and below or above
    // nothing, so only `ne` holds for it.
    assert_eq!(
        float_compare(0x61, &pairs), // f64.eq
        vec![Ok(zero), Ok(zero), Ok(zero), Ok(one)]
    );
    assert_eq!(
        float_compare(0x62, &pairs), // f64.ne
        vec![Ok(one), Ok(one), Ok(one), Ok(zero)]
    );
    assert_eq!(
        float_compare(0x63, &pairs), // f64.lt
        vec![Ok(zero), Ok(one), Ok(zero), Ok(zero)]
    );
    assert_eq!(
        float_compare(0x64, &pairs), // f64.gt
        vec![Ok(zero), Ok(zero), Ok(one), Ok(zero)]
    );
    assert_eq!(
        float_compare(0x65, &pairs), // f64.le
        vec![Ok(zero), Ok(one), Ok(zero), Ok(one)]
    );
    assert_eq!(
        float_compare(0x66, &pairs), // f64.ge
        vec![Ok(zero), Ok(zero), Ok(one), Ok(one)]
    );
}

#[test]
fn f64_min_max_copysign_and_rounding_agree_with_micro() {
    // min and max: zeros of different sign, a NaN, and ordered operands.
    let pairs = [
        (bits(0.0), bits(-0.0)),
        (bits(-0.0), bits(0.0)),
        (bits(f64::NAN), bits(1.0)),
        (bits(1.0), bits(2.0)),
        (bits(2.0), bits(1.0)),
        (bits(2.0), bits(2.0)),
    ];
    let min = float_op(0xA4, &pairs); // f64.min
    assert_eq!(
        min,
        vec![
            Ok(bits(-0.0)),
            Ok(bits(-0.0)),
            Ok(CANONICAL_NAN),
            Ok(bits(1.0)),
            Ok(bits(1.0)),
            Ok(bits(2.0)),
        ]
    );
    let max = float_op(0xA5, &pairs); // f64.max
    assert_eq!(
        max,
        vec![
            Ok(bits(0.0)),
            Ok(bits(0.0)),
            Ok(CANONICAL_NAN),
            Ok(bits(2.0)),
            Ok(bits(2.0)),
            Ok(bits(2.0)),
        ]
    );

    // copysign takes the second operand's sign, and keeps the first one's payload even for a NaN.
    let copysign = float_op(
        0xA6,
        &[
            (bits(1.5), bits(-0.0)),
            (bits(-2.0), bits(3.0)),
            (PAYLOAD_NAN, bits(-1.0)),
        ],
    ); // f64.copysign
    assert_eq!(
        copysign,
        vec![Ok(bits(-1.5)), Ok(bits(2.0)), Ok(PAYLOAD_NAN ^ i64::MIN)]
    );

    // The rounding operations, with a NaN giving the canonical one.
    let rounding = |op: u8, values: &[f64]| -> Vec<Result<i64, Trap>> {
        let pairs: Vec<(i64, i64)> = values.iter().map(|value| (bits(*value), 0)).collect();
        three_ways_typed(
            "f64.round",
            typed_module_of(ValueType::F64, vec![0x20, 0, op, 0x0B]),
            &pairs,
            ValueType::F64,
        )
    };
    let inputs = [2.5, 3.5, -0.5, 1.7];
    assert_eq!(
        rounding(0x9E, &inputs), // f64.nearest: ties to even
        vec![Ok(bits(2.0)), Ok(bits(4.0)), Ok(bits(-0.0)), Ok(bits(2.0))]
    );
    let inputs = [-1.5, 1.7, -0.0, 0.5];
    assert_eq!(
        rounding(0x9C, &inputs), // f64.floor
        vec![Ok(bits(-2.0)), Ok(bits(1.0)), Ok(bits(-0.0)), Ok(bits(0.0))]
    );
    let inputs = [-0.5, 1.2, -0.0, 0.3];
    assert_eq!(
        rounding(0x9B, &inputs), // f64.ceil
        vec![Ok(bits(-0.0)), Ok(bits(2.0)), Ok(bits(-0.0)), Ok(bits(1.0))]
    );
    let inputs = [-1.7, 2.9, -0.5, 0.5];
    assert_eq!(
        rounding(0x9D, &inputs), // f64.trunc
        vec![Ok(bits(-1.0)), Ok(bits(2.0)), Ok(bits(-0.0)), Ok(bits(0.0))]
    );
    let inputs = [f64::NAN, -0.0];
    assert_eq!(
        rounding(0x9C, &inputs), // a NaN floors to the canonical NaN
        vec![Ok(CANONICAL_NAN), Ok(bits(-0.0))]
    );
}

/// The word of an `f32`, zero-extended.
fn bits32(value: f32) -> i64 {
    i64::from(value.to_bits())
}

/// The canonical NaN that Micro gives an `f32` arithmetic result.
const CANONICAL_NAN_F32: i64 = 0x7fc0_0000;

/// Run one `f32` binary operator on `run(a, b) = a op b`.
fn float32_op(op: u8, pairs: &[(i64, i64)]) -> Vec<Result<i64, Trap>> {
    three_ways_typed(
        "f32",
        typed_module_of(ValueType::F32, vec![0x20, 0, 0x20, 1, op, 0x0B]),
        pairs,
        ValueType::F32,
    )
}

/// Run one `f32` unary operator on `run(a, _) = op(a)`.
fn float32_unary(op: u8, pairs: &[(i64, i64)]) -> Vec<Result<i64, Trap>> {
    three_ways_typed(
        "f32.unary",
        typed_module_of(ValueType::F32, vec![0x20, 0, op, 0x0B]),
        pairs,
        ValueType::F32,
    )
}

/// Run one `f32` comparison, selecting between `1.0` and `0.0` so the result is an `f32`.
fn float32_compare(op: u8, pairs: &[(i64, i64)]) -> Vec<Result<i64, Trap>> {
    let mut body = vec![0x43];
    body.extend(1.0_f32.to_bits().to_le_bytes());
    body.push(0x43);
    body.extend(0.0_f32.to_bits().to_le_bytes());
    body.extend([0x20, 0, 0x20, 1, op, 0x1B, 0x0B]);
    three_ways_typed(
        "f32.compare",
        typed_module_of(ValueType::F32, body),
        pairs,
        ValueType::F32,
    )
}

#[test]
fn f32_operators_agree_with_micro_at_their_edges() {
    let pairs = [
        (bits32(1.5), bits32(2.25)),
        (bits32(f32::INFINITY), bits32(f32::NEG_INFINITY)),
        (i64::from(PAYLOAD_NAN_F32), bits32(1.0)),
        (bits32(0.0), bits32(-0.0)),
        (bits32(f32::NAN), bits32(1.0)),
    ];
    assert_eq!(float32_op(0x92, &pairs)[0], Ok(bits32(3.75))); // f32.add
    assert_eq!(float32_op(0x92, &pairs)[1], Ok(CANONICAL_NAN_F32)); // inf + -inf
    assert_eq!(float32_op(0x92, &pairs)[2], Ok(CANONICAL_NAN_F32)); // a payload NaN operand
    let div = float32_op(0x95, &pairs); // f32.div
    assert_eq!(div[3], Ok(CANONICAL_NAN_F32)); // 0 / -0 is a NaN
    assert_eq!(float32_op(0x96, &pairs)[3], Ok(bits32(-0.0))); // f32.min of zeros of both signs
    assert_eq!(float32_op(0x97, &pairs)[3], Ok(bits32(0.0))); // f32.max of zeros of both signs
    assert_eq!(float32_op(0x96, &pairs)[4], Ok(CANONICAL_NAN_F32)); // f32.min with a NaN
    assert_eq!(
        float32_op(
            0x98,
            &[
                (bits32(1.5), bits32(-0.0)),
                (i64::from(PAYLOAD_NAN_F32), bits32(-1.0))
            ]
        ), // f32.copysign
        vec![
            Ok(bits32(-1.5)),
            Ok(i64::from(PAYLOAD_NAN_F32 | 0x8000_0000))
        ]
    );

    // Unary: sign operations keep the payload, sqrt and rounding give the canonical NaN.
    let negated = float32_unary(0x8C, &[(i64::from(PAYLOAD_NAN_F32), 0)]); // f32.neg
    assert_eq!(negated, vec![Ok(i64::from(PAYLOAD_NAN_F32 | 0x8000_0000))]);
    let absolute = float32_unary(
        0x8B,
        &[
            (bits32(-0.0), 0),
            (i64::from(PAYLOAD_NAN_F32 | 0x8000_0000), 0),
        ],
    ); // f32.abs
    assert_eq!(
        absolute,
        vec![Ok(bits32(0.0)), Ok(i64::from(PAYLOAD_NAN_F32))]
    );
    let root = float32_unary(0x91, &[(bits32(4.0), 0), (bits32(-1.0), 0)]); // f32.sqrt
    assert_eq!(root, vec![Ok(bits32(2.0)), Ok(CANONICAL_NAN_F32)]);
    let nearest = float32_unary(
        0x90,
        &[(bits32(2.5), 0), (bits32(3.5), 0), (bits32(-0.5), 0)],
    ); // f32.nearest
    assert_eq!(
        nearest,
        vec![Ok(bits32(2.0)), Ok(bits32(4.0)), Ok(bits32(-0.0))]
    );
    let floor = float32_unary(0x8E, &[(bits32(-1.5), 0), (i64::from(PAYLOAD_NAN_F32), 0)]); // f32.floor
    assert_eq!(floor, vec![Ok(bits32(-2.0)), Ok(CANONICAL_NAN_F32)]);

    // Comparisons: a NaN is unequal to everything, and below or above nothing.
    let compare_pairs = [
        (bits32(f32::NAN), bits32(1.0)),
        (bits32(1.0), bits32(2.0)),
        (bits32(2.0), bits32(1.0)),
        (bits32(1.0), bits32(1.0)),
    ];
    let one = bits32(1.0);
    assert_eq!(
        float32_compare(0x5B, &compare_pairs),
        vec![Ok(0), Ok(0), Ok(0), Ok(one)]
    ); // f32.eq
    assert_eq!(
        float32_compare(0x5C, &compare_pairs),
        vec![Ok(one), Ok(one), Ok(one), Ok(0)]
    ); // f32.ne
    assert_eq!(
        float32_compare(0x5D, &compare_pairs),
        vec![Ok(0), Ok(one), Ok(0), Ok(0)]
    ); // f32.lt

    // A float store and load move the bits, payload included.
    let round_trip = three_ways_typed(
        "f32.memory",
        typed_module_of(ValueType::F32, wide_memory_body(0x38, 0x2A, 16)),
        &[(bits32(3.5), 0), (i64::from(PAYLOAD_NAN_F32), 0)],
        ValueType::F32,
    );
    assert_eq!(
        round_trip,
        vec![Ok(bits32(3.5)), Ok(i64::from(PAYLOAD_NAN_F32))]
    );
}

#[test]
fn f64_memory_agrees_with_micro_and_keeps_the_bits() {
    // A float store and load move the bits of the value, payload included.
    let pairs = [(bits(3.5), 0), (PAYLOAD_NAN, 0)];
    let round_trip = three_ways_typed(
        "f64.memory",
        typed_module_of(ValueType::F64, wide_memory_body(0x39, 0x2B, 16)),
        &pairs,
        ValueType::F64,
    );
    assert_eq!(round_trip, vec![Ok(bits(3.5)), Ok(PAYLOAD_NAN)]);
}

/// The opcodes and widths of one float precision, for the generated expressions.
struct Shape {
    /// The `const` opcode, and its operand width in bytes.
    konst: u8,
    width: usize,
    load: u8,
    store: u8,
    /// The `if` block type of the precision's result.
    block: u8,
    /// The first binary operator (add), with the seven operators after it: sub, mul, div, min, max,
    /// copysign.
    binary: u8,
    /// The five unary operators the generator uses: abs, neg, ceil, floor, trunc, nearest, sqrt.
    unary: [u8; 7],
    /// The first comparison (eq), with the six after it.
    compare: u8,
}

const DOUBLE: Shape = Shape {
    konst: 0x44,
    width: 8,
    load: 0x2B,
    store: 0x39,
    block: 0x7C,
    binary: 0xA0,
    unary: [0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F],
    compare: 0x61,
};

const SINGLE: Shape = Shape {
    konst: 0x43,
    width: 4,
    load: 0x2A,
    store: 0x38,
    block: 0x7D,
    binary: 0x92,
    unary: [0x8B, 0x8C, 0x8D, 0x8E, 0x8F, 0x90, 0x91],
    compare: 0x5B,
};

/// A bit pattern for a float constant or argument of `shape`: the interesting values, and random bits,
/// which include NaNs.
fn float_bits(rng: &mut Lcg, shape: &Shape) -> i64 {
    if shape.width == 8 {
        match rng.below(12) {
            0 => bits(0.0),
            1 => bits(-0.0),
            2 => bits(f64::INFINITY),
            3 => bits(f64::NEG_INFINITY),
            4 => bits(f64::NAN),
            5 => PAYLOAD_NAN,
            // Ordinary finite numbers, so the arithmetic and comparisons have varied operands to act on.
            6..=9 => bits(rng.below(2000) as f64 / 8.0 - 125.0),
            _ => rng.i64(),
        }
    } else {
        match rng.below(12) {
            0 => i64::from(0.0_f32.to_bits()),
            1 => i64::from((-0.0_f32).to_bits()),
            2 => i64::from(f32::INFINITY.to_bits()),
            3 => i64::from(f32::NEG_INFINITY.to_bits()),
            4 => i64::from(f32::NAN.to_bits()),
            5 => i64::from(PAYLOAD_NAN_F32),
            6..=9 => i64::from((rng.below(2000) as f32 / 8.0 - 125.0).to_bits()),
            _ => i64::from(rng.next() as u32),
        }
    }
}

/// Append code that pushes one float of `shape`. Locals 0 and 1 are the parameters; 2 and 3 are
/// declared.
fn float_expression(shape: &Shape, rng: &mut Lcg, depth: u32, out: &mut Vec<u8>) {
    if depth == 0 || rng.below(4) == 0 {
        if rng.below(2) == 0 {
            out.extend([0x20, rng.below(4) as u8]);
        } else {
            out.push(shape.konst);
            out.extend(float_bits(rng, shape).to_le_bytes()[..shape.width].iter());
        }
        return;
    }
    match rng.below(8) {
        0..=2 => {
            float_expression(shape, rng, depth - 1, out);
            float_expression(shape, rng, depth - 1, out);
            // add through copysign
            out.push(shape.binary + rng.below(7) as u8);
        }
        3 => {
            float_expression(shape, rng, depth - 1, out);
            out.push(shape.unary[rng.below(shape.unary.len() as u64) as usize]);
        }
        4 => {
            // select(a, b, a comparison of two more)
            for _ in 0..4 {
                float_expression(shape, rng, depth - 1, out);
            }
            out.push(shape.compare + rng.below(6) as u8); // eq through ge
            out.push(0x1B); // select
        }
        5 => {
            // if (result float) on a comparison
            float_expression(shape, rng, depth - 1, out);
            float_expression(shape, rng, depth - 1, out);
            out.push(shape.compare + rng.below(6) as u8);
            out.extend([0x04, shape.block]); // if (result float)
            float_expression(shape, rng, depth - 1, out);
            out.push(0x05); // else
            float_expression(shape, rng, depth - 1, out);
            out.push(0x0B); // end
        }
        6 => {
            float_expression(shape, rng, depth - 1, out);
            let local = 2 + rng.below(2) as u8;
            out.extend([0x22, local]); // local.tee
        }
        _ => {
            // A float store at a constant address, then a float load from it.
            let address = rng.below(256) as i32;
            out.push(0x41);
            out.extend(sleb(address));
            float_expression(shape, rng, depth - 1, out);
            out.extend([shape.store, 0x00, 0x00]);
            out.push(0x41);
            out.extend(sleb(address));
            out.extend([shape.load, 0x00, 0x00]);
        }
    }
}

/// Random float expressions of one precision, each run three ways on random arguments.
fn random_float_expressions(precision: Precision, shape: &Shape, seed: u64, label: &str) {
    let cases = 200;
    let mut rng = Lcg(seed);
    let mut runs = 0usize;
    let mut distinct = BTreeSet::new();
    for case in 0..cases {
        let mut body = Vec::new();
        float_expression(shape, &mut rng, 4, &mut body);
        body.push(0x0B);
        let pairs: Vec<(i64, i64)> = (0..4)
            .map(|_| (float_bits(&mut rng, shape), float_bits(&mut rng, shape)))
            .collect();
        for outcome in three_ways_typed(
            &format!("{label} case {case}"),
            typed_module_of(kind_of(precision), body),
            &pairs,
            kind_of(precision),
        ) {
            if let Ok(bits) = outcome {
                distinct.insert(bits);
            }
            runs += 1;
        }
    }
    assert!(
        distinct.len() > 50,
        "the {label} expressions must produce varied results, got {} distinct",
        distinct.len()
    );
    eprintln!(
        "{label} machine IR and native code: {runs} runs over {cases} modules agree with Micro"
    );
}

/// The value type of a precision.
fn kind_of(precision: Precision) -> ValueType {
    match precision {
        Precision::Single => ValueType::F32,
        Precision::Double => ValueType::F64,
    }
}

#[test]
fn the_f64_machine_ir_and_native_code_agree_with_micro_on_random_expressions() {
    random_float_expressions(Precision::Double, &DOUBLE, 0x5eed_f064, "f64");
}

#[test]
fn the_f32_machine_ir_and_native_code_agree_with_micro_on_random_expressions() {
    random_float_expressions(Precision::Single, &SINGLE, 0x5eed_f032, "f32");
}

/// The `f32` bits of a signalling NaN with a payload, the single-precision counterpart of
/// [`PAYLOAD_NAN`].
const PAYLOAD_NAN_F32: u32 = 0x7f80_0001;

/// One section of a binary module: its id, then its contents, which must be short enough for a one-byte
/// length.
fn section(id: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![id, u8::try_from(content.len()).expect("a short section")];
    out.extend(content);
    out
}

/// A binary module of one function, `run(a, b) = global + a` on a mutable global that starts at its
/// initializer, and which it then stores back, the sum being the result. `value_type` is the type's byte,
/// `add` the matching addition, and `init` the initializer, its constant and `end`.
fn global_module_bytes(value_type: u8, add: u8, init: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[1, 0x60, 2, value_type, value_type, 1, value_type],
    ));
    bytes.extend(section(3, &[1, 0]));
    let mut global = vec![1, value_type, 1];
    global.extend(init);
    bytes.extend(section(6, &global));
    bytes.extend(section(7, &[1, 3, b'r', b'u', b'n', 0, 0]));
    // The body: no locals; global.get 0; local.get 0; add; global.set 0; global.get 0; end.
    let body = [0, 0x23, 0, 0x20, 0, add, 0x24, 0, 0x23, 0, 0x0B];
    let mut code = vec![1, u8::try_from(body.len()).expect("a short body")];
    code.extend(body);
    bytes.extend(section(10, &code));
    bytes
}

#[test]
fn i64_and_float_globals_agree_with_micro_across_calls() {
    // i64: the global starts at 7 and each call adds its argument.
    let i64_module = tpt_wasm_decode::decode(&global_module_bytes(0x7E, 0x7C, &[0x42, 7, 0x0B]))
        .expect("the i64 module decodes");
    let i64_outcomes = three_ways_io(
        "i64.global",
        i64_module,
        &[(1, 0), (10, 0), (-3, 0)],
        ValueType::I64,
        ValueType::I64,
        &[7],
    );
    assert_eq!(i64_outcomes, vec![Ok(8), Ok(18), Ok(15)]);

    // f64: the global starts at 7.0, and the bits of each argument are added to it.
    let mut f64_init = vec![0x44];
    f64_init.extend(7.0_f64.to_bits().to_le_bytes());
    f64_init.push(0x0B);
    let f64_module = tpt_wasm_decode::decode(&global_module_bytes(0x7C, 0xA0, &f64_init))
        .expect("the f64 module decodes");
    let f64_outcomes = three_ways_io(
        "f64.global",
        f64_module,
        &[(bits(1.5), 0), (bits(2.0), 0)],
        ValueType::F64,
        ValueType::F64,
        &[bits(7.0) as u64],
    );
    assert_eq!(f64_outcomes, vec![Ok(bits(8.5)), Ok(bits(10.5))]);

    // f32: as f64, at single precision.
    let mut f32_init = vec![0x43];
    f32_init.extend(7.0_f32.to_bits().to_le_bytes());
    f32_init.push(0x0B);
    let f32_module = tpt_wasm_decode::decode(&global_module_bytes(0x7D, 0x92, &f32_init))
        .expect("the f32 module decodes");
    let f32_outcomes = three_ways_io(
        "f32.global",
        f32_module,
        &[(bits32(1.5), 0), (bits32(2.0), 0)],
        ValueType::F32,
        ValueType::F32,
        &[bits32(7.0) as u64],
    );
    assert_eq!(f32_outcomes, vec![Ok(bits32(8.5)), Ok(bits32(10.5))]);
}

/// A binary module that imports `env.f`, of type `(value, value) -> value`, and whose `run(a, b)` returns
/// `f(a, b)`. `value_type` is the type's byte.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
fn host_module_bytes(value_type: u8) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[1, 0x60, 2, value_type, value_type, 1, value_type],
    ));
    bytes.extend(section(2, &[1, 3, b'e', b'n', b'v', 1, b'f', 0, 0]));
    bytes.extend(section(3, &[1, 0]));
    bytes.extend(section(7, &[1, 3, b'r', b'u', b'n', 0, 1]));
    // The body: no locals; local.get 0; local.get 1; call 0 (the import); end.
    let body = [0, 0x20, 0, 0x20, 1, 0x10, 0, 0x0B];
    let mut code = vec![1, u8::try_from(body.len()).expect("a short body")];
    code.extend(body);
    bytes.extend(section(10, &code));
    bytes
}

/// A host function of `kind`: `a * 7 + b` on `i64`s, or the same on `f64`s by their bits. It fails when
/// `b` is zero, and records each call's import and words.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
struct WideAffine {
    kind: ValueType,
    trace: Vec<(u32, Vec<i64>)>,
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
impl tpt_wasm_jit::HostCalls for WideAffine {
    fn arity(&self, _import: u32) -> (usize, usize) {
        (2, 1)
    }

    fn call(
        &mut self,
        import: u32,
        args: &[i64],
        results: &mut [i64],
        _memory: Option<&mut [u8]>,
    ) -> bool {
        self.trace.push((import, args.to_vec()));
        match self.kind {
            ValueType::F64 => {
                let (x, y) = (
                    f64::from_bits(args[0] as u64),
                    f64::from_bits(args[1] as u64),
                );
                if y == 0.0 {
                    return false;
                }
                results[0] = (x * 7.0 + y).to_bits() as i64;
            }
            _ => {
                if args[1] == 0 {
                    return false;
                }
                results[0] = args[0].wrapping_mul(7).wrapping_add(args[1]);
            }
        }
        true
    }
}

/// Run a host-importing module on each pair through the interpreter and through native code, with a host
/// of each. The host must see the same calls in the same order with the same words, and the two must
/// agree on each result; a refused call (a zero `b`) fails both. Returns each pair's result, or `Err(())`
/// for a refused call.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
fn host_agreement(
    module_bytes: &[u8],
    kind: ValueType,
    pairs: &[(i64, i64)],
) -> Vec<Result<i64, ()>> {
    let module = tpt_wasm_decode::decode(module_bytes).expect("the host module decodes");
    let validated = tpt_wasm_validate::validate(module).expect("the host module validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the host module lowers");
    let limit = Config::default().limits.max_call_depth as u64;
    let machines: Vec<mir::MirFunction> = verified
        .module()
        .functions
        .iter()
        .map(|function| mir::lower(function).expect("the machine IR lowers"))
        .collect();
    let indirect = mir::Indirect::for_module(verified.module());
    let native = tpt_wasm_jit::compile_module(verified.module(), limit, false);

    let mut outcomes = Vec::new();
    for &(a, b) in pairs {
        let mut interpreted_host = WideAffine {
            kind,
            trace: Vec::new(),
        };
        let mut memory = vec![0_u8; PAGE];
        let mut globals = Vec::new();
        let interpreted = mir::run(
            &machines,
            0,
            &[a, b],
            &mut memory,
            CAPACITY,
            &mut globals,
            Some(&mut interpreted_host),
            &indirect,
            limit,
            1_000_000,
        );

        let mut native_host = WideAffine {
            kind,
            trace: Vec::new(),
        };
        let compiled = native.call_with_host(0, &[a, b], None, &mut [], Some(&mut native_host));
        assert_eq!(
            native_host.trace, interpreted_host.trace,
            "the host calls differ for ({a}, {b})"
        );
        match (interpreted, compiled) {
            (Ok(values), Ok(bits)) => {
                assert_eq!(values, vec![bits as i64], "results differ for ({a}, {b})");
                outcomes.push(Ok(values[0]));
            }
            (Err(mir::MirError::HostFailed), Err(tpt_wasm_jit::JitError::Unsupported(_))) => {
                outcomes.push(Err(()));
            }
            (interpreted, compiled) => {
                panic!("the backends disagree on ({a}, {b}): {interpreted:?} and {compiled:?}")
            }
        }
    }
    outcomes
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
#[test]
fn i64_host_calls_carry_full_words_and_agree_with_the_interpreter() {
    let outcomes = host_agreement(
        &host_module_bytes(0x7E),
        ValueType::I64,
        &[(2, 3), (-1, 10), (5, 0)],
    );
    // The host's sums are full 64-bit words; the zero second argument makes it refuse.
    assert_eq!(outcomes, vec![Ok(17), Ok(3), Err(())]);
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
))]
#[test]
fn f64_host_calls_carry_the_bits_and_agree_with_the_interpreter() {
    let outcomes = host_agreement(
        &host_module_bytes(0x7C),
        ValueType::F64,
        &[
            (bits(1.5), bits(2.0)),
            (bits(-1.0), bits(0.5)),
            (bits(3.0), bits(0.0)),
        ],
    );
    assert_eq!(outcomes, vec![Ok(bits(12.5)), Ok(bits(-6.5)), Err(())]);
}

#[test]
fn unreachable_agrees_with_micro_as_a_trap() {
    // if (a == 0) { unreachable }; a + b
    let body = vec![
        0x20, 0, 0x45, 0x04, 0x40, 0x00, 0x0B, 0x20, 0, 0x20, 1, 0x6A, 0x0B,
    ];
    let outcomes = three_ways("unreachable", module_of(body), &[(0, 5), (3, 4)]);
    assert_eq!(outcomes, vec![Err(Trap::Unreachable), Ok(7)]);
}

/// A binary module of two functions of type `(v, v) -> v` and a table of two slots: slot 0 holds
/// function 1, slot 1 is null. `run(a, b)` calls table[index(b)](a, a), where `index` is the conversion
/// `index_op` into an `i32`. Function 1 is `a * 3 - b`, with the constant and the operators of `value_type`
/// (`konst` is the constant's opcode and bytes, `mul` and `sub` the operators).
fn indirect_module_bytes(value_type: u8, index_op: u8, konst: &[u8], mul: u8, sub: u8) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(
        1,
        &[1, 0x60, 2, value_type, value_type, 1, value_type],
    ));
    bytes.extend(section(3, &[2, 0, 0]));
    bytes.extend(section(4, &[1, 0x70, 0, 2]));
    bytes.extend(section(7, &[1, 3, b'r', b'u', b'n', 0, 0]));
    // Element segment: at offset 0, slot 0 holds function 1; slot 1 stays null.
    bytes.extend(section(9, &[1, 0, 0x41, 0, 0x0B, 1, 1]));
    // run: local.get 0 (the callee's first argument), local.get 0, local.get 1, the index conversion,
    // call_indirect type 0 table 0.
    let run = [0, 0x20, 0, 0x20, 0, 0x20, 1, index_op, 0x11, 0, 0, 0x0B];
    // function 1: local.get 0; the constant 3; mul; local.get 1; sub.
    let mut callee = vec![0, 0x20, 0];
    callee.extend(konst);
    callee.extend([mul, 0x20, 1, sub, 0x0B]);
    let mut code = vec![2, u8::try_from(run.len()).expect("a short body")];
    code.extend(run);
    code.push(u8::try_from(callee.len()).expect("a short body"));
    code.extend(callee);
    bytes.extend(section(10, &code));
    bytes
}

#[test]
fn indirect_calls_of_i64_and_f64_agree_with_micro() {
    // i64: the index is the wrapped low half of b; 0 names function 1, 1 is null, 2 is out of range.
    let i64_module =
        tpt_wasm_decode::decode(&indirect_module_bytes(0x7E, 0xA7, &[0x42, 3], 0x7E, 0x7D))
            .expect("the i64 module decodes");
    let i64_outcomes = three_ways_io(
        "i64.indirect",
        i64_module,
        &[(5, 0), (5, 1), (5, 2)],
        ValueType::I64,
        ValueType::I64,
        &[],
    );
    assert_eq!(
        i64_outcomes,
        vec![
            Ok(10),
            Err(Trap::NullReference),
            Err(Trap::TableOutOfBounds)
        ]
    );

    // f64: the index truncates b, so 0.0 names function 1, 1.0 is null, 2.0 is out of range.
    let mut f64_const = vec![0x44];
    f64_const.extend(3.0_f64.to_bits().to_le_bytes());
    let f64_module =
        tpt_wasm_decode::decode(&indirect_module_bytes(0x7C, 0xAA, &f64_const, 0xA2, 0xA1))
            .expect("the f64 module decodes");
    let f64_outcomes = three_ways_io(
        "f64.indirect",
        f64_module,
        &[
            (bits(5.0), bits(0.0)),
            (bits(5.0), bits(1.0)),
            (bits(5.0), bits(2.0)),
        ],
        ValueType::F64,
        ValueType::F64,
        &[],
    );
    assert_eq!(
        f64_outcomes,
        vec![
            Ok(bits(10.0)),
            Err(Trap::NullReference),
            Err(Trap::TableOutOfBounds)
        ]
    );
}

#[test]
fn i64_bit_counts_agree_with_micro_at_their_edges() {
    // Only the first argument is read; the second is ignored.
    let inputs = [
        (0, 0),
        (1, 0),
        (-1, 0),
        (i64::MIN, 0),
        (0x1234_5678_9ABC_DEF0, 0),
        (0x0000_0001_0000_0000, 0),
    ];
    let clz = three_ways64(
        "i64.clz",
        wide_module_of(vec![0x20, 0, 0x79, 0x0B]),
        &inputs,
    );
    assert_eq!(clz[0], Ok(64)); // a zero input has the full width of leading zeros
    assert_eq!(clz[1], Ok(63));
    assert_eq!(clz[2], Ok(0));
    assert_eq!(clz[4], Ok(3));
    assert_eq!(clz[5], Ok(31));

    let ctz = three_ways64(
        "i64.ctz",
        wide_module_of(vec![0x20, 0, 0x7A, 0x0B]),
        &inputs,
    );
    assert_eq!(ctz[0], Ok(64));
    assert_eq!(ctz[3], Ok(63));
    assert_eq!(ctz[4], Ok(4));
    assert_eq!(ctz[5], Ok(32));

    let popcnt = three_ways64(
        "i64.popcnt",
        wide_module_of(vec![0x20, 0, 0x7B, 0x0B]),
        &inputs,
    );
    assert_eq!(popcnt[0], Ok(0));
    assert_eq!(popcnt[2], Ok(64));
    assert_eq!(popcnt[3], Ok(1));
    assert_eq!(popcnt[4], Ok(32)); // 5 + 8 + 9 + 10 across the four nibble groups
    assert_eq!(popcnt[5], Ok(1));
}

#[test]
fn i64_operators_agree_with_micro_at_their_edges() {
    // Indices into the pairs are used by the pinned outcomes.
    let pairs = [
        (7, 2),
        (i64::MIN, -1),
        (5, 0),
        (-7, 2),
        (-1, 2),
        (1, 65),
        (-8, 1),
        (-1, 60),
        (1, 64),
        (1, 1),
    ];

    let div_s = wide_op(0x7F, &pairs); // i64.div_s
    assert_eq!(div_s[0], Ok(3));
    assert_eq!(div_s[1], Err(Trap::IntegerOverflow));
    assert_eq!(div_s[2], Err(Trap::IntegerDivisionByZero));
    assert_eq!(div_s[3], Ok(-3));

    let rem_s = wide_op(0x81, &pairs); // i64.rem_s
    assert_eq!(rem_s[1], Ok(0)); // the minimum value by -1 has no remainder, and does not trap
    assert_eq!(rem_s[3], Ok(-1));

    let div_u = wide_op(0x80, &pairs); // i64.div_u
    assert_eq!(div_u[4], Ok(i64::MAX)); // (2^64 - 1) / 2

    let rem_u = wide_op(0x82, &pairs); // i64.rem_u
    assert_eq!(rem_u[4], Ok(1));

    let mul = wide_op(0x7E, &pairs); // i64.mul
    assert_eq!(mul[1], Ok(i64::MIN)); // the product wraps

    // Shift and rotation counts are taken modulo 64.
    let shl = wide_op(0x86, &pairs); // i64.shl
    assert_eq!(shl[5], Ok(2));
    let shr_s = wide_op(0x87, &pairs); // i64.shr_s
    assert_eq!(shr_s[6], Ok(-4));
    let shr_u = wide_op(0x88, &pairs); // i64.shr_u
    assert_eq!(shr_u[7], Ok(15));
    let rotl = wide_op(0x89, &pairs); // i64.rotl
    assert_eq!(rotl[8], Ok(1));
    let rotr = wide_op(0x8A, &pairs); // i64.rotr
    assert_eq!(rotr[9], Ok(i64::MIN));

    // Signed and unsigned comparisons of -1 with 0 disagree, as they should.
    let select_lt = |compare: u8| {
        vec![
            0x42, 0x01, 0x42, 0x00, 0x20, 0, 0x20, 1, compare, 0x1B, 0x0B,
        ]
    };
    let lt_s = three_ways64("lt_s", wide_module_of(select_lt(0x53)), &[(-1, 0), (0, -1)]);
    assert_eq!(lt_s, vec![Ok(1), Ok(0)]); // i64.lt_s
    let lt_u = three_ways64("lt_u", wide_module_of(select_lt(0x54)), &[(-1, 0), (0, -1)]);
    assert_eq!(lt_u, vec![Ok(0), Ok(1)]); // i64.lt_u
}

#[test]
fn the_i64_machine_ir_and_native_code_agree_with_micro_on_random_expressions() {
    let cases = 200;
    let mut rng = Lcg(0x5eed_6464);
    let mut runs = 0usize;
    let mut distinct = BTreeSet::new();
    let mut trapped = 0usize;
    for case in 0..cases {
        let mut body = Vec::new();
        wide_expression(&mut rng, 4, &mut body);
        body.push(0x0b);
        let pairs: Vec<(i64, i64)> = (0..4).map(|_| (rng.i64(), rng.i64())).collect();
        for outcome in three_ways64(&format!("case {case}"), wide_module_of(body), &pairs) {
            match outcome {
                Ok(value) => {
                    distinct.insert(value);
                }
                Err(_) => trapped += 1,
            }
            runs += 1;
        }
    }
    assert!(
        distinct.len() > 50,
        "the i64 expressions must produce varied results, got {} distinct",
        distinct.len()
    );
    assert!(trapped > 0, "the random i64 division must trap sometimes");
    eprintln!(
        "i64 machine IR and native code: {runs} runs over {cases} modules agree with Micro ({trapped} trapped)"
    );
}

#[test]
fn indirect_calls_agree_with_micro_on_null_mismatched_and_out_of_range_slots() {
    // A binary module, decoded. `run(a, b)` calls table[b](a, a) through type 0. The table has three
    // slots: slot 0 is function 1 (type 0, a * 3 - b), slot 1 is null, and slot 2 is function 2,
    // whose type 1 differs from the call's.
    let bytes: &[u8] = &[
        0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // magic and version
        0x01, 0x0C, 0x02, // type section: two types
        0x60, 0x02, 0x7F, 0x7F, 0x01, 0x7F, // type 0: (i32, i32) -> i32
        0x60, 0x01, 0x7F, 0x01, 0x7F, // type 1: (i32) -> i32
        0x03, 0x04, 0x03, 0x00, 0x00, 0x01, // function section: types 0, 0, 1
        0x04, 0x04, 0x01, 0x70, 0x00, 0x03, // table section: one funcref table, minimum 3
        0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, // export "run" is function 0
        0x09, 0x0F, 0x01, 0x04, 0x41, 0x00, 0x0B, 0x03, // element: at 0, three entries
        0xD2, 0x01, 0x0B, // ref.func 1
        0xD0, 0x70, 0x0B, // ref.null
        0xD2, 0x02, 0x0B, // ref.func 2
        0x0A, 0x20, 0x03, // code section: three bodies
        0x0B, 0x00, 0x20, 0x00, 0x20, 0x00, 0x20, 0x01, 0x11, 0x00, 0x00, 0x0B, // run
        0x0A, 0x00, 0x20, 0x00, 0x41, 0x03, 0x6C, 0x20, 0x01, 0x6B, 0x0B, // a * 3 - b
        0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6A, 0x0B, // a + 1
    ];
    let module = tpt_wasm_decode::decode(bytes).expect("the module decodes");
    let outcomes = three_ways(
        "indirect",
        module,
        &[(5, 0), (5, 1), (5, 2), (5, 3), (5, -1)],
    );
    assert_eq!(
        outcomes,
        vec![
            Ok(10),
            Err(Trap::NullReference),
            Err(Trap::IndirectCallTypeMismatch),
            Err(Trap::TableOutOfBounds),
            Err(Trap::TableOutOfBounds),
        ]
    );
}

#[test]
fn the_machine_ir_and_native_code_agree_with_micro_on_random_control_flow() {
    let cases = 300;
    let mut rng = Lcg(0x5eed_1001);
    let mut runs = 0usize;
    let mut distinct = BTreeSet::new();
    let mut trap_kinds = BTreeSet::new();
    let mut trapped = 0usize;
    for case in 0..cases {
        let mut body = Vec::new();
        expression(&mut rng, 4, &mut body);
        body.push(0x0b);
        let pairs: Vec<(i32, i32)> = (0..4).map(|_| (rng.i32(), rng.i32())).collect();

        for outcome in three_ways(&format!("case {case}"), module_of(body), &pairs) {
            match outcome {
                Ok(value) => {
                    distinct.insert(value);
                }
                Err(trap) => {
                    trapped += 1;
                    trap_kinds.insert(format!("{trap:?}"));
                }
            }
            runs += 1;
        }
    }
    assert!(
        distinct.len() > 50,
        "the expressions must produce varied results, got {} distinct",
        distinct.len()
    );
    assert!(
        trapped > 0 && trap_kinds.contains("IntegerDivisionByZero"),
        "the random division must trap on a zero divisor, got {trap_kinds:?} in {trapped} runs"
    );
    eprintln!(
        "machine IR and native code: {runs} runs over {cases} modules agree with Micro ({trapped} trapped)"
    );
}

#[test]
fn memory_size_and_grow_agree_with_micro_up_to_the_maximum() {
    // Growing by three from one page reaches the four-page maximum; every later grow fails.
    let grows = three_ways(
        "grow",
        module_of(vec![0x20, 0, 0x40, 0x00, 0x0b]),
        &[(3, 0), (0, 0), (1, 0), (1, 0), (-1, 0)],
    );
    // The first grow returns the old size; a zero grow returns the current size; a failed grow is -1.
    assert_eq!(grows, vec![Ok(1), Ok(4), Ok(-1), Ok(-1), Ok(-1)]);

    // The size after each grow: one page more per call until the maximum, then it stays put.
    let sizes = three_ways(
        "size",
        module_of(vec![0x20, 0, 0x40, 0x00, 0x1a, 0x3f, 0x00, 0x0b]),
        &[(1, 0), (1, 0), (1, 0), (1, 0)],
    );
    assert_eq!(sizes, vec![Ok(2), Ok(3), Ok(4), Ok(4)]);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn compile_machine_refuses_growth_without_a_reservation() {
    let module = module_of(vec![0x20, 0, 0x40, 0x00, 0x0b]);
    let validated = tpt_wasm_validate::validate(module).expect("the module validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the IR lowers");
    let function = &verified.module().functions[0];
    assert!(matches!(
        tpt_wasm_jit::compile_machine(function),
        Err(tpt_wasm_jit::JitError::Unsupported(_))
    ));
    assert!(tpt_wasm_jit::compile_machine_reserved(function).is_ok());
}

#[test]
fn memory_accesses_agree_with_micro_at_their_edges() {
    // The first argument is the address, the second the value stored there.
    let pairs = [
        (0, 0x1234_5678),
        (65_532, -1), // the last whole word of the page
        (65_533, 5),  // a word that runs past the page: it traps
        (-1, 0),      // the largest unsigned address: it traps
        (100, 0x80),  // a byte with its high bit set
        (8, -2),
    ];
    // Store the value as a word, then read the word back.
    let word = three_ways(
        "word",
        module_of(vec![
            0x20, 0, 0x20, 1, 0x36, 0x02, 0x00, 0x20, 0, 0x28, 0x02, 0x00, 0x0b,
        ]),
        &pairs,
    );
    assert_eq!(word[0], Ok(0x1234_5678));
    assert_eq!(word[1], Ok(-1));
    assert_eq!(word[2], Err(Trap::MemoryOutOfBounds));
    assert_eq!(word[3], Err(Trap::MemoryOutOfBounds));

    // Store a byte, then read it back sign-extended, and again zero-extended.
    let signed = three_ways(
        "load8_s",
        module_of(vec![
            0x20, 0, 0x20, 1, 0x3a, 0x00, 0x00, 0x20, 0, 0x2c, 0x00, 0x00, 0x0b,
        ]),
        &pairs,
    );
    assert_eq!(signed[4], Ok(-128));
    let unsigned = three_ways(
        "load8_u",
        module_of(vec![
            0x20, 0, 0x20, 1, 0x3a, 0x00, 0x00, 0x20, 0, 0x2d, 0x00, 0x00, 0x0b,
        ]),
        &pairs,
    );
    assert_eq!(unsigned[4], Ok(128));
    assert_eq!(unsigned[3], Err(Trap::MemoryOutOfBounds));

    // A half-word store writes two bytes, so the word read after it holds the old high bytes.
    let half = three_ways(
        "store16",
        module_of(vec![
            0x20, 0, 0x20, 1, 0x3b, 0x00, 0x00, 0x20, 0, 0x28, 0x02, 0x00, 0x0b,
        ]),
        &pairs,
    );
    assert_eq!(half[0], Ok(0x0000_5678));
    assert_eq!(half[1], Ok(0x0000_FFFF));
}

#[test]
fn bit_counts_agree_with_micro_at_their_edges() {
    // Only the first argument is read; the second is ignored.
    let inputs = [
        (0, 0),
        (1, 0),
        (-1, 0),
        (i32::MIN, 0),
        (0x1234_5678, 0),
        (0x0F0F_0F0F, 0),
        (0x5555_5555, 0),
    ];
    let clz = three_ways("clz", module_of(vec![0x20, 0, 0x67, 0x0b]), &inputs);
    assert_eq!(clz[0], Ok(32)); // a zero input has the full width of leading zeros
    assert_eq!(clz[1], Ok(31));
    assert_eq!(clz[3], Ok(0));
    assert_eq!(clz[4], Ok(3));
    let ctz = three_ways("ctz", module_of(vec![0x20, 0, 0x68, 0x0b]), &inputs);
    assert_eq!(ctz[0], Ok(32));
    assert_eq!(ctz[3], Ok(31));
    assert_eq!(ctz[4], Ok(3));
    let popcnt = three_ways("popcnt", module_of(vec![0x20, 0, 0x69, 0x0b]), &inputs);
    assert_eq!(popcnt[0], Ok(0));
    assert_eq!(popcnt[2], Ok(32));
    assert_eq!(popcnt[4], Ok(13));
    assert_eq!(popcnt[5], Ok(16));
    assert_eq!(popcnt[6], Ok(16));
}

#[test]
fn rotations_agree_with_micro_with_counts_past_the_width() {
    // The count is taken modulo 32: rotl by 33 is rotl by 1, and rotl by -1 is rotl by 31.
    let pairs = [(1, 33), (1, 1), (i32::MIN, -1), (0x1234_5678, 0), (-2, 36)];
    let rotl = three_ways(
        "rotl",
        module_of(vec![0x20, 0, 0x20, 1, 0x77, 0x0b]),
        &pairs,
    );
    assert_eq!(rotl[0], Ok(2));
    assert_eq!(rotl[2], Ok(1_073_741_824)); // 0x4000_0000
    let rotr = three_ways(
        "rotr",
        module_of(vec![0x20, 0, 0x20, 1, 0x78, 0x0b]),
        &pairs,
    );
    assert_eq!(rotr[1], Ok(i32::MIN)); // 0x8000_0000
    assert_eq!(rotr[3], Ok(0x1234_5678));
}

#[test]
fn division_and_remainder_agree_with_micro_at_their_edges() {
    // Indices into the pairs below are used by the pinned outcomes.
    let pairs = [
        (7, 2),
        (-7, 2),
        (5, 0),
        (i32::MIN, -1),
        (i32::MIN, 1),
        (-1, 2),
        (i32::MIN, 2),
        (0, 0),
        (-1, -1),
        (9, -3),
    ];

    let div_s = three_ways(
        "div_s",
        module_of(vec![0x20, 0, 0x20, 1, 0x6d, 0x0b]),
        &pairs,
    );
    assert_eq!(div_s[0], Ok(3));
    assert_eq!(div_s[1], Ok(-3));
    assert_eq!(div_s[2], Err(Trap::IntegerDivisionByZero));
    // The minimum value divided by -1 is out of range, so it traps rather than wrapping.
    assert_eq!(div_s[3], Err(Trap::IntegerOverflow));
    assert_eq!(div_s[4], Ok(i32::MIN));
    assert_eq!(div_s[9], Ok(-3));

    let div_u = three_ways(
        "div_u",
        module_of(vec![0x20, 0, 0x20, 1, 0x6e, 0x0b]),
        &pairs,
    );
    assert_eq!(div_u[1], Ok(2_147_483_644)); // (2^32 - 7) / 2
    assert_eq!(div_u[2], Err(Trap::IntegerDivisionByZero));
    assert_eq!(div_u[3], Ok(0)); // 2^31 / (2^32 - 1) is zero, with no overflow

    let rem_s = three_ways(
        "rem_s",
        module_of(vec![0x20, 0, 0x20, 1, 0x6f, 0x0b]),
        &pairs,
    );
    assert_eq!(rem_s[0], Ok(1));
    assert_eq!(rem_s[1], Ok(-1)); // the remainder takes the sign of the dividend
    assert_eq!(rem_s[2], Err(Trap::IntegerDivisionByZero));
    // The remainder of the minimum value by -1 is zero, and it does not trap.
    assert_eq!(rem_s[3], Ok(0));
    assert_eq!(rem_s[8], Ok(0));

    let rem_u = three_ways(
        "rem_u",
        module_of(vec![0x20, 0, 0x20, 1, 0x70, 0x0b]),
        &pairs,
    );
    assert_eq!(rem_u[1], Ok(1)); // (2^32 - 7) mod 2
    assert_eq!(rem_u[2], Err(Trap::IntegerDivisionByZero));
    assert_eq!(rem_u[3], Ok(i32::MIN)); // 2^31 mod (2^32 - 1) is 2^31
}
