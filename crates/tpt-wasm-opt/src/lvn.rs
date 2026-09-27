// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Local value numbering and common subexpression elimination.
//!
//! ## Precondition
//!
//! Verified IR.
//!
//! ## Transformation
//!
//! * **LVN** numbers expressions within one block. Two instructions that compute
//!   the same thing get the same number, and the second is replaced by a
//!   `local.get`-free reference to the first's result -- concretely, every use of
//!   the duplicate's result is rewritten to the original's, and the duplicate is
//!   then dead. No copy instruction is needed, and none is added: the IR is
//!   single-assignment, so a use may name any dominating definition directly.
//! * **CSE** is the same idea across blocks, restricted to expressions available
//!   along the dominator tree. A value from a non-dominating block is *not*
//!   reused, which is what keeps the rewrite dominance-safe.
//!
//! ## Semantic invariant
//!
//! The computed values are identical, and the program takes the same traps in
//! the same order.
//!
//! Two restrictions do the work:
//!
//! * Only **pure and total** expressions participate. An expression that can
//!   trap or that has an effect is never treated as a redundant computation, so
//!   eliminating one cannot remove a trap or a store. A `div_s` by a
//!   non-constant divisor is therefore never eliminated even when the identical
//!   expression appears twice -- the second one could trap for a reason the first
//!   did not, if their operands were different values that happen to look alike.
//! * Operands are compared by **identity**, and the expression key sorts
//!   operands only for operations Wasm defines as commutative. `a - b` and
//!   `b - a` are different computations and get different keys.
//!
//! Expression keys are built from a structural match rather than a formatted
//! string, so a new IR variant cannot accidentally collide with an existing one:
//! an unlisted variant produces no key at all and simply does not participate.

use std::collections::HashMap;

use tpt_wasm_ir::{IrFunction, IrInstr, IrModule, ValueId};

use crate::analysis::{
    self, defined_values, has_side_effects, map_operands, may_trap, Cfg, Dominators,
};
use crate::PassOutcome;

/// Number expressions within each block and eliminate duplicates.
pub fn run(module: &mut IrModule) -> PassOutcome {
    let mut removed = 0;
    for function in &mut module.functions {
        removed += eliminate_in_blocks(function);
    }
    PassOutcome {
        instructions_removed: removed,
        ..PassOutcome::default()
    }
}

/// Eliminate redundancy across blocks, along the dominator tree.
pub fn run_global(module: &mut IrModule) -> PassOutcome {
    let mut removed = 0;
    for function in &mut module.functions {
        removed += eliminate_across_blocks(function);
    }
    PassOutcome {
        instructions_removed: removed,
        ..PassOutcome::default()
    }
}

/// A structural key identifying "the same computation".
///
/// Derived from the instruction's own fields rather than its `Debug` output, so
/// it is stable, allocation-light, and cannot be perturbed by a change to a
/// derived `Debug` impl.
type ExpressionKey = (Operation, Vec<ValueId>);

/// The operation half of an expression key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Operation {
    ConstI32(i32),
    ConstI64(i64),
    ConstF32(u32),
    ConstF64(u64),
    RefNull,
    RefFunc(u32),
    MemorySize,
    /// A pure total unary operation, tagged by its own discriminant.
    Unary(u8),
    /// A pure total binary operation, tagged by its own discriminant.
    Binary(u8),
    /// A comparison, tagged by its own discriminant.
    Compare(u8),
    Select,
}

