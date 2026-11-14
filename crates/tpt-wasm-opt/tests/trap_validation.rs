// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of trap behaviour: the lowered IR traps exactly when the Micro engine
//! traps, and the Lean model's Wasm run traps exactly then too.
//!
//! The bodies are memory stores and loads like those of `memory_validation.rs`, but some
//! addresses fall outside the one page, so the access traps. For each body the Micro engine
//! gives either a value or a trap. The Lean model is then asked for three things: the Wasm
//! model's run, the lowering run by the IR executor, and the expected outcome. Both model runs
//! must be `none` exactly when Micro traps, and produce Micro's value otherwise.
//!
//! This is evidence about the lowering's trap behaviour, not a proof of trap position: the
//! simulation in `V4Straight.lean` covers successful runs only. Needs Lean and Lake, so it runs
//! only when `TPT_TRANSLATION_VALIDATION` is set.

mod common;

use common::{push_i32_constant, Rng};
use tpt_wasm_format::Memory;
use tpt_wasm_types::{Limits, MemoryType, Value};

const STORES: [(u8, &str); 3] = [(0x36, "i32"), (0x3a, "i32_8"), (0x3b, "i32_16")];
const LOADS: [(u8, &str); 5] = [
    (0x28, "i32"),
    (0x2c, "i32_8s"),
    (0x2d, "i32_8u"),
    (0x2e, "i32_16s"),
    (0x2f, "i32_16u"),
];

fn push_unsigned_leb(bytes: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            bytes.push(byte);
            return;
        }
        bytes.push(byte | 0x80);
    }
}

/// An address that is usually in the page and sometimes past its end.
fn address(rng: &mut Rng) -> i32 {
    if rng.below(3) == 0 {
        65_500 + rng.below(40) as i32
    } else {
        rng.below(64) as i32
    }
}

/// Generate a store-then-load body, with its Lean rendering.
fn generate(rng: &mut Rng) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    let mut lean = Vec::new();
    for _ in 0..1 + rng.below(3) {
        let (opcode, name) = STORES[rng.below(STORES.len() as u64) as usize];
        let addr = address(rng);
        let offset = rng.below(8) as u32;
        let value = rng.next() as i32;
        push_i32_constant(&mut bytes, addr);
        push_i32_constant(&mut bytes, value);
        bytes.push(opcode);
        bytes.push(0x00);
        push_unsigned_leb(&mut bytes, offset);
        lean.push(format!(".i32Const ({addr})"));
        lean.push(format!(".i32Const ({value})"));
        lean.push(format!(".store .{name} 0 {{ offset := {offset} }}"));
    }
    let loads = 1 + rng.below(3);
    for index in 0..loads {
        let (opcode, name) = LOADS[rng.below(LOADS.len() as u64) as usize];
        let addr = address(rng);
        let offset = rng.below(8) as u32;
        push_i32_constant(&mut bytes, addr);
        bytes.push(opcode);
        bytes.push(0x00);
        push_unsigned_leb(&mut bytes, offset);
        lean.push(format!(".i32Const ({addr})"));
        lean.push(format!(".load .{name} 0 {{ offset := {offset} }}"));
        if index > 0 {
            bytes.push(0x6a);
            lean.push(".i32Bin .add".to_owned());
        }
    }
    bytes.push(0x0b);
    (bytes, format!("[{}]", lean.join(", ")))
}

fn module_with_memory(body: &[u8]) -> tpt_wasm_format::Module {
    let mut module = common::module_of(body);
    module.memories = vec![Memory {
        memory_type: MemoryType {
            limits: Limits {
                min: 1,
                max: Some(1),
            },
            memory64: false,
        },
    }];
    module
}

/// The Micro engine's outcome: `Some(value)`, or `None` when the body traps.
fn micro_outcome(body: &[u8]) -> Option<i32> {
    let engine = tpt_wasm_runtime::Engine::new(tpt_wasm_runtime::Config::default())
        .expect("the engine is constructible");
    let mut instance = engine
        .instantiate(module_with_memory(body))
        .expect("a generated body validates");
    match instance.call("run", Vec::new()) {
        Ok(values) => match values.as_slice() {
            [Value::I32(value)] => Some(*value),
            other => panic!("the body returned {other:?}, not one i32"),
        },
        Err(_) => None,
    }
}

#[test]
fn the_lowered_and_model_runs_trap_exactly_when_micro_traps() {
    if !common::enabled("trap validation") {
        return;
    }
    let mutant = common::mutant();
    let cases: u64 = std::env::var("TPT_TRANSLATION_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);

    let mut source = String::new();
    source.push_str("import TptWasm.Proofs.V4Straight\n\n");
    source.push_str("open TptWasm TptWasm.V4Straight\n\n");
    source.push_str(
        "def trapStore : Store := { memories := [{ bytes := List.replicate 65536 0, maxPages := 1 }] }\n\n\
         def trapConfig : Configuration :=\n  \
         { store := trapStore, frames := [], operandStack := [], controlStack := [] }\n\n\
         /-- The Wasm model's outcome: the operand stack, or `none` for a trap. -/\n\
         def modelOutcome (b : List Instr) : Option (List Value) :=\n  \
         (runWasm b trapConfig).map (·.1.operandStack)\n\n\
         /-- The lowered body's outcome, run by the IR executor over the same memory. -/\n\
         def irOutcome (b : List Instr) : Option (List Value) :=\n  \
         match lower b [] [] 0 with\n  \
         | some (ir, final, _, _) =>\n    \
           match IR.runBody ir { IR.State.empty with store := trapStore } [] with\n    \
           | some (s, _) => some (final.map s.values)\n    \
           | none => none\n  \
         | none => none\n\n",
    );

    let mut rng = Rng(0x5eed_000a);
    let mut traps = 0usize;
    let mut values = 0usize;
    for case in 0..cases {
        let (bytes, lean) = generate(&mut rng);
        let expected = micro_outcome(&bytes);
        // The mutant flips every expected outcome, so each case must be rejected.
        let expected = if mutant {
            match expected {
                Some(_) => None,
                None => Some(0),
            }
        } else {
            expected
        };
        match expected {
            Some(_) => values += 1,
            None => traps += 1,
        }
        let rendering = match expected {
            Some(v) => format!("some [.i32 ({v})]"),
            None => "none".to_owned(),
        };
        source.push_str(&format!("def body{case} : List Instr := {lean}\n"));
        source.push_str(&format!(
            "#eval decide (modelOutcome body{case} = {rendering}) && \
             decide (irOutcome body{case} = {rendering})\n\n"
        ));
    }
    assert!(
        traps > 0 && values > 0,
        "the bodies must both trap and complete: {traps} traps, {values} values"
    );

    let verdicts = common::run_lean_checker("TrapCases.lean", &source);
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
            "{agreeing} of {cases} flipped outcomes were accepted; the check cannot fail"
        );
        eprintln!("mutant check: all {cases} flipped outcomes are rejected");
        return;
    }
    assert_eq!(
        agreeing as u64,
        cases,
        "the model disagrees with the Micro engine on {} of {cases} bodies",
        cases as usize - agreeing
    );
    eprintln!(
        "trap validation: {cases} bodies agree with the Micro engine ({traps} trap, {values} complete)"
    );
}
