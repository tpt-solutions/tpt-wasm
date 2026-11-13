// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Random modules with calls, indirect calls through a table, memory, and floats, compiled
//! together and run natively, must agree with Micro on every call: the result, the trap, and the
//! whole memory afterwards.
//!
//! The generator is typed, so each module is well formed by construction, and a module that still
//! fails validation is skipped. Direct calls go only to earlier functions, so they cannot recurse;
//! indirect calls can, through the table, and the call-depth limit then stops them on both engines.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_wasm_format::{
    ConstExpr, Element, ElementInit, ElementMode, Export, ExportDesc, Function, Import, ImportDesc,
    Memory, Module, Table,
};
use tpt_wasm_jit::{compile_module, HostCalls, LinearMemory};
use tpt_wasm_runtime::{Config, Engine, EngineMode, HostFunction, RuntimeError};
use tpt_wasm_types::{
    FunctionType, Limits, MemoryType, RefType, ResourceLimits, ResultType, TableType, Value,
    ValueType,
};

/// The call-depth limit both engines enforce. Small, so recursion through the table reaches it.
const MAX_CALL_DEPTH: usize = 8;
const PAGE: usize = 65_536;
/// The imports the module declares, `env.h`, which come before its defined functions in the index space.
const IMPORTS: u32 = 1;

/// Reads choices from the fuzzer's bytes. Past the end it returns zero, so every input describes
/// some module.
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

    fn quad(&mut self) -> u64 {
        u64::from(self.word() as u32) | (u64::from(self.word() as u32) << 32)
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

/// The value kinds the generator works in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    I32,
    F32,
    F64,
}

impl Kind {
    fn value_type(self) -> ValueType {
        match self {
            Kind::I32 => ValueType::I32,
            Kind::F32 => ValueType::F32,
            Kind::F64 => ValueType::F64,
        }
    }
}

/// The module's function types. Type 0 is `(i32, i32) -> i32`, the type indirect calls name.
const TYPES: [(&[Kind], Kind); 6] = [
    (&[Kind::I32, Kind::I32], Kind::I32),
    (&[Kind::I32], Kind::I32),
    (&[Kind::F64, Kind::F64], Kind::F64),
    (&[Kind::F64], Kind::I32),
    (&[Kind::F32, Kind::F32], Kind::F32),
    (&[Kind::F32], Kind::I32),
];

const I32_BINARY: [u8; 21] = [
    0x6A, 0x6B, 0x6C, 0x71, 0x72, 0x73, // add sub mul and or xor
    0x6D, 0x6E, 0x6F, 0x70, // div_s div_u rem_s rem_u
    0x74, 0x75, 0x76, // shl shr_s shr_u
    0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, // eq ne lt_s lt_u gt_s gt_u le_s le_u
];

#[derive(Clone)]
struct Signature {
    params: Vec<Kind>,
    result: Kind,
}

/// Generates the expression for one function. `functions` are the functions already generated,
/// which a direct call may name, and `all` is every signature, which an indirect call may reach.
struct Generator<'a, 'b> {
    bytes: &'a mut Bytes<'b>,
    functions: Vec<Signature>,
}

impl Generator<'_, '_> {
    fn pick(&mut self, count: usize) -> usize {
        usize::from(self.bytes.byte()) % count
    }

    /// A leaf of `kind`: a constant, or a parameter of that kind when there is one.
    fn leaf(&mut self, kind: Kind, params: &[Kind]) -> Vec<u8> {
        let of_kind: Vec<usize> = params
            .iter()
            .enumerate()
            .filter(|(_, parameter)| **parameter == kind)
            .map(|(index, _)| index)
            .collect();
        if !of_kind.is_empty() && self.bytes.byte() % 2 == 0 {
            let chosen = of_kind[self.pick(of_kind.len())];
            return vec![0x20, chosen as u8];
        }
        match kind {
            Kind::I32 => {
                let mut out = vec![0x41];
                out.extend(sleb(i64::from(self.bytes.word())));
                out
            }
            Kind::F32 => {
                let mut out = vec![0x43];
                out.extend((self.bytes.word() as u32).to_le_bytes());
                out
            }
            Kind::F64 => {
                let mut out = vec![0x44];
                out.extend(self.bytes.quad().to_le_bytes());
                out
            }
        }
    }