/// The key for an instruction, or `None` if it may not participate.
///
/// `None` covers every instruction that can trap or has an effect, plus the forms
/// that read mutable state: a `load` is a pure expression, but two loads of the
/// same address are only interchangeable if nothing wrote memory in between,
/// which is a memory-dependence question this pass does not answer. Leaving
/// loads out costs some optimization and removes an entire class of bug.
fn expression_key(instruction: &IrInstr) -> Option<ExpressionKey> {
    if may_trap(instruction) || has_side_effects(instruction) {
        return None;
    }
    let key = match instruction {
        IrInstr::ConstI32 { value, .. } => (Operation::ConstI32(*value), Vec::new()),
        IrInstr::ConstI64 { value, .. } => (Operation::ConstI64(*value), Vec::new()),
        IrInstr::ConstF32 { value, .. } => (Operation::ConstF32(*value), Vec::new()),
        IrInstr::ConstF64 { value, .. } => (Operation::ConstF64(*value), Vec::new()),
        IrInstr::RefNull { .. } => (Operation::RefNull, Vec::new()),
        IrInstr::RefFunc { function, .. } => (Operation::RefFunc(*function), Vec::new()),
        IrInstr::MemorySize { .. } => (Operation::MemorySize, Vec::new()),
        IrInstr::Select {
            condition,
            left,
            right,
            ..
        } => (Operation::Select, vec![*condition, *left, *right]),
        // A `local.get` is already a copy; eliminating it would be a no-op at
        // best and a miscompile at worst, so it does not participate.
        IrInstr::LocalGet { .. } => return None,
        IrInstr::I32Eqz { value, .. } | IrInstr::I64Eqz { value, .. } => {
            (Operation::Unary(0), vec![*value])
        }
        IrInstr::I32Unary {
            value, operation, ..
        } => (Operation::Unary(1 + *operation as u8), vec![*value]),
        IrInstr::I64Unary {
            value, operation, ..
        } => (Operation::Unary(4 + *operation as u8), vec![*value]),
        IrInstr::F32Unary {
            value, operation, ..
        } => (Operation::Unary(7 + *operation as u8), vec![*value]),
        IrInstr::F64Unary {
            value, operation, ..
        } => (Operation::Unary(14 + *operation as u8), vec![*value]),
        IrInstr::IntConvert {
            value, operation, ..
        } => (Operation::Unary(21 + *operation as u8), vec![*value]),
        IrInstr::Reinterpret {
            value, operation, ..
        } => (Operation::Unary(24 + *operation as u8), vec![*value]),
        IrInstr::SignExtend {
            value, operation, ..
        } => (Operation::Unary(28 + *operation as u8), vec![*value]),
        IrInstr::FloatConvert {
            value, operation, ..
        } => (Operation::Unary(33 + *operation as u8), vec![*value]),
        IrInstr::RefIsNull { value, .. } => (Operation::Unary(42), vec![*value]),
        IrInstr::I32Compare {
            left,
            right,
            comparison,
            ..
        } => (Operation::Compare(*comparison as u8), vec![*left, *right]),
        IrInstr::I64Compare {
            left,
            right,
            comparison,
            ..
        } => (
            Operation::Compare(10 + *comparison as u8),
            vec![*left, *right],
        ),
        IrInstr::F32Compare {
            left,
            right,
            comparison,
            ..
        } => (
            Operation::Compare(20 + *comparison as u8),
            vec![*left, *right],
        ),
        IrInstr::F64Compare {
            left,
            right,
            comparison,
            ..
        } => (
            Operation::Compare(26 + *comparison as u8),
            vec![*left, *right],
        ),
        other => {
            // The remaining pure total forms are all binary. Their
            // discriminants are spaced so no two operations can share a tag.
            let tag = binary_tag(other)?;
            (Operation::Binary(tag), Vec::new())
        }
    };
    let (operation, mut operands) = key;
    if operands.is_empty() {
        operands = analysis::used_values(instruction);
        if commutative(instruction) && operands.len() == 2 {
            // `a + b` and `b + a` are the same value, so they must share a key.
            // `a - b` and `b - a` are not, which is why only the commutative
            // operations get here.
            operands.sort_by_key(|value| value.0);
        }
    }
    Some((operation, operands))
}

