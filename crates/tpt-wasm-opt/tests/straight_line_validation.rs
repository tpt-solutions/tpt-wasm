// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of the straight-line Lean simulation against the Rust engine.
//!
//! `formal/TptWasm/Proofs/V4Straight.lean` proves that a straight-line Wasm body and its
//! lowering end in the same operand values. That proof is about the Lean model. This test ties
//! the model to the Rust implementation: it generates random straight-line bodies over `i32`
//! constants, the six operators the IR lifts, and the ten `i32` comparisons, runs each on the Micro engine for its result,
//! and asks the Lean checker to evaluate the same body two ways: by the Wasm interpreter model
//! and by the lowering model followed by the IR executor. Each verdict is true only when both
//! model runs produce exactly the Micro result.
//!
//! Needs Lean and Lake, so it runs only when `TPT_TRANSLATION_VALIDATION` is set. With
//! `TPT_TRANSLATION_MUTANT` set, the expected results are corrupted, and the test passes only
//! if the checker rejects every corrupted case.
//!
//! This is testing evidence on generated bodies, not a proof that the Rust engine refines the
//! model.

mod common;

use common::{micro_result, push_i32_constant, Rng};

/// The six operators the IR lifts, with their Wasm opcode and Lean constructor.
const OPERATORS: [(u8, &str); 6] = [
    (0x6a, "add"),
    (0x6b, "sub"),
    (0x6c, "mul"),
    (0x71, "and"),
    (0x72, "or"),
    (0x73, "xor"),
];

/// The `i32` comparisons, which lower to the IR's `cmpI32`. Opcode and Lean constructor name.
const COMPARISONS: [(u8, &str); 10] = [
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

/// Pick a binary operator or a comparison, and return its opcode and Lean rendering. A
/// comparison is one time in four.
fn pick_operator(rng: &mut Rng) -> (u8, String) {
    if rng.below(4) == 0 {
        let (opcode, name) = COMPARISONS[rng.below(COMPARISONS.len() as u64) as usize];
        (opcode, format!(".i32Cmp .{name}"))
    } else {
        let (opcode, name) = OPERATORS[rng.below(OPERATORS.len() as u64) as usize];
        (opcode, format!(".i32Bin .{name}"))
    }
}

/// Generate a body that leaves exactly one `i32` on the stack, with its Lean rendering.
fn generate(rng: &mut Rng, steps: usize) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    let mut lean = Vec::new();
    let mut depth = 0usize;
    for _ in 0..steps {
        if depth >= 2 && rng.below(3) != 0 {
            let (opcode, text) = pick_operator(rng);
            bytes.push(opcode);
            lean.push(text);
            depth -= 1;
        } else {
            let value = (rng.next() as i32) >> rng.below(31);
            push_i32_constant(&mut bytes, value);
            lean.push(format!(".i32Const ({value})"));
            depth += 1;
        }
    }
    while depth > 1 {
        let (opcode, text) = pick_operator(rng);
        bytes.push(opcode);
        lean.push(text);
        depth -= 1;
    }
    if depth == 0 {
        push_i32_constant(&mut bytes, 0);
        lean.push(".i32Const (0)".to_owned());
    }
    bytes.push(0x0b); // end
    (bytes, format!("[{}]", lean.join(", ")))
}

#[test]
fn the_straight_line_model_agrees_with_the_micro_engine_on_generated_bodies() {
    if !common::enabled("straight-line validation") {
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
        "def emptyConfig : Configuration := { store := {}, frames := [], operandStack := [], controlStack := [] }\n\n\
         def wasmTop (b : List Instr) : Option (List Value) :=\n  \
         (runWasm b emptyConfig).map (·.1.operandStack)\n\n\
         def irTop (b : List Instr) : Option (List Value) :=\n  \
         match lower b [] [] 0 with\n  \
         | some (ir, final, _, _) =>\n    \
           match IR.runBody ir IR.State.empty [] with\n    \
           | some (s, _) => some (final.map s.values)\n    \
           | none => none\n  \
         | none => none\n\n",
    );

    let mut rng = Rng(0x5eed_0004);
    for case in 0..cases {
        let steps = 2 + rng.below(12) as usize;
        let (bytes, lean) = generate(&mut rng, steps);
        let expected = micro_result(&bytes);
        // The mutant shifts every expectation by one, so each case must be rejected.
        let expected = if mutant {
            expected.wrapping_add(1)
        } else {
            expected
        };
        source.push_str(&format!("def body{case} : List Instr := {lean}\n"));
        source.push_str(&format!(
            "#eval decide (wasmTop body{case} = some [.i32 ({expected})]) && \
             decide (irTop body{case} = some [.i32 ({expected})])\n\n"
        ));
    }

    let verdicts = common::run_lean_checker("StraightLineCases.lean", &source);
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
        "the model disagrees with the Micro engine on {} of {cases} bodies",
        cases as usize - agreeing
    );
    eprintln!("straight-line validation: {cases} bodies agree with the Micro engine");
}
