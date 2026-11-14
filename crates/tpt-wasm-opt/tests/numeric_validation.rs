// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of the Lean model's integer semantics against the Micro engine.
//!
//! The straight-line test covers only the six operators the IR lifts. This one covers the rest
//! of the integer set the Lean model defines: all `i32` binary operators, the `i32` and `i64`
//! comparisons, the bit-count operators, `eqz`, and the width conversions. The bodies are
//! random expression trees, serialized in postfix order, so every body is well typed by
//! construction. Each one leaves a single `i32`, which the Micro engine evaluates for the
//! expected value, and which the Lean model's `runWasm` must reproduce.
//!
//! Divisors are never zero or `-1`, so no body traps: the model has no trap rule for the
//! overflowing `INT_MIN / -1`, and that case is left out rather than modelled here.
//!
//! This test is what found that the model's `natAbs` treatment of unsigned operations was
//! wrong, so it is the check that keeps those corrections honest. It needs Lean, so it runs
//! only when `TPT_TRANSLATION_VALIDATION` is set.

mod common;

use common::{micro_result, push_i32_constant, push_i64_constant, Rng};

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    I32,
    I64,
}

/// The bytes and the Lean rendering of one body, built together so they cannot disagree.
struct Code {
    bytes: Vec<u8>,
    lean: Vec<String>,
}

/// `i32` binary operators: opcode and Lean constructor name.
const I32_BIN: [(u8, &str); 15] = [
    (0x6a, "add"),
    (0x6b, "sub"),
    (0x6c, "mul"),
    (0x6d, "divS"),
    (0x6e, "divU"),
    (0x6f, "remS"),
    (0x70, "remU"),
    (0x71, "and"),
    (0x72, "or"),
    (0x73, "xor"),
    (0x74, "shl"),
    (0x75, "shrS"),
    (0x76, "shrU"),
    (0x77, "rotl"),
    (0x78, "rotr"),
];