/// A unique tag per binary operation, or `None` for a non-binary form.
fn binary_tag(instruction: &IrInstr) -> Option<u8> {
    use IrInstr::*;
    Some(match instruction {
        I32Add { .. } => 0,
        I32Sub { .. } => 1,
        I32Mul { .. } => 2,
        I32DivS { .. } => 3,
        I32DivU { .. } => 4,
        I32RemS { .. } => 5,
        I32RemU { .. } => 6,
        I32And { .. } => 7,
        I32Or { .. } => 8,
        I32Xor { .. } => 9,
        I32Shl { .. } => 10,
        I32ShrS { .. } => 11,
        I32ShrU { .. } => 12,
        I32Rotl { .. } => 13,
        I32Rotr { .. } => 14,
        I64Add { .. } => 15,
        I64Sub { .. } => 16,
        I64Mul { .. } => 17,
        I64DivS { .. } => 18,
        I64DivU { .. } => 19,
        I64RemS { .. } => 20,
        I64RemU { .. } => 21,
        I64And { .. } => 22,
        I64Or { .. } => 23,
        I64Xor { .. } => 24,
        I64Shl { .. } => 25,
        I64ShrS { .. } => 26,
        I64ShrU { .. } => 27,
        I64Rotl { .. } => 28,
        I64Rotr { .. } => 29,
        F32Add { .. } => 30,
        F32Sub { .. } => 31,
        F32Mul { .. } => 32,
        F32Div { .. } => 33,
        F32Min { .. } => 34,
        F32Max { .. } => 35,
        F32Copysign { .. } => 36,
        F64Add { .. } => 37,
        F64Sub { .. } => 38,
        F64Mul { .. } => 39,
        F64Div { .. } => 40,
        F64Min { .. } => 41,
        F64Max { .. } => 42,
        F64Copysign { .. } => 43,
        _ => return None,
    })
}

/// Is this operation commutative in Wasm?
fn commutative(instruction: &IrInstr) -> bool {
    use IrInstr::*;
    matches!(
        instruction,
        I32Add { .. }
            | I32Mul { .. }
            | I32And { .. }
            | I32Or { .. }
            | I32Xor { .. }
            | I64Add { .. }
            | I64Mul { .. }
            | I64And { .. }
            | I64Or { .. }
            | I64Xor { .. }
            | F32Add { .. }
            | F32Mul { .. }
            | F64Add { .. }
            | F64Mul { .. }
    )
}

/// LVN: eliminate redundancy within each block.
///
/// Uses of a replaced value can live in a *later* block -- the IR is not
/// restricted to single-use values -- so the replacements are collected into one
/// map for the whole function and applied at the end. Applying them per block
/// would miss a use past a branch, and that miss would surface as an undefined
/// value in the verifier rather than as a wrong answer, which is the better of
/// the two failures but still a failure.
fn eliminate_in_blocks(function: &mut IrFunction) -> usize {
    let cfg = Cfg::new(function);
    let mut replacements: HashMap<ValueId, ValueId> = HashMap::new();
    let mut removed = 0;

    for block_index in 0..function.blocks.len() {
        if !cfg.is_reachable(block_index) {
            continue;
        }
        let mut available: HashMap<ExpressionKey, ValueId> = HashMap::new();
        let block = &mut function.blocks[block_index];
        block.instrs.retain_mut(|instruction| {
            let key = expression_key(instruction);
            if let Some(key) = &key {
                if let Some(&original) = available.get(key) {
                    let results = defined_values(instruction);
                    if results.len() == 1 && results[0] != original {
                        let duplicate = results[0];
                        let lookup = |value: ValueId| {
                            if value == duplicate {
                                original
                            } else {
                                value
                            }
                        };
                        map_operands(instruction, lookup);
                        replacements.insert(duplicate, original);
                        removed += 1;
                        return false;
                    }
                }
            }
            if let (Some(key), Some(result)) = (key, analysis::defines(instruction)) {
                available.entry(key).or_insert(result);
            }
            true
        });
    }

    if !replacements.is_empty() {
        apply_replacements(function, &replacements);
    }
    removed
}

