// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of the nested simulation against the Rust engine.
//!
//! `formal/TptWasm/Proofs/Nested/Program.lean` proves that a program of nested `if`/`else`
//! statements, lowered to IR, returns what its Wasm run returns, and that the machine running the
//! flat layout reaches the same end. That proof is about the Lean model. This test ties the model
//! to the Micro engine: it generates random programs whose statements nest `if`/`else` to depth
//! three, runs each as a Wasm function on the Micro engine for its result, and asks the Lean checker
//! to evaluate the same program two ways: by the Wasm model `runSeq` followed by the return
//! expression, and by `lowerProg` followed by the IR executor. A verdict is true only when both
//! model runs produce exactly the Micro result.
//!
//! Needs Lean and Lake, so it runs only when `TPT_TRANSLATION_VALIDATION` is set. With
//! `TPT_TRANSLATION_MUTANT` set, the expected results are corrupted, and the test passes only if
//! the checker rejects every corrupted case.
//!
//! This is testing evidence on generated programs, not a proof that the Rust engine refines the
//! model.

mod common;

use common::{push_i32_constant, Rng};
use tpt_wasm_format::{Export, ExportDesc, Function, LocalDecl, Module};
use tpt_wasm_runtime::{Config, Engine};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

/// How deeply `if`s nest. The model's proof is for any depth; the test uses three.
const MAX_DEPTH: usize = 3;

/// The binary operators the IR lifts, with their Wasm opcode and Lean constructor.
const BINARY: [(u8, &str); 6] = [
    (0x6a, "add"),
    (0x6b, "sub"),
    (0x6c, "mul"),
    (0x71, "and"),
    (0x72, "or"),
    (0x73, "xor"),
];

/// The `i32` comparisons, with their Wasm opcode and Lean constructor.
const COMPARE: [(u8, &str); 10] = [
    (0x46, "eq"),
    (0x47, "ne"),
    (0x48, "ltS"),
    (0x49, "ltU"),
    (0x4a, "gtS"),
    (0x4b, "gtU"),
    (0x4c, "leS"),
    (0x4d, "leU"),
    (0x4e, "geS"),
    (0x4f, "geU"),
];

/// Append one operator, chosen at random, and its Lean rendering to `bytes` and `lean`.
fn push_operator(rng: &mut Rng, bytes: &mut Vec<u8>, lean: &mut Vec<String>) {
    if rng.below(4) == 0 {
        let (opcode, name) = COMPARE[rng.below(COMPARE.len() as u64) as usize];
        bytes.push(opcode);
        lean.push(format!(".i32Cmp .{name}"));
    } else {
        let (opcode, name) = BINARY[rng.below(BINARY.len() as u64) as usize];
        bytes.push(opcode);
        lean.push(format!(".i32Bin .{name}"));
    }
}

/// An expression that leaves exactly one `i32` on the operand stack, with no `end` opcode. Its
/// leaves are constants and reads of local 0.
fn expression(rng: &mut Rng, steps: usize) -> (Vec<u8>, Vec<String>) {
    let mut bytes = Vec::new();
    let mut lean = Vec::new();
    let mut depth = 0usize;
    for _ in 0..steps {
        if depth >= 2 && rng.below(3) != 0 {
            push_operator(rng, &mut bytes, &mut lean);
            depth -= 1;
        } else if rng.below(3) == 0 {
            bytes.extend_from_slice(&[0x20, 0x00]); // local.get 0
            lean.push(".localGet 0".to_owned());
            depth += 1;
        } else {
            let value = (rng.next() as i32) >> rng.below(31);
            push_i32_constant(&mut bytes, value);
            lean.push(format!(".i32Const ({value})"));
            depth += 1;
        }
    }
    while depth > 1 {
        push_operator(rng, &mut bytes, &mut lean);
        depth -= 1;
    }
    if depth == 0 {
        push_i32_constant(&mut bytes, 0);
        lean.push(".i32Const (0)".to_owned());
    }
    (bytes, lean)
}