/// `i32` comparisons, which produce an `i32`.
const I32_CMP: [(u8, &str); 10] = [
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

/// `i32` unary operators that produce an `i32`.
const I32_UN: [(u8, &str); 3] = [(0x67, "clz"), (0x68, "ctz"), (0x69, "popcnt")];

/// `i64` binary operators the model defines (its division is left out of the model).
const I64_BIN: [(u8, &str); 8] = [
    (0x7c, "add"),
    (0x7d, "sub"),
    (0x7e, "mul"),
    (0x83, "and"),
    (0x84, "or"),
    (0x85, "xor"),
    (0x86, "shl"),
    (0x88, "shrU"),
];

/// `i64` comparisons, which produce an `i32`.
const I64_CMP: [(u8, &str); 10] = [
    (0x51, "eq"),
    (0x52, "ne"),
    (0x53, "ltS"),
    (0x54, "ltU"),
    (0x55, "gtS"),
    (0x56, "gtU"),
    (0x57, "leS"),
    (0x58, "leU"),
    (0x59, "geS"),
    (0x5a, "geU"),
];

/// `i64` unary operators that produce an `i64`. `eqz` is separate, since it produces an `i32`.
const I64_UN: [(u8, &str); 3] = [(0x79, "clz"), (0x7a, "ctz"), (0x7b, "popcnt")];

const I64_EQZ: u8 = 0x50;
const I32_WRAP_I64: u8 = 0xa7;
const I64_EXTEND_I32_S: u8 = 0xac;
const I64_EXTEND_I32_U: u8 = 0xad;

/// Values that exercise the edges as often as ordinary ones.
fn random_i32(rng: &mut Rng) -> i32 {
    match rng.below(8) {
        0 => 0,
        1 => -1,
        2 => i32::MIN,
        3 => i32::MAX,
        4 => rng.below(64) as i32 - 32,
        _ => rng.next() as i32,
    }
}

fn random_i64(rng: &mut Rng) -> i64 {
    match rng.below(8) {
        0 => 0,
        1 => -1,
        2 => i64::MIN,
        3 => i64::MAX,
        4 => rng.below(64) as i64 - 32,
        _ => rng.next() as i64,
    }
}

fn constant_i32(rng: &mut Rng, code: &mut Code) {
    let value = random_i32(rng);
    push_i32_constant(&mut code.bytes, value);
    code.lean.push(format!(".i32Const ({value})"));
}

fn constant_i64(rng: &mut Rng, code: &mut Code) {
    let value = random_i64(rng);
    push_i64_constant(&mut code.bytes, value);
    code.lean.push(format!(".i64Const ({value})"));
}

/// A divisor that is neither zero nor `-1`, so the division cannot trap.
fn divisor_i32(rng: &mut Rng, code: &mut Code) {
    let value = loop {
        let candidate = random_i32(rng);
        if candidate != 0 && candidate != -1 {
            break candidate;
        }
    };
    push_i32_constant(&mut code.bytes, value);
    code.lean.push(format!(".i32Const ({value})"));
}

fn is_division(name: &str) -> bool {
    matches!(name, "divS" | "divU" | "remS" | "remU")
}

/// Append a random well-typed expression of type `ty`, nested to at most `depth` operators.
fn generate(rng: &mut Rng, code: &mut Code, ty: Ty, depth: u32) {
    if depth == 0 || rng.below(4) == 0 {
        match ty {
            Ty::I32 => constant_i32(rng, code),
            Ty::I64 => constant_i64(rng, code),
        }
        return;
    }
    match ty {
        Ty::I32 => match rng.below(5) {
            0 | 1 => {
                let (opcode, name) = I32_BIN[rng.below(I32_BIN.len() as u64) as usize];
                generate(rng, code, Ty::I32, depth - 1);
                if is_division(name) {
                    divisor_i32(rng, code);
                } else {
                    generate(rng, code, Ty::I32, depth - 1);
                }
                code.bytes.push(opcode);
                code.lean.push(format!(".i32Bin .{name}"));
            }
            2 => {
                if rng.below(2) == 0 {
                    let (opcode, name) = I32_CMP[rng.below(I32_CMP.len() as u64) as usize];
                    generate(rng, code, Ty::I32, depth - 1);
                    generate(rng, code, Ty::I32, depth - 1);
                    code.bytes.push(opcode);
                    code.lean.push(format!(".i32Cmp .{name}"));
                } else {
                    let (opcode, name) = I64_CMP[rng.below(I64_CMP.len() as u64) as usize];
                    generate(rng, code, Ty::I64, depth - 1);
                    generate(rng, code, Ty::I64, depth - 1);
                    code.bytes.push(opcode);
                    code.lean.push(format!(".i64Cmp .{name}"));
                }
            }
            3 => {
                if rng.below(2) == 0 {
                    let (opcode, name) = I32_UN[rng.below(I32_UN.len() as u64) as usize];
                    generate(rng, code, Ty::I32, depth - 1);
                    code.bytes.push(opcode);
                    code.lean.push(format!(".i32Un .{name}"));
                } else {
                    generate(rng, code, Ty::I64, depth - 1);
                    code.bytes.push(I64_EQZ);
                    code.lean.push(".i64Un .eqz".to_owned());
                }
            }
            _ => {
                generate(rng, code, Ty::I64, depth - 1);
                code.bytes.push(I32_WRAP_I64);
                code.lean.push(".i32WrapI64".to_owned());
            }
        },
        Ty::I64 => match rng.below(4) {
            0 | 1 => {
                let (opcode, name) = I64_BIN[rng.below(I64_BIN.len() as u64) as usize];
                generate(rng, code, Ty::I64, depth - 1);
                generate(rng, code, Ty::I64, depth - 1);
                code.bytes.push(opcode);
                code.lean.push(format!(".i64Bin .{name}"));
            }
            2 => {
                let (opcode, name) = I64_UN[rng.below(I64_UN.len() as u64) as usize];
                generate(rng, code, Ty::I64, depth - 1);
                code.bytes.push(opcode);
                code.lean.push(format!(".i64Un .{name}"));
            }
            _ => {
                generate(rng, code, Ty::I32, depth - 1);
                if rng.below(2) == 0 {
                    code.bytes.push(I64_EXTEND_I32_S);
                    code.lean.push(".i64ExtendI32S".to_owned());
                } else {
                    code.bytes.push(I64_EXTEND_I32_U);
                    code.lean.push(".i64ExtendI32U".to_owned());
                }
            }
        },
    }
}

#[test]
fn the_integer_model_agrees_with_the_micro_engine_on_generated_expressions() {
    if !common::enabled("numeric validation") {
        return;
    }
    let mutant = common::mutant();
    let cases: u64 = std::env::var("TPT_TRANSLATION_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(400);

    let mut source = String::new();
    source.push_str("import TptWasm.Proofs.V4Straight\n\n");
    source.push_str("open TptWasm TptWasm.V4Straight\n\n");
    source.push_str(
        "def emptyConfig : Configuration := { store := {}, frames := [], operandStack := [], controlStack := [] }\n\n\
         def wasmTop (b : List Instr) : Option (List Value) :=\n  \
         (runWasm b emptyConfig).map (·.1.operandStack)\n\n",
    );

    let mut rng = Rng(0x5eed_0005);
    let mut distinct = std::collections::BTreeSet::new();
    for case in 0..cases {
        let mut code = Code {
            bytes: Vec::new(),
            lean: Vec::new(),
        };
        let depth = 2 + rng.below(4) as u32;
        generate(&mut rng, &mut code, Ty::I32, depth);
        code.bytes.push(0x0b); // end
        let expected = micro_result(&code.bytes);
        distinct.insert(expected);
        // The mutant shifts every expectation by one, so each case must be rejected.
        let expected = if mutant {
            expected.wrapping_add(1)
        } else {
            expected
        };
        source.push_str(&format!(
            "def body{case} : List Instr := [{}]\n",
            code.lean.join(", ")
        ));
        source.push_str(&format!(
            "#eval decide (wasmTop body{case} = some [.i32 ({expected})])\n\n"
        ));
    }
    // Guard against a vacuous pass: the expressions must produce varied results.
    assert!(
        distinct.len() > (cases as usize) / 4,
        "the expressions produced only {} distinct results",
        distinct.len()
    );

    let verdicts = common::run_lean_checker("NumericCases.lean", &source);
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
        "the model disagrees with the Micro engine on {} of {cases} expressions",
        cases as usize - agreeing
    );
    eprintln!("numeric validation: {cases} expressions agree with the Micro engine");
}
