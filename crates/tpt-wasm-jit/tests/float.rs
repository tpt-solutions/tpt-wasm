// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Compiled `f32` and `f64` code must agree with Micro: arithmetic, comparisons (including the
//! unordered cases), negation and absolute value, constants, and loads and stores of both
//! widths. The comparison is exact, bit for bit: arithmetic that yields a NaN yields the canonical one,
//! as Micro's deterministic mode does, and `neg` and `abs` keep a NaN's payload, as Micro does.
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
use tpt_wasm_jit::{compile, CompiledFunction, JitError};
use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{FunctionType, Limits, MemoryType, ResultType, Trap, Value, ValueType};

const PAGE: usize = 65_536;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 3
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// An `f32` operand as its bits: an edge value, or arbitrary bits (which include NaNs).
    fn f32_bits(&mut self) -> u32 {
        const EDGES: [u32; 14] = [
            0x0000_0000, // +0
            0x8000_0000, // -0
            0x3f80_0000, // 1
            0xbf80_0000, // -1
            0x3f00_0000, // 0.5
            0x4040_0000, // 3
            0x7f80_0000, // +inf
            0xff80_0000, // -inf
            0x7fc0_0000, // a quiet NaN
            0x0000_0001, // the smallest subnormal
            0x7f7f_ffff, // the largest finite
            0x3eff_fffe, // just below 0.5
            0xc2f6_0000, // -123
            0x0080_0000, // the smallest normal
        ];
        match self.below(3) {
            0 => self.next() as u32,
            _ => EDGES[self.below(EDGES.len() as u64) as usize],
        }
    }

    /// An `f64` operand as its bits.
    fn f64_bits(&mut self) -> u64 {
        const EDGES: [u64; 14] = [
            0x0000_0000_0000_0000,
            0x8000_0000_0000_0000,
            0x3ff0_0000_0000_0000, // 1
            0xbff0_0000_0000_0000, // -1
            0x3fe0_0000_0000_0000, // 0.5
            0x4008_0000_0000_0000, // 3
            0x7ff0_0000_0000_0000, // +inf
            0xfff0_0000_0000_0000, // -inf
            0x7ff8_0000_0000_0000, // a quiet NaN
            0x0000_0000_0000_0001, // the smallest subnormal
            0x7fef_ffff_ffff_ffff, // the largest finite
            0x3fdf_ffff_ffff_fffe,
            0xc05e_c000_0000_0000, // -123
            0x0010_0000_0000_0000, // the smallest normal
        ];
        match self.below(3) {
            0 => self.next(),
            _ => EDGES[self.below(EDGES.len() as u64) as usize],
        }
    }

    /// An address: inside the page or near its end or past it.
    fn address(&mut self) -> i32 {
        match self.below(3) {
            0 => PAGE as i32 - 1 - self.below(16) as i32,
            1 => PAGE as i32 + self.below(16) as i32,
            _ => self.below(PAGE as u64) as i32,
        }
    }
}

fn module(params: Vec<ValueType>, result: ValueType, body: Vec<u8>, memory: bool) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(params),
            results: ResultType(vec![result]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            body,
        }],
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
        exports: {
            let mut exports = vec![Export {
                name: "run".into(),
                desc: ExportDesc::Function(0),
            }];
            if memory {
                exports.push(Export {
                    name: "mem".into(),
                    desc: ExportDesc::Memory(0),
                });
            }
            exports
        },
        ..Module::default()
    }
}

/// A case: its parameter types, its result type, and its body.
struct Case {
    params: Vec<ValueType>,
    result: ValueType,
    body: Vec<u8>,
    memory: bool,
    name: String,
}

/// The arguments for one call, as the raw bits both sides take.
fn arguments(rng: &mut Lcg, params: &[ValueType]) -> Vec<i64> {
    params
        .iter()
        .map(|parameter| match parameter {
            ValueType::F32 => i64::from(rng.f32_bits()),
            ValueType::F64 => rng.f64_bits() as i64,
            ValueType::I32 => i64::from(rng.address()),
            other => panic!("the cases use no {other:?} parameter"),
        })
        .collect()
}

fn to_value(value_type: ValueType, bits: i64) -> Value {
    match value_type {
        ValueType::I32 => Value::I32(bits as i32),
        ValueType::F32 => Value::F32(bits as u32),
        ValueType::F64 => Value::F64(bits as u64),
        other => panic!("the cases use no {other:?} value"),
    }
}