    /// An expression of `kind`, at most `depth` operations deep.
    fn expression(&mut self, kind: Kind, depth: u32, params: &[Kind]) -> Vec<u8> {
        if depth == 0 {
            return self.leaf(kind, params);
        }
        let choice = self.bytes.byte() % 15;
        match (kind, choice) {
            (_, 0 | 1) => self.leaf(kind, params),
            (Kind::I32, 2 | 3) => {
                let mut out = self.expression(Kind::I32, depth - 1, params);
                out.extend(self.expression(Kind::I32, depth - 1, params));
                out.push(I32_BINARY[self.pick(I32_BINARY.len())]);
                out
            }
            (Kind::I32, 4) => {
                // f32 or f64 comparisons, which yield an i32: eq ne lt gt le ge.
                let width = if self.bytes.byte() % 2 == 0 {
                    Kind::F32
                } else {
                    Kind::F64
                };
                let base = if width == Kind::F32 { 0x5B } else { 0x61 };
                let mut out = self.expression(width, depth - 1, params);
                out.extend(self.expression(width, depth - 1, params));
                out.push(base + self.pick(6) as u8);
                out
            }
            (Kind::F32, 2 | 3) => {
                let mut out = self.expression(Kind::F32, depth - 1, params);
                out.extend(self.expression(Kind::F32, depth - 1, params));
                // add sub mul div min max copysign
                out.push([0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98][self.pick(7)]);
                out
            }
            (Kind::F32, 4) => {
                let mut out = self.expression(Kind::F32, depth - 1, params);
                // abs, neg, ceil, floor, trunc, nearest, sqrt
                out.push([0x8B, 0x8C, 0x8D, 0x8E, 0x8F, 0x90, 0x91][self.pick(7)]);
                out
            }
            (Kind::I32, 5) => {
                let mut out = self.expression(Kind::I32, depth - 1, params);
                out.push(0x45); // eqz
                out
            }
            (Kind::F64, 2 | 3) => {
                let mut out = self.expression(Kind::F64, depth - 1, params);
                out.extend(self.expression(Kind::F64, depth - 1, params));
                // add sub mul div min max copysign
                out.push([0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6][self.pick(7)]);
                out
            }
            (Kind::F64, 4) => {
                let mut out = self.expression(Kind::F64, depth - 1, params);
                // abs, neg, sqrt, ceil, floor, trunc, nearest
                out.push([0x99, 0x9A, 0x9F, 0x9B, 0x9C, 0x9D, 0x9E][self.pick(7)]);
                out
            }
            (Kind::I32, 10 | 11) => {
                // A truncation of a float, which traps on a NaN, an infinity, or an out-of-range
                // value: the trap is part of what is compared.
                if self.bytes.byte() % 2 == 0 {
                    let mut out = self.expression(Kind::F64, depth - 1, params);
                    out.push(if self.bytes.byte() % 2 == 0 { 0xAA } else { 0xAB });
                    out
                } else {
                    let mut out = self.expression(Kind::F32, depth - 1, params);
                    out.push(if self.bytes.byte() % 2 == 0 { 0xA8 } else { 0xA9 });
                    out
                }
            }
            (Kind::F64, 10 | 11) => {
                // An i32 converted to an f64, signed or unsigned.
                let mut out = self.expression(Kind::I32, depth - 1, params);
                out.push(if self.bytes.byte() % 2 == 0 { 0xB7 } else { 0xB8 });
                out
            }
            (Kind::F32, 10 | 11) => {
                // An f64 demoted to an f32, or an i32 converted to one, signed or unsigned.
                if self.bytes.byte() % 2 == 0 {
                    let mut out = self.expression(Kind::F64, depth - 1, params);
                    out.push(0xB6); // f32.demote_f64
                    out
                } else {
                    let mut out = self.expression(Kind::I32, depth - 1, params);
                    out.push(if self.bytes.byte() % 2 == 0 { 0xB2 } else { 0xB3 });
                    out
                }
            }
            (Kind::F64, 12 | 13) => {
                // An f32 promoted to an f64.
                let mut out = self.expression(Kind::F32, depth - 1, params);
                out.push(0xBB); // f64.promote_f32
                out
            }
            (Kind::I32, 12 | 13) => {
                // memory.size: the memory's length in pages, which is fixed during a call.
                vec![0x3F, 0x00]
            }
            (Kind::I32, 14) => {
                // A call to the host import, which computes `x * 7 + 3` on its argument.
                let mut out = self.expression(Kind::I32, depth - 1, params);
                out.push(0x10); // call
                out.extend(uleb(0));
                out
            }
            (_, 6) => self.call(kind, depth, params),
            (Kind::I32, 7) => self.indirect_call(depth, params),
            (_, 8) => {
                // A store, then the value. The address is masked into the page half of the time.
                let mut out = self.address(depth, params);
                match kind {
                    Kind::I32 => {
                        out.extend(self.expression(Kind::I32, depth - 1, params));
                        out.extend([0x36, 0x02, 0x00]); // i32.store
                    }
                    Kind::F32 => {
                        out.extend(self.expression(Kind::F32, depth - 1, params));
                        out.extend([0x38, 0x02, 0x00]); // f32.store
                    }
                    Kind::F64 => {
                        out.extend(self.expression(Kind::F64, depth - 1, params));
                        out.extend([0x39, 0x03, 0x00]); // f64.store
                    }
                }
                out.extend(self.expression(kind, depth - 1, params));
                out
            }
            (_, 9) => {
                // A load from an address.
                let mut out = self.address(depth, params);
                match kind {
                    Kind::I32 => out.extend([0x28, 0x02, 0x00]), // i32.load
                    Kind::F32 => out.extend([0x2A, 0x02, 0x00]), // f32.load
                    Kind::F64 => out.extend([0x2B, 0x03, 0x00]), // f64.load
                }
                out
            }
            _ => self.leaf(kind, params),
        }
    }

