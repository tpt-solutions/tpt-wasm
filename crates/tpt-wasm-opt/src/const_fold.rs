// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Constant folding and constant propagation over the TPT IR.
//!
//! ## Precondition
//!
//! The function is verified IR: single-assignment for instruction results,
//! every value dominated by its uses, and every block reachable from the entry
//! carrying exactly the values its incoming edges supply.
//!
//! ## Transformation
//!
//! Within one basic block, a value's constant is known if the block defines it as
//! a constant and nothing since has redefined it. An instruction whose operands
//! are all known is evaluated by [`crate::numeric::evaluate`] and, when the
//! result is a value, replaced by a constant definition of its own result.
//!
//! ## Semantic invariant
//!
//! **The trap is part of the value.** An instruction that provably traps is not
//! deleted and not replaced by a value; it is rewritten into the `Trap`
//! terminator it always was, and the rest of the block is dropped. That is only
//! sound when nothing before it in the block can trap or have an effect, because
//! otherwise the *first* observable event would change. A block that has already
//! stored to memory keeps its store, and the trap simply happens at the same
//! place it always did.
//!
//! The alternative -- leaving a provably trapping operation alone -- would also
//! be correct, just less optimal. Rewriting it is the point: it is what lets dead
//! code elimination afterwards remove the dead tail and shrink the block.

use std::collections::HashMap;

use tpt_wasm_ir::{BasicBlock, IrFunction, IrInstr, IrModule, Terminator, ValueId};

use crate::analysis::{may_trap, used_values};
use crate::numeric::{evaluate, Const, Folded};
use crate::PassOutcome;

/// Fold constant expressions in every function.
pub fn run(module: &mut IrModule) -> PassOutcome {
    let mut folded = 0;
    for function in &mut module.functions {
        folded += fold_function(function);
    }
    PassOutcome {
        rewrites: folded,
        ..PassOutcome::default()
    }
}

/// Fold one function, returning how many instructions it rewrote.
pub fn fold_function(function: &mut IrFunction) -> usize {
    let mut folded = 0;
    // Blocks are folded in place. A block's instruction list is the only thing
    // that changes; block identity, parameters, and terminators are left alone
    // except where a proven trap replaces a terminator, which is counted
    // separately because it removes instructions rather than rewriting one.
    for index in 0..function.blocks.len() {
        folded += fold_block(&mut function.blocks[index]);
    }
    folded
}

/// The constants known for a block's values, as it is walked.
///
/// Only *this block's* definitions are consulted. A constant computed in a
/// dominating block is still a valid constant, but deciding that needs the
/// dominator tree and is constant propagation's job; keeping the folder to one
/// block means it can never reason about a value that is not in scope, which is
/// the mistake that would produce a use of an undefined value.
#[derive(Default)]
struct Known {
    values: HashMap<ValueId, Const>,
    /// True once an instruction in this block has been seen that can trap or
    /// have an effect, after which a proven trap may not rewrite the block.
    barrier_reached: bool,
}

impl Known {
    fn get(&self, value: ValueId) -> Option<Const> {
        self.values.get(&value).copied()
    }

    fn set(&mut self, value: ValueId, constant: Const) {
        self.values.insert(value, constant);
    }

    /// Record whatever an instruction does to the constant environment.
    fn observe(&mut self, instruction: &IrInstr) {
        if may_trap(instruction) || crate::analysis::has_side_effects(instruction) {
            self.barrier_reached = true;
        }
        // A `local.set` / `local.tee` re-assigns the slot, so any constant known
        // for that local is void from here on. Instruction results are
        // single-assignment, so only locals need invalidating -- and a local is
        // the one value whose identity does not imply its content.
        match instruction {
            IrInstr::LocalSet { local, .. } | IrInstr::LocalTee { local, .. } => {
                self.values.remove(local);
            }
            _ => {}
        }
    }
}