fn micro(module: &Module, case: &Case, args: &[i64]) -> (Result<Value, Trap>, Vec<u8>) {
    let engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("accepted");
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    let values: Vec<Value> = case
        .params
        .iter()
        .zip(args)
        .map(|(parameter, bits)| to_value(*parameter, *bits))
        .collect();
    let outcome = match instance.call("run", values) {
        Ok(results) => match results.as_slice() {
            [value] => Ok(value.clone()),
            other => panic!("expected one result, got {other:?}"),
        },
        Err(RuntimeError::Trap(trap)) => Err(trap),
        Err(other) => panic!("Micro failed in a way that is not a trap: {other:?}"),
    };
    let memory = if module.memories.is_empty() {
        Vec::new()
    } else {
        instance
            .read_memory("mem", 0, PAGE)
            .expect("reads the page")
    };
    (outcome, memory)
}

fn compiled(module: &Module) -> CompiledFunction {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("the case validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the case lowers");
    compile(&verified.module().functions[0]).expect("the case is in the subset")
}

/// Run the compiled function on raw bits and map the slot to the value Micro would report.
fn jit(
    code: &CompiledFunction,
    result: ValueType,
    args: &[i64],
    memory: Option<&mut [u8]>,
) -> Result<Value, Trap> {
    match code.call_raw(args, memory) {
        Ok(raw) => Ok(match result {
            ValueType::I32 => Value::I32(raw as u32 as i32),
            ValueType::F32 => Value::F32(raw as u32),
            ValueType::F64 => Value::F64(raw),
            other => panic!("the cases use no {other:?} result"),
        }),
        Err(JitError::Trap(trap)) => Err(trap),
        Err(other) => panic!("the compiled code failed without a trap: {other}"),
    }
}

fn is_nan(value: &Value) -> bool {
    match value {
        Value::F32(bits) => f32::from_bits(*bits).is_nan(),
        Value::F64(bits) => f64::from_bits(*bits).is_nan(),
        _ => false,
    }
}

/// Equal, or both NaN: a NaN payload is not part of the result the specification fixes.
fn agree(expected: &Result<Value, Trap>, actual: &Result<Value, Trap>) -> bool {
    match (expected, actual) {
        // Exact: a NaN result is the canonical one on both sides, as Micro's deterministic mode makes it.
        (Ok(a), Ok(b)) => a == b,
        (Err(a), Err(b)) => a == b,
        _ => false,
    }
}

/// Run every case on random operands and compare each outcome with Micro. Returns how many
/// values and traps were seen, so the caller can rule out a vacuous pass.
fn check(cases: &[Case], rng: &mut Lcg, per_case: usize) -> (usize, usize, usize) {
    let mut values = 0;
    let mut traps = 0;
    let mut nans = 0;
    for case in cases {
        let module = module(
            case.params.clone(),
            case.result,
            case.body.clone(),
            case.memory,
        );
        let code = compiled(&module);
        for _ in 0..per_case {
            let args = arguments(rng, &case.params);
            let (expected, expected_memory) = micro(&module, case, &args);
            let (actual, actual_memory) = if case.memory {
                let mut memory = vec![0u8; PAGE];
                let actual = jit(&code, case.result, &args, Some(&mut memory));
                (actual, memory)
            } else {
                (jit(&code, case.result, &args, None), Vec::new())
            };
            assert!(
                agree(&expected, &actual),
                "{}, args {args:?}: Micro {expected:?}, compiled {actual:?}",
                case.name
            );
            assert_eq!(
                expected_memory, actual_memory,
                "{}, args {args:?}: memory differs",
                case.name
            );
            match &expected {
                Ok(value) => {
                    values += 1;
                    if is_nan(value) {
                        nans += 1;
                    }
                }
                Err(_) => traps += 1,
            }
        }
    }
    (values, traps, nans)
}

const F32_BINARY: [(u8, &str); 4] = [
    (0x92, "f32.add"),
    (0x93, "f32.sub"),
    (0x94, "f32.mul"),
    (0x95, "f32.div"),
];
const F64_BINARY: [(u8, &str); 4] = [
    (0xA0, "f64.add"),
    (0xA1, "f64.sub"),
    (0xA2, "f64.mul"),
    (0xA3, "f64.div"),
];
const F32_COMPARE: [(u8, &str); 6] = [
    (0x5B, "f32.eq"),
    (0x5C, "f32.ne"),
    (0x5D, "f32.lt"),
    (0x5E, "f32.gt"),
    (0x5F, "f32.le"),
    (0x60, "f32.ge"),
];
const F64_COMPARE: [(u8, &str); 6] = [
    (0x61, "f64.eq"),
    (0x62, "f64.ne"),
    (0x63, "f64.lt"),
    (0x64, "f64.gt"),
    (0x65, "f64.le"),
    (0x66, "f64.ge"),
];

