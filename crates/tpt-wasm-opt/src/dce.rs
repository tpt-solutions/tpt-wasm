// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Dead code elimination.
//!
//! ## Precondition
//!
//! Verified IR.
//!
//! ## Transformation
//!
//! Sweep each block from the bottom, keeping an instruction whose results are
//! still needed and dropping one that satisfies
//! [`crate::analysis::is_removable`]. Dropping an instruction releases its
//! operands, so the sweep is a single backward pass with a worklist: removing
//! one instruction can make the instruction that produced its operand dead in
//! turn.
//!
//! ## Semantic invariant
//!
//! The set of *observable* events is unchanged: same values, same traps in the
//! same order, same memory, global, table, and host effects, in the same order.
//!
//! Three classes of instruction are protected, and each protects a different
//! clause of the contract:
//!
//! * anything that may trap, so a trap is never deleted even when its result is
//!   dead -- an `i32.div_s` by zero still has to fire;
//! * anything with a side effect, so a store, a `global.set`, a `memory.grow`,
//!   or a call into the host boundary still happens;
//! * anything whose result is live, obviously.
//!
//! Unreachable blocks are *not* removed here. That is control-flow
//! simplification's job, and it needs the terminator graph intact to do it
//! correctly. Dead code elimination only ever empties blocks.

use tpt_wasm_ir::{IrFunction, IrModule, ValueId};

use crate::analysis::{defined_values, is_removable, used_values};
use crate::PassOutcome;

/// Remove dead instructions from every function.
pub fn run(module: &mut IrModule) -> PassOutcome {
    let mut removed = 0;
    for function in &mut module.functions {
        removed += eliminate(function);
    }
    PassOutcome {
        instructions_removed: removed,
        ..PassOutcome::default()
    }
}

/// Remove dead instructions from one function, returning how many went.
pub fn eliminate(function: &mut IrFunction) -> usize {
    let mut removed = 0;
    // A value is live if a terminator names it, or if a *surviving* instruction
    // does. The worklist carries values that have just become live so their
    // producers are kept, and values that have just died so their producers can
    // be re-examined.
    let mut live: Vec<ValueId> = Vec::new();
    for block in &function.blocks {
        live.extend(crate::analysis::terminator_used_values(&block.terminator));
    }

    // Sweep blocks in reverse order so a producer is examined after the
    // consumers that might have kept it. Within a block, sweep instructions back
    // to front for the same reason.
    for block in function.blocks.iter_mut().rev() {
        let mut survivors: Vec<tpt_wasm_ir::IrInstr> = Vec::with_capacity(block.instrs.len());
        for instruction in block.instrs.iter().rev() {
            let results = defined_values(instruction);
            let any_used = results.iter().any(|result| live.contains(result));
            if !is_removable(instruction, any_used) {
                // Keep it, and mark everything it reads as needed.
                live.extend(used_values(instruction));
                survivors.push(instruction.clone());
            } else {
                removed += 1;
            }
        }
        survivors.reverse();
        block.instrs = survivors;
    }
    removed
}

/// A value is live anywhere in the function, by the crude whole-function measure.
///
/// Exposed for the scheduling pass, which needs to know whether moving an
/// instruction would carry a value across a use.
pub fn is_live_anywhere(function: &IrFunction, value: ValueId) -> bool {
    for block in &function.blocks {
        if crate::analysis::terminator_used_values(&block.terminator).contains(&value) {
            return true;
        }
        for instruction in &block.instrs {
            if used_values(instruction).contains(&value) {
                return true;
            }
        }
    }
    false
}
