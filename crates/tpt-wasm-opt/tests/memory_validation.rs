// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of the Lean model's memory semantics against the Micro engine.
//!
//! The model's loads and stores take a static `memarg` offset, and the effective address is the
//! operand's unsigned 32-bit pattern plus that offset, without wrapping. This test checks that
//! against the Micro engine. Each body stores some `i32` values into a one-page memory, then
//! loads several of them and sums the results. Addresses and offsets are chosen so every access
//! is in bounds, so no body traps, and the result depends on which bytes each store wrote and
//! each load read. The Micro engine gives the expected value, and the Lean model's `runWasm`
//! must reproduce it from a zeroed memory.
//!
//! Needs Lean and Lake, so it runs only when `TPT_TRANSLATION_VALIDATION` is set.

mod common;

use common::{push_i32_constant, Rng};
use tpt_wasm_format::Memory;
use tpt_wasm_types::{Limits, MemoryType};

/// Append an unsigned LEB128 immediate.
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

/// Append a memory instruction's `memarg`: alignment hint zero, then the offset.
fn push_memarg(bytes: &mut Vec<u8>, offset: u32) {
    bytes.push(0x00);
    push_unsigned_leb(bytes, offset);
}

/// The stores: opcode, and the Lean `StoreOp` constructor.
const STORES: [(u8, &str); 3] = [(0x36, "i32"), (0x3a, "i32_8"), (0x3b, "i32_16")];

/// The loads: opcode, and the Lean `LoadOp` constructor.
const LOADS: [(u8, &str); 5] = [
    (0x28, "i32"),
    (0x2c, "i32_8s"),
    (0x2d, "i32_8u"),
    (0x2e, "i32_16s"),
    (0x2f, "i32_16u"),
];

/// An address for an access. Most come from a small window, so that stores and loads overlap and
/// the loaded bytes reach the result; one in four comes from the whole page.
fn access_address(rng: &mut Rng) -> i32 {
    if rng.below(4) == 0 {
        rng.below(65_000) as i32
    } else {
        rng.below(8) as i32
    }
}

/// Generate a body that stores a few values and then sums several loads, with its Lean
/// rendering. Each address plus offset stays inside the one page.
fn generate(rng: &mut Rng) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    let mut lean = Vec::new();
    for _ in 0..1 + rng.below(3) {
        let (opcode, name) = STORES[rng.below(STORES.len() as u64) as usize];
        let address = access_address(rng);
        let offset = rng.below(4) as u32;
        let value = rng.next() as i32;
        push_i32_constant(&mut bytes, address);
        push_i32_constant(&mut bytes, value);
        bytes.push(opcode);
        push_memarg(&mut bytes, offset);
        lean.push(format!(".i32Const ({address})"));
        lean.push(format!(".i32Const ({value})"));
        lean.push(format!(".store .{name} 0 {{ offset := {offset} }}"));
    }
    let loads = 1 + rng.below(3);
    for index in 0..loads {
        let (opcode, name) = LOADS[rng.below(LOADS.len() as u64) as usize];
        let address = access_address(rng);
        let offset = rng.below(4) as u32;
        push_i32_constant(&mut bytes, address);
        bytes.push(opcode);
        push_memarg(&mut bytes, offset);
        lean.push(format!(".i32Const ({address})"));
        lean.push(format!(".load .{name} 0 {{ offset := {offset} }}"));
        if index > 0 {
            bytes.push(0x6a); // i32.add
            lean.push(".i32Bin .add".to_owned());
        }
    }
    bytes.push(0x0b); // end
    (bytes, format!("[{}]", lean.join(", ")))
}

/// The module with one page of memory and a `run` function over `body`.
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

#[test]
fn the_memory_model_agrees_with_the_micro_engine_on_generated_accesses() {
    if !common::enabled("memory validation") {
        return;
    }
    let mutant = common::mutant();
    let cases: u64 = std::env::var("TPT_TRANSLATION_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(150);

    let mut source = String::new();
    source.push_str("import TptWasm.Proofs.V4Straight\n\n");
    source.push_str("open TptWasm TptWasm.V4Straight\n\n");
    source.push_str(
        "def memConfig : Configuration :=\n  \
         { store := { memories := [{ bytes := List.replicate 65536 0, maxPages := 1 }] },\n    \
         frames := [], operandStack := [], controlStack := [] }\n\n\
         def memTop (b : List Instr) : Option (List Value) :=\n  \
         (runWasm b memConfig).map (·.1.operandStack)\n\n\
         def memStore : Store := { memories := [{ bytes := List.replicate 65536 0, maxPages := 1 }] }\n\n\
         /-- The lowered body, run by the IR executor over the same zeroed memory. -/\n\
         def irMemTop (b : List Instr) : Option (List Value) :=\n  \
         match lower b [] [] 0 with\n  \
         | some (ir, final, _, _) =>\n    \
           match IR.runBody ir { IR.State.empty with store := memStore } [] with\n    \
           | some (s, _) => some (final.map s.values)\n    \
           | none => none\n  \
         | none => none\n\n",
    );

    let mut rng = Rng(0x5eed_0006);
    let mut distinct = std::collections::BTreeSet::new();
    for case in 0..cases {
        let (bytes, lean) = generate(&mut rng);
        let expected = {
            // `micro_result` runs a module without memory, so the memory module is run here.
            let engine = tpt_wasm_runtime::Engine::new(tpt_wasm_runtime::Config::default())
                .expect("the engine is constructible");
            let mut instance = engine
                .instantiate(module_with_memory(&bytes))
                .expect("a generated body validates");
            match instance
                .call("run", Vec::new())
                .expect("the body returns")
                .as_slice()
            {
                [tpt_wasm_types::Value::I32(value)] => *value,
                other => panic!("the body returned {other:?}, not one i32"),
            }
        };
        distinct.insert(expected);
        let expected = if mutant {
            expected.wrapping_add(1)
        } else {
            expected
        };
        source.push_str(&format!("def body{case} : List Instr := {lean}\n"));
        source.push_str(&format!(
            "#eval decide (memTop body{case} = some [.i32 ({expected})]) && \
             decide (irMemTop body{case} = some [.i32 ({expected})])\n\n"
        ));
    }
    assert!(
        distinct.len() > (cases as usize) / 4,
        "the accesses produced only {} distinct results",
        distinct.len()
    );

    let verdicts = common::run_lean_checker("MemoryCases.lean", &source);
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
    eprintln!("memory validation: {cases} bodies agree with the Micro engine");
}