/// CSE: eliminate redundancy along the dominator tree.
///
/// The same shape as LVN, with the available-expression table carried from a
/// block's *dominators* rather than from the top of the block. Only a dominating
/// block's expressions are in scope, which is what makes a reuse
/// dominance-safe: a value computed in a sibling block is simply not in the
/// table, so it is never reused.
fn eliminate_across_blocks(function: &mut IrFunction) -> usize {
    let cfg = Cfg::new(function);
    let dominators = Dominators::new(&cfg);
    let mut replacements: HashMap<ValueId, ValueId> = HashMap::new();
    let mut removed = 0;
    // `carried[block]` is the table available at that block's entry.
    let mut carried: Vec<HashMap<ExpressionKey, ValueId>> =
        vec![HashMap::new(); function.blocks.len()];

    for &block_index in &cfg.order {
        // Inherited from every *strict* dominator. The immediate dominator is
        // enough -- everything it inherited is already in its own table -- and
        // walking the whole dominator set would be quadratic for the same result.
        // The immediate dominator's table is the whole inherited set: everything
        // it inherited is already in it, so the chain needs no separate walk.
        let inherited = dominators
            .immediate(block_index)
            .map(|parent| carried[parent].clone())
            .unwrap_or_default();
        let mut available: HashMap<ExpressionKey, ValueId> = inherited;

        let block = &mut function.blocks[block_index];
        block.instrs.retain_mut(|instruction| {
            let key = expression_key(instruction);
            if let Some(key) = &key {
                if let Some(&original) = available.get(key) {
                    let results = defined_values(instruction);
                    if results.len() == 1 && results[0] != original {
                        let duplicate = results[0];
                        let lookup = |value: ValueId| {
                            if value == duplicate {
                                original
                            } else {
                                value
                            }
                        };
                        map_operands(instruction, lookup);
                        replacements.insert(duplicate, original);
                        removed += 1;
                        return false;
                    }
                }
            }
            if let (Some(key), Some(result)) = (key, analysis::defines(instruction)) {
                available.entry(key).or_insert(result);
            }
            true
        });
        // Published *after* the block is processed, so a successor sees this
        // block's own expressions. Publishing before -- as an earlier version did,
        // to reuse the inherited table for the successor's lookup -- records a
        // table that does not yet contain this block's results, and a successor
        // then fails to reuse an expression that is available. That is a missed
        // optimization rather than a wrong answer, but it also meant the recorded
        // table and the one actually used disagreed, which is the harder kind of
        // bug to find later.
        carried[block_index] = available;
    }

    if !replacements.is_empty() {
        apply_replacements(function, &replacements);
    }
    removed
}

/// Apply a value-replacement map across a whole function.
///
/// Every operand and every block parameter is rewritten, and the instructions
/// that *defined* a replaced value have already been removed by the caller. The
/// map is applied transitively: a replacement chain `a -> b -> c` is resolved to
/// `c`, because a pass that ran earlier may itself have replaced `b`, and
/// rewriting `a` to `b` would leave a use of a value that no longer exists.
fn apply_replacements(function: &mut IrFunction, replacements: &HashMap<ValueId, ValueId>) {
    let resolve = |value: ValueId| {
        let mut current = value;
        // Bounded by the map size so a cycle introduced by a bug cannot spin.
        for _ in 0..=replacements.len() {
            match replacements.get(&current) {
                Some(&next) if next != current => current = next,
                _ => return current,
            }
        }
        current
    };
    for block in &mut function.blocks {
        for param in block.params.iter_mut() {
            *param = resolve(*param);
        }
        for instruction in &mut block.instrs {
            map_operands(instruction, &resolve);
        }
        analysis::map_terminator_operands(&mut block.terminator, &resolve);
    }
}