fn binary_cases(width: ValueType, table: &[(u8, &str)], result: ValueType) -> Vec<Case> {
    table
        .iter()
        .map(|(op, name)| Case {
            params: vec![width, width],
            result,
            body: vec![0x20, 0x00, 0x20, 0x01, *op, 0x0B],
            memory: false,
            name: (*name).to_string(),
        })
        .collect()
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
fn compiled_float_arithmetic_and_comparisons_agree_with_micro() {
    let mut rng = Lcg(0x3232_3232);
    let mut cases = binary_cases(ValueType::F32, &F32_BINARY, ValueType::F32);
    cases.extend(binary_cases(ValueType::F64, &F64_BINARY, ValueType::F64));
    cases.extend(binary_cases(ValueType::F32, &F32_COMPARE, ValueType::I32));
    cases.extend(binary_cases(ValueType::F64, &F64_COMPARE, ValueType::I32));
    let (values, traps, nans) = check(&cases, &mut rng, 200);
    assert!(values > 3000, "values: {values}");
    assert_eq!(traps, 0, "float operations do not trap");
    assert!(nans > 100, "NaN results: {nans}");
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
fn compiled_float_negation_and_absolute_value_agree_with_micro() {
    let mut rng = Lcg(0x4141_4141);
    let mut cases = Vec::new();
    for (op, name) in [(0x8B, "f32.abs"), (0x8C, "f32.neg")] {
        cases.push(Case {
            params: vec![ValueType::F32],
            result: ValueType::F32,
            body: vec![0x20, 0x00, op, 0x0B],
            memory: false,
            name: name.into(),
        });
    }
    for (op, name) in [(0x99, "f64.abs"), (0x9A, "f64.neg")] {
        cases.push(Case {
            params: vec![ValueType::F64],
            result: ValueType::F64,
            body: vec![0x20, 0x00, op, 0x0B],
            memory: false,
            name: name.into(),
        });
    }
    let (values, traps, _) = check(&cases, &mut rng, 300);
    assert!(values > 1000, "values: {values}");
    assert_eq!(traps, 0);
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
fn compiled_float_constants_agree_with_micro() {
    let mut rng = Lcg(0x5050_5050);
    let mut cases = Vec::new();
    for _ in 0..24 {
        let bits = rng.f32_bits();
        let mut body = vec![0x43];
        body.extend_from_slice(&bits.to_le_bytes());
        body.push(0x0B);
        cases.push(Case {
            params: Vec::new(),
            result: ValueType::F32,
            body,
            memory: false,
            name: format!("f32.const {bits:#010x}"),
        });
        let bits = rng.f64_bits();
        let mut body = vec![0x44];
        body.extend_from_slice(&bits.to_le_bytes());
        body.push(0x0B);
        cases.push(Case {
            params: Vec::new(),
            result: ValueType::F64,
            body,
            memory: false,
            name: format!("f64.const {bits:#018x}"),
        });
    }
    let (values, traps, _) = check(&cases, &mut rng, 1);
    assert_eq!(values, cases.len());
    assert_eq!(traps, 0);
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
fn compiled_float_memory_round_trips_agree_with_micro() {
    let mut rng = Lcg(0x6060_6060);
    // Store the float argument at the address, then load it back: the result is the loaded value,
    // and the memory is compared too, so a stored byte that is wrong shows even if it reads back.
    let cases = vec![
        Case {
            params: vec![ValueType::I32, ValueType::F32],
            result: ValueType::F32,
            // local.get 0; local.get 1; f32.store 2 0; local.get 0; f32.load 2 0
            body: vec![
                0x20, 0x00, 0x20, 0x01, 0x38, 0x02, 0x00, 0x20, 0x00, 0x2A, 0x02, 0x00, 0x0B,
            ],
            memory: true,
            name: "f32 store then load".into(),
        },
        Case {
            params: vec![ValueType::I32, ValueType::F64],
            result: ValueType::F64,
            // local.get 0; local.get 1; f64.store 3 0; local.get 0; f64.load 3 0
            body: vec![
                0x20, 0x00, 0x20, 0x01, 0x39, 0x03, 0x00, 0x20, 0x00, 0x2B, 0x03, 0x00, 0x0B,
            ],
            memory: true,
            name: "f64 store then load".into(),
        },
    ];
    let (values, traps, _) = check(&cases, &mut rng, 400);
    assert!(values > 100, "values: {values}");
    assert!(traps > 50, "traps: {traps}");
}