/// Fold one block. Returns the number of instructions rewritten or removed.
fn fold_block(block: &mut BasicBlock) -> usize {
    let mut known = Known::default();
    // Block parameters are incoming values, not constants, so nothing is seeded
    // for them. They are still *read*, and a read of an unknown value simply
    // stops the fold at that instruction.
    let mut changes = 0;
    let mut kept: Vec<IrInstr> = Vec::with_capacity(block.instrs.len());
    for instruction in std::mem::take(&mut block.instrs) {
        // Record the environment *before* the instruction, so an instruction's own
        // result is not visible to itself.
        let operands: Vec<Option<Const>> = {
            let mut table: Vec<Option<Const>> = vec![None; 64];
            for value in used_values(&instruction) {
                let slot = value.0 as usize;
                if slot >= table.len() {
                    table.resize(slot + 1, None);
                }
                table[slot] = known.get(value);
            }
            table
        };
        let folded = evaluate(&instruction, &operands);

        match folded {
            Folded::Value(constant) => {
                // A constant definition for the instruction's own result. Any
                // results beyond the first (a call, which never folds) are not
                // reachable here because `evaluate` only folds single-result
                // forms.
                if let Some(result) = crate::analysis::defines(&instruction) {
                    let replacement = constant_instruction(result, constant);
                    if let Some(replacement) = replacement {
                        known.set(result, constant);
                        kept.push(replacement);
                        changes += 1;
                        continue;
                    }
                }
                // Not a single-result form after all: keep the original and let
                // the environment absorb whatever it defines.
                record_definitions(&mut known, &instruction);
                kept.push(instruction);
            }
            Folded::Traps(trap) => {
                // The instruction is a proven trap. It may be rewritten into the
                // block's terminator only if nothing before it could have been
                // observed first, because the trap's *position* in the observable
                // order is part of the program's meaning.
                if !known.barrier_reached {
                    // The instructions already kept stay: a store among them is
                    // observable and happened before the trap. Only the
                    // instructions *after* the trap are dropped, because the
                    // trap means they can never execute.
                    block.instrs = kept;
                    block.terminator = Terminator::Trap(trap);
                    return changes + 1;
                }
                // Otherwise leave the instruction exactly where it was: the trap
                // still happens at the same point, which is the only thing that
                // matters.
                record_definitions(&mut known, &instruction);
                kept.push(instruction);
            }
            Folded::Unknown => {
                record_definitions(&mut known, &instruction);
                kept.push(instruction);
            }
        }
    }
    block.instrs = kept;
    changes
}

/// Record the constants a non-folded instruction establishes.
///
/// A constant definition makes its own result known. Nothing else here does,
/// because a value is only a constant if it was computed from constants, and
/// that is what `evaluate` already decided it was not.
fn record_definitions(known: &mut Known, instruction: &IrInstr) {
    known.observe(instruction);
    if let IrInstr::ConstI32 { result, value } = instruction {
        known.set(*result, Const::I32(*value));
    } else if let IrInstr::ConstI64 { result, value } = instruction {
        known.set(*result, Const::I64(*value));
    } else if let IrInstr::ConstF32 { result, value } = instruction {
        known.set(*result, Const::F32(*value));
    } else if let IrInstr::ConstF64 { result, value } = instruction {
        known.set(*result, Const::F64(*value));
    }
}

/// The constant definition replacing a folded instruction, if there is one.
///
/// A folded value is always one of the four Wasm scalar types, so a constant
/// instruction always exists. The `Option` is kept so a type added to the IR
/// later degrades into "not folded" rather than into a panic mid-compilation.
fn constant_instruction(result: ValueId, constant: Const) -> Option<IrInstr> {
    Some(match constant {
        Const::I32(value) => IrInstr::ConstI32 { result, value },
        Const::I64(value) => IrInstr::ConstI64 { result, value },
        Const::F32(bits) => IrInstr::ConstF32 {
            result,
            value: bits,
        },
        Const::F64(bits) => IrInstr::ConstF64 {
            result,
            value: bits,
        },
    })
}