    /// An i32 address: the expression, masked into the page or left raw.
    fn address(&mut self, depth: u32, params: &[Kind]) -> Vec<u8> {
        let mut out = self.expression(Kind::I32, depth.saturating_sub(1), params);
        if self.bytes.byte() % 4 != 0 {
            out.push(0x41);
            out.extend(sleb(0xFFFF));
            out.push(0x71); // i32.and
        }
        out
    }

    /// A direct call to an earlier function whose result is `kind`, if there is one.
    fn call(&mut self, kind: Kind, depth: u32, params: &[Kind]) -> Vec<u8> {
        let candidates: Vec<usize> = self
            .functions
            .iter()
            .enumerate()
            .filter(|(_, signature)| signature.result == kind)
            .map(|(index, _)| index)
            .collect();
        if candidates.is_empty() {
            return self.leaf(kind, params);
        }
        let target = candidates[self.pick(candidates.len())];
        let signature = self.functions[target].clone();
        let mut out = Vec::new();
        for parameter in &signature.params {
            out.extend(self.expression(*parameter, depth - 1, params));
        }
        out.push(0x10); // call
        out.extend(uleb(target as u32 + IMPORTS));
        out
    }

    /// An indirect call through the table, to type 0 `(i32, i32) -> i32`.
    fn indirect_call(&mut self, depth: u32, params: &[Kind]) -> Vec<u8> {
        let mut out = self.expression(Kind::I32, depth - 1, params);
        out.extend(self.expression(Kind::I32, depth - 1, params));
        out.extend(self.expression(Kind::I32, depth - 1, params));
        out.extend([0x11, 0x00, 0x00]); // call_indirect type 0, table 0
        out
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

/// Build the module: its functions, one memory page, a four-slot table, and every function
/// exported as `f<index>`, with the memory as `mem`.
fn build(bytes: &mut Bytes<'_>) -> Module {
    let count = usize::from(bytes.byte() % 4) + 1;
    let mut functions = Vec::new();
    let mut generator = Generator {
        bytes,
        functions: Vec::new(),
    };
    for _ in 0..count {
        let type_index = generator.pick(TYPES.len());
        let (params, result) = TYPES[type_index];
        let body = {
            let mut body = generator.expression(result, 3, params);
            body.push(0x0B);
            body
        };
        functions.push(Function {
            type_index: type_index as u32,
            locals: Vec::new(),
            body,
        });
        generator.functions.push(Signature {
            params: params.to_vec(),
            result,
        });
    }
    // The table: functions of any type, and nulls, so an indirect call can match or mismatch.
    let mut table = Vec::new();
    for _ in 0..4 {
        let choice = generator.pick(count + 1);
        table.push(if choice == count {
            None
        } else {
            Some(choice as u32 + IMPORTS)
        });
    }

    let types = TYPES
        .iter()
        .map(|(params, result)| FunctionType {
            params: ResultType(params.iter().map(|kind| kind.value_type()).collect()),
            results: ResultType(vec![result.value_type()]),
        })
        .collect();
    let elements = vec![Element {
        element_type: RefType::FuncRef,
        mode: ElementMode::Active {
            table_index: 0,
            offset: ConstExpr(vec![0x41, 0x00, 0x0B]),
        },
        init: ElementInit::Expressions(
            table
                .iter()
                .map(|slot| match slot {
                    Some(index) => {
                        let mut expression = vec![0xD2];
                        expression.extend(uleb(*index + IMPORTS));
                        expression.push(0x0B);
                        ConstExpr(expression)
                    }
                    None => ConstExpr(vec![0xD0, 0x70, 0x0B]),
                })
                .collect(),
        ),
    }];
    let mut exports: Vec<Export> = (0..count)
        .map(|index| Export {
            name: format!("f{index}"),
            desc: ExportDesc::Function(index as u32 + IMPORTS),
        })
        .collect();
    exports.push(Export {
        name: "mem".into(),
        desc: ExportDesc::Memory(0),
    });
    Module {
        types,
        imports: vec![Import {
            module: "env".into(),
            name: "h".into(),
            desc: ImportDesc::Function(1),
        }],
        functions,
        tables: vec![Table {
            table_type: TableType {
                element_type: RefType::FuncRef,
                limits: Limits {
                    min: 4,
                    max: None,
                },
            },
            init: None,
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
                memory64: false,
            },
        }],
        elements,
        exports,
        ..Module::default()
    }
}

/// The host function on Micro, the golden side of the host call.
struct GoldenHost;

impl HostFunction for GoldenHost {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn call(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        match args {
            [Value::I32(value)] => Ok(vec![Value::I32(value.wrapping_mul(7).wrapping_add(3))]),
            _ => Err("h takes one i32".into()),
        }
    }
}

/// The same host function, as compiled code reaches it: its argument and result are words.
struct NativeHost;

impl HostCalls for NativeHost {
    fn arity(&self, _import: u32) -> (usize, usize) {
        (1, 1)
    }