fn lean_list(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

/// One statement, appended to `bytes`, and its Lean rendering. A statement is straight-line
/// assignments to local 0, or a void `if`/`else` whose arms are statement lists. `deepest` records
/// the greatest depth at which an `if` was generated.
fn statement(rng: &mut Rng, depth: usize, bytes: &mut Vec<u8>, deepest: &mut usize) -> String {
    if depth < MAX_DEPTH && rng.below(2) == 0 {
        *deepest = (*deepest).max(depth);
        let steps = 1 + rng.below(4) as usize;
        let (cond_bytes, cond_lean) = expression(rng, steps);
        bytes.extend(cond_bytes);
        bytes.extend_from_slice(&[0x04, 0x40]); // if (void)
        let then_lean = statements(rng, depth + 1, bytes, deepest, 0, 2);
        bytes.push(0x05); // else
        let else_lean = statements(rng, depth + 1, bytes, deepest, 0, 2);
        bytes.push(0x0b); // end of the if
        format!(".ifElse {} {then_lean} {else_lean}", lean_list(&cond_lean))
    } else {
        let mut lean = Vec::new();
        for _ in 0..1 + rng.below(2) {
            let steps = 1 + rng.below(5) as usize;
            let (expr_bytes, expr_lean) = expression(rng, steps);
            bytes.extend(expr_bytes);
            bytes.extend_from_slice(&[0x21, 0x00]); // local.set 0
            lean.extend(expr_lean);
            lean.push(".localSet 0".to_owned());
        }
        format!(".straight {}", lean_list(&lean))
    }
}

/// A statement list of between `min` and `max` statements, as a Lean list.
fn statements(
    rng: &mut Rng,
    depth: usize,
    bytes: &mut Vec<u8>,
    deepest: &mut usize,
    min: u64,
    max: u64,
) -> String {
    let count = min + rng.below(max - min + 1);
    let items: Vec<String> = (0..count)
        .map(|_| statement(rng, depth, bytes, deepest))
        .collect();
    lean_list(&items)
}

/// The `i32` the Micro engine returns for a function with one `i32` local, initialised to zero.
fn micro_result(body: &[u8]) -> i32 {
    let module = Module {
        types: vec![FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: vec![LocalDecl {
                count: 1,
                value_type: ValueType::I32,
            }],
            body: body.to_vec(),
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    };
    let engine = Engine::new(Config::default()).expect("the engine is constructible");
    let mut instance = engine
        .instantiate(module)
        .expect("a generated program validates");
    match instance
        .call("run", Vec::new())
        .expect("the program returns")
        .as_slice()
    {
        [Value::I32(value)] => *value,
        other => panic!("the program returned {other:?}, not one i32"),
    }
}

#[test]
fn the_nested_model_agrees_with_the_micro_engine_on_generated_programs() {
    if !common::enabled("nested validation") {
        return;
    }
    let mutant = common::mutant();
    let cases: u64 = std::env::var("TPT_TRANSLATION_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);

    let mut source = String::new();
    source.push_str("import TptWasm.Nested.Program\n\n");
    source.push_str("open TptWasm TptWasm.Nested TptWasm.V4Straight\n\n");
    source.push_str(
        "def startConfig : Configuration :=\n  \
         { store := {}, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }\n\n\
         def startState : IR.State := { values := fun _ => .i32 0, store := {} }\n\n\
         def wasmNest (b : List Stmt) : Option (List Value) :=\n  \
         match runSeq b startConfig with\n  \
         | some (c1, _) => (runWasm [.localGet 0] c1).map (·.1.operandStack)\n  \
         | none => none\n\n\
         def irNest (b : List Stmt) : Option (List Value) :=\n  \
         match lowerProg b [.localGet 0] 1 with\n  \
         | some f =>\n    \
           match IR.runFunction f 400 startState with\n    \
           | .returned _ values => some values\n    \
           | _ => none\n  \
         | none => none\n\n",
    );

    let mut rng = Rng(0x5eed_0006);
    let mut results = std::collections::BTreeSet::new();
    let mut deepest = 0usize;
    for case in 0..cases {
        let mut body = Vec::new();
        let lean = statements(&mut rng, 0, &mut body, &mut deepest, 1, 3);
        body.extend_from_slice(&[0x20, 0x00]); // local.get 0: the return expression
        body.push(0x0b); // end of the function
        let expected = micro_result(&body);
        results.insert(expected);
        // The mutant shifts every expectation by one, so each case must be rejected.
        let expected = if mutant {
            expected.wrapping_add(1)
        } else {
            expected
        };
        source.push_str(&format!("def prog{case} : List Stmt := {lean}\n"));
        source.push_str(&format!(
            "#eval decide (wasmNest prog{case} = some [.i32 ({expected})]) && \
             decide (irNest prog{case} = some [.i32 ({expected})])\n\n"
        ));
    }

    // The check must not pass vacuously: the programs have to nest, and produce varied results.
    assert!(
        deepest >= 2,
        "no program nested an `if` inside an arm of another (deepest {deepest})"
    );
    assert!(
        results.len() >= 10,
        "the generated programs produced only {} distinct results",
        results.len()
    );

    let verdicts = common::run_lean_checker("NestedCases.lean", &source);
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
        "the nested model disagrees with the Micro engine on {} of {cases} programs",
        cases as usize - agreeing
    );
    eprintln!("nested validation: {cases} programs agree with the Micro engine");
}
