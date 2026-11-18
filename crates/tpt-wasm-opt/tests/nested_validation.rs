// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of the nested simulation against the Rust engine.
//!
//! `formal/TptWasm/Proofs/Nested/Program.lean` proves that a program of nested `if`/`else`
//! statements and loops, lowered to IR, returns what its Wasm run returns. That proof is about the
//! Lean model. This test ties the model to the Micro engine: it generates random programs whose
//! statements nest `if`/`else` and loops to depth three, runs each as a Wasm function on the Micro
//! engine for its result, and asks the Lean checker to evaluate the same program two ways: by the
//! Wasm model `runSeq` followed by the return expression, and by `lowerProg` followed by the IR
//! executor. A verdict is true only when both model runs produce exactly the Micro result.
//!
//! A loop is encoded as the usual Wasm loop, `loop { cond; if { body; br 1 } }`, and modelled as
//! `whileNZ cond body`. Each loop counts down a counter local of its own, set to a small value
//! before it starts, and decrements it at the top of its body, so every generated loop terminates.
//! Only local 0 is written by the random statements, so the counters are not disturbed.
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

/// How deeply `if`s and loops nest. The model's proof is for any depth; the test uses three.
const MAX_DEPTH: usize = 3;

/// The locals of each program: local 0 holds the result, and local `1 + depth` counts a loop at
/// nesting `depth`.
const LOCALS: usize = MAX_DEPTH + 1;

/// The fuel the Lean evaluations get: enough for every generated loop to finish.
const FUEL: u32 = 5000;

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

/// The statements of a list of between `min` and `max` statements, each appended to `bytes`, as
/// their Lean renderings. `deepest` records the greatest depth at which an `if` or a loop was
/// generated.
fn statements(
    rng: &mut Rng,
    depth: usize,
    bytes: &mut Vec<u8>,
    deepest: &mut usize,
    min: u64,
    max: u64,
) -> Vec<String> {
    let count = min + rng.below(max - min + 1);
    let mut items = Vec::new();
    for _ in 0..count {
        items.extend(statement(rng, depth, bytes, deepest));
    }
    items
}

/// One statement, appended to `bytes`, as its Lean renderings: straight-line assignments to local
/// 0, a void `if`/`else` whose arms are statement lists, or a loop with its counter.
fn statement(rng: &mut Rng, depth: usize, bytes: &mut Vec<u8>, deepest: &mut usize) -> Vec<String> {
    let roll = rng.below(4);
    if depth < MAX_DEPTH && roll == 0 {
        *deepest = (*deepest).max(depth);
        let steps = 1 + rng.below(4) as usize;
        let (cond_bytes, cond_lean) = expression(rng, steps);
        bytes.extend(cond_bytes);
        bytes.extend_from_slice(&[0x04, 0x40]); // if (void)
        let then_items = statements(rng, depth + 1, bytes, deepest, 0, 2);
        bytes.push(0x05); // else
        let else_items = statements(rng, depth + 1, bytes, deepest, 0, 2);
        bytes.push(0x0b); // end of the if
        vec![format!(
            ".ifElse {} {} {}",
            lean_list(&cond_lean),
            lean_list(&then_items),
            lean_list(&else_items)
        )]
    } else if depth < MAX_DEPTH && roll == 1 {
        *deepest = (*deepest).max(depth);
        let counter = 1 + depth;
        let c = counter as u8;
        // the counter starts at a small value, so the loop runs at most three times
        let start = rng.below(4) as i32;
        push_i32_constant(bytes, start);
        bytes.extend_from_slice(&[0x21, c]); // local.set counter
        let mut items = vec![format!(
            ".straight [.i32Const ({start}), .localSet {counter}]"
        )];
        bytes.extend_from_slice(&[0x03, 0x40]); // loop (void)
        bytes.extend_from_slice(&[0x20, c]); // the condition: the counter
        bytes.extend_from_slice(&[0x04, 0x40]); // if (void): taken while the counter is non-zero
        bytes.extend_from_slice(&[0x20, c, 0x41, 0x01, 0x6b, 0x21, c]); // counter := counter - 1
        let body = statements(rng, depth + 1, bytes, deepest, 0, 2);
        bytes.extend_from_slice(&[0x0c, 0x01]); // br 1: back to the loop
        bytes.push(0x0b); // end of the if
        bytes.push(0x0b); // end of the loop
        let mut lean_body = vec![format!(
            ".straight [.localGet {counter}, .i32Const (1), .i32Bin .sub, .localSet {counter}]"
        )];
        lean_body.extend(body);
        items.push(format!(
            ".whileNZ [.localGet {counter}] {}",
            lean_list(&lean_body)
        ));
        items
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
        vec![format!(".straight {}", lean_list(&lean))]
    }
}

/// The `i32` the Micro engine returns for a function with `LOCALS` `i32` locals, all zero.
fn micro_result(body: &[u8]) -> i32 {
    let module = Module {
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

    let zeros = ["Value.i32 0"; LOCALS].join(", ");
    let mut source = String::new();
    source.push_str("import TptWasm.Nested.Program\n\n");
    source.push_str("open TptWasm TptWasm.Nested TptWasm.V4Straight\n\n");
    source.push_str(&format!(
        "def startConfig : Configuration :=\n  \
         {{ store := {{}}, frames := [⟨0, [{zeros}], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }}\n\n\
         def startState : IR.State := {{ values := fun _ => .i32 0, store := {{}} }}\n\n\
         def wasmNest (b : List Stmt) : Option (List Value) :=\n  \
         match runSeq {FUEL} b startConfig with\n  \
         | some (c1, _) => (runWasm [.localGet 0] c1).map (·.1.operandStack)\n  \
         | none => none\n\n\
         def irNest (b : List Stmt) : Option (List Value) :=\n  \
         match lowerProg b [.localGet 0] {LOCALS} with\n  \
         | some f =>\n    \
           match IR.runFunction f {FUEL} startState with\n    \
           | .returned _ values => some values\n    \
           | _ => none\n  \
         | none => none\n\n"
    ));

    let mut rng = Rng(0x5eed_0006);
    let mut results = std::collections::BTreeSet::new();
    let mut deepest = 0usize;
    let mut programs_with_loops = 0u64;
    for case in 0..cases {
        let mut body = Vec::new();
        let items = statements(&mut rng, 0, &mut body, &mut deepest, 1, 3);
        let lean = lean_list(&items);
        if lean.contains(".whileNZ") {
            programs_with_loops += 1;
        }
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

    // The check must not pass vacuously: the programs have to nest, loop, and produce varied
    // results.
    assert!(
        deepest >= 2,
        "no program nested a statement inside an arm or a loop body (deepest {deepest})"
    );
    assert!(
        programs_with_loops >= cases / 10,
        "only {programs_with_loops} of {cases} programs contain a loop"
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
    eprintln!(
        "nested validation: {cases} programs ({programs_with_loops} with loops) agree with the Micro engine"
    );
}