    fn call(
        &mut self,
        _import: u32,
        args: &[i64],
        results: &mut [i64],
        _memory: Option<&mut [u8]>,
    ) -> bool {
        results[0] = i64::from((args[0] as i32).wrapping_mul(7).wrapping_add(3));
        true
    }
}

/// The module instantiated on Micro, the golden engine, with the same call-depth limit.
fn micro_instance(module: &Module) -> tpt_wasm_runtime::Instance {
    let mut engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        limits: ResourceLimits {
            max_call_depth: MAX_CALL_DEPTH,
            ..ResourceLimits::default()
        },
        ..Config::default()
    })
    .expect("accepted");
    engine.linker_mut().define_function("env", "h", GoldenHost);
    engine.instantiate(module.clone()).expect("instantiates")
}

fn to_value(kind: Kind, bits: u64) -> Value {
    match kind {
        Kind::I32 => Value::I32(bits as u32 as i32),
        Kind::F32 => Value::F32(bits as u32),
        Kind::F64 => Value::F64(bits),
    }
}


fuzz_target!(|data: &[u8]| {
    let mut bytes = Bytes { data, position: 0 };
    let module = build(&mut bytes);

    // Modules that do not validate are not the compiler's input.
    let Ok(validated) = tpt_wasm_validate::validate(module.clone()) else {
        return;
    };
    let lowered = tpt_wasm_ir::lower_and_verify(&validated)
        .expect("a validated module lowers and verifies");
    let ir: &tpt_wasm_ir::IrModule = lowered.module();
    let native = compile_module(ir, MAX_CALL_DEPTH as u64, true);
    let mut golden = micro_instance(&module);
    // The memory the native code reads and writes, which starts as Micro's does: zeroed.
    let mut memory = vec![0u8; PAGE];

    for _ in 0..6 {
        let index = usize::from(bytes.byte()) % module.functions.len();
        let function_type = &module.types[module.functions[index].type_index as usize];
        let result_kind = match function_type.results.0[0] {
            ValueType::F32 => Kind::F32,
            ValueType::F64 => Kind::F64,
            _ => Kind::I32,
        };
        let args: Vec<(Kind, u64)> = function_type
            .params
            .0
            .iter()
            .map(|parameter| {
                let kind = match parameter {
                    ValueType::F32 => Kind::F32,
                    ValueType::F64 => Kind::F64,
                    _ => Kind::I32,
                };
                (kind, bytes.quad())
            })
            .collect();

        let micro_args: Vec<Value> = args
            .iter()
            .map(|(kind, bits)| to_value(*kind, *bits))
            .collect();
        let expected = match golden.call(&format!("f{index}"), micro_args) {
            Ok(results) => match results.as_slice() {
                [value] => Ok(value.clone()),
                other => panic!("expected one result, got {other:?}"),
            },
            Err(RuntimeError::Trap(trap)) => Err(trap),
            Err(other) => panic!("Micro failed in a way that is not a trap: {other:?}"),
        };

        let slots: Vec<i64> = args.iter().map(|(_, bits)| *bits as i64).collect();
        let actual = {
            let mut view = LinearMemory {
                bytes: &mut memory,
                len: PAGE as u64,
            };
            match native.call_with_host(index, &slots, Some(&mut view), &mut [], Some(&mut NativeHost)) {
                Ok(bits) => Ok(to_value(result_kind, bits)),
                Err(tpt_wasm_jit::JitError::Trap(trap)) => Err(trap),
                Err(error) => panic!("the compiled code failed without a trap: {error}"),
            }
        };

        // Exact, bit for bit, as the deterministic mode of Micro is.
        let agree = match (&expected, &actual) {
            (Ok(a), Ok(b)) => a == b,
            (Err(a), Err(b)) => a == b,
            _ => false,
        };
        if !agree {
            panic!("f{index} with {args:?}: native {actual:?}, Micro {expected:?}");
        }
        let micro_memory = golden
            .read_memory("mem", 0, PAGE)
            .expect("reads the memory");
        if micro_memory != memory {
            panic!("f{index} with {args:?}: the memory after the call differs");
        }
    }
});
