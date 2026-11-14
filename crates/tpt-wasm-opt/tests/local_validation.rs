// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of the lowered local and global accesses against the Micro engine.
//!
//! The simulation in `formal/TptWasm/Proofs/V4Straight.lean` covers `local.get`, `local.set`,
//! `local.tee`, `global.get`, and `global.set`. Locals need no IR instruction: the lowering keeps
//! the value id each local holds. This test runs bodies that read and write three `i32` locals and
//! two `i32` globals, two ways: the Micro engine, and the Lean model's lowering followed by the IR
//! executor, over a state whose three locals are the ids `0, 1, 2` and whose globals are zero.
//! Needs Lean and Lake, so it runs only when `TPT_TRANSLATION_VALIDATION` is set.

mod common;

use common::{push_i32_constant, Rng};
use tpt_wasm_format::{ConstExpr, Function, Global, LocalDecl, Module};
use tpt_wasm_types::{FunctionType, GlobalType, ResultType, Value, ValueType};

const LOCALS: u64 = 3;
const GLOBALS: u64 = 2;
const BINARY: [(u8, &str); 3] = [(0x6a, "add"), (0x6b, "sub"), (0x73, "xor")];

/// Generate a body that leaves one `i32`, with its Lean rendering.
fn generate(rng: &mut Rng, steps: usize) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    let mut lean = Vec::new();
    let mut depth = 0usize;
    for _ in 0..steps {
        match rng.below(8) {
            0 | 1 => {
                let value = (rng.next() as i32) >> rng.below(31);
                push_i32_constant(&mut bytes, value);
                lean.push(format!(".i32Const ({value})"));
                depth += 1;
            }
            2 => {
                let index = rng.below(LOCALS);
                bytes.extend([0x20, index as u8]);
                lean.push(format!(".localGet {index}"));
                depth += 1;
            }
            3 => {
                let index = rng.below(GLOBALS);
                bytes.extend([0x23, index as u8]);
                lean.push(format!(".globalGet {index}"));
                depth += 1;
            }
            4 if depth >= 1 => {
                let index = rng.below(LOCALS);
                if rng.below(2) == 0 {
                    bytes.extend([0x21, index as u8]);
                    lean.push(format!(".localSet {index}"));
                    depth -= 1;
                } else {
                    bytes.extend([0x22, index as u8]);
                    lean.push(format!(".localTee {index}"));
                }
            }
            5 if depth >= 1 => {
                let index = rng.below(GLOBALS);
                bytes.extend([0x24, index as u8]);
                lean.push(format!(".globalSet {index}"));
                depth -= 1;
            }
            _ if depth >= 2 => {
                let (opcode, name) = BINARY[rng.below(BINARY.len() as u64) as usize];
                bytes.push(opcode);
                lean.push(format!(".i32Bin .{name}"));
                depth -= 1;
            }
            _ => {}
        }
    }
    while depth > 1 {
        let (opcode, name) = BINARY[rng.below(BINARY.len() as u64) as usize];
        bytes.push(opcode);
        lean.push(format!(".i32Bin .{name}"));
        depth -= 1;
    }
    if depth == 0 {
        push_i32_constant(&mut bytes, 0);
        lean.push(".i32Const (0)".to_owned());
    }
    bytes.push(0x0b); // end
    (bytes, format!("[{}]", lean.join(", ")))
}

/// The module: `run` returns one `i32`, with three `i32` locals and two mutable `i32` globals.
fn module_of(body: &[u8]) -> Module {
    let global = Global {
        global_type: GlobalType {
            value_type: ValueType::I32,
            mutable: true,
        },
        init: ConstExpr(vec![0x41, 0, 0x0b]),
    };
    Module {
        types: vec![FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: vec![LocalDecl {
                count: LOCALS as u32,
                value_type: ValueType::I32,
            }],
            body: body.to_vec(),
        }],
        globals: vec![global.clone(), global],
        exports: vec![tpt_wasm_format::Export {
            name: "run".into(),
            desc: tpt_wasm_format::ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

fn micro_result(body: &[u8]) -> i32 {
    let engine = tpt_wasm_runtime::Engine::new(tpt_wasm_runtime::Config::default())
        .expect("the engine is constructible");
    let mut instance = engine
        .instantiate(module_of(body))
        .expect("a generated body validates");
    match instance
        .call("run", Vec::new())
        .expect("the body returns")
        .as_slice()
    {
        [Value::I32(value)] => *value,
        other => panic!("the body returned {other:?}, not one i32"),
    }
}

#[test]
fn the_lowered_local_and_global_accesses_agree_with_the_micro_engine() {
    if !common::enabled("local lowering validation") {
        return;
    }
    let mutant = common::mutant();
    let cases: u64 = std::env::var("TPT_TRANSLATION_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300);

    let mut source = String::new();
    source.push_str("import TptWasm.Proofs.V4Straight\n\n");
    source.push_str("open TptWasm TptWasm.V4Straight\n\n");
    source.push_str(
        "def localStore : Store := { globals := [{ ty := .i32, value := .i32 0 }, { ty := .i32, value := .i32 0 }] }\n\n\
         /-- The three locals are the ids 0, 1, 2, all zero; the fresh ids start at 3. -/\n\
         def localState : IR.State := { values := fun _ => .i32 0, store := localStore }\n\n\
         /-- The lowering, run by the IR executor over the same state. -/\n\
         def irLocalTop (b : List Instr) : Option (List Value) :=\n  \
         match lower b [] [0, 1, 2] 3 with\n  \
         | some (ir, final, _, _) =>\n    \
           match IR.runBody ir localState [] with\n    \
           | some (s, _) => some (final.map s.values)\n    \
           | none => none\n  \
         | none => none\n\n",
    );

    let mut rng = Rng(0x5eed_0009);
    let mut distinct = std::collections::BTreeSet::new();
    for case in 0..cases {
        let steps = 4 + rng.below(16) as usize;
        let (bytes, lean) = generate(&mut rng, steps);
        let expected = micro_result(&bytes);
        distinct.insert(expected);
        let expected = if mutant {
            expected.wrapping_add(1)
        } else {
            expected
        };
        source.push_str(&format!("def body{case} : List Instr := {lean}\n"));
        source.push_str(&format!(
            "#eval decide (irLocalTop body{case} = some [.i32 ({expected})])\n\n"
        ));
    }
    assert!(
        distinct.len() > (cases as usize) / 4,
        "the bodies produced only {} distinct results",
        distinct.len()
    );

    let verdicts = common::run_lean_checker("LocalCases.lean", &source);
    assert_eq!(
        verdicts.len() as u64,
        cases,
        "one verdict per case; got {}",
        verdicts.len()
    );
    let agreeing = verdicts.iter().filter(|verdict| *verdict == "true").count();
    if mutant {
        assert_eq!(
            agreeing, 0,
            "{agreeing} of {cases} corrupted expectations were accepted; the check cannot fail"
        );
        eprintln!("mutant check: all {cases} corrupted expectations are rejected");
        return;
    }
    assert_eq!(
        agreeing as u64,
        cases,
        "the lowering disagrees with the Micro engine on {} of {cases} bodies",
        cases as usize - agreeing
    );
    eprintln!("local lowering validation: {cases} bodies agree with the Micro engine");
}
