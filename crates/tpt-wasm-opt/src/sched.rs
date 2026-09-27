// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Instruction scheduling within a basic block.
//!
//! ## Precondition
//!
//! Verified IR.
//!
//! ## Transformation
//!
//! Each block is cut into *regions* at every instruction that may trap or has an
//! effect. Within a region, the pure total instructions are topologically sorted
//! by their data dependences, preferring the original order among instructions
//! that are ready at the same time. The barrier instructions keep their exact
//! positions, and a region never crosses one.
//!
//! ## Semantic invariant
//!
//! Instruction order is observable, and this pass preserves every observable
//! order exactly.
//!
//! That is the whole design. A scheduler that moved freely would change *when* a
//! trap fires, which `spec.txt` section 67 calls out explicitly: an optimization
//! that changes when a trap occurs is incorrect even if the eventual return value
//! matches. So the movable set is restricted twice over:
//!
//! * only pure, total instructions move -- an instruction that can trap or that
//!   writes memory, a global, or the host is a fixed barrier;
//! * a movable instruction never crosses a barrier, because the barrier's
//!   position relative to it is itself observable.
//!
//! Within those two restrictions, any topological order of the dependences is
//! sound: every operand is still computed before its use, and nothing else about
//! the block changed. The pass is therefore free to be aggressive inside a
//! region and completely immobile outside one.
//!
//! Ties are broken toward the original order so the schedule is deterministic:
//! two runs over the same input produce byte-identical output, which is what lets
//! the compilation cache treat a scheduled module as a cache key.

use std::collections::HashMap;

use tpt_wasm_ir::{IrFunction, IrInstr, IrModule, ValueId};

use crate::analysis::{defined_values, has_side_effects, may_trap, used_values};
use crate::PassOutcome;

/// Schedule every function.
pub fn run(module: &mut IrModule) -> PassOutcome {
    let mut rewrites = 0;
    for function in &mut module.functions {
        rewrites += schedule_function(function);
    }
    PassOutcome {
        rewrites,
        ..PassOutcome::default()
    }
}

/// Schedule one function, returning how many instructions moved.
pub fn schedule_function(function: &mut IrFunction) -> usize {
    let mut moved = 0;
    for index in 0..function.blocks.len() {
        moved += schedule_block(&mut function.blocks[index].instrs);
    }
    moved
}

/// Is this instruction fixed in place?
fn is_barrier(instruction: &IrInstr) -> bool {
    may_trap(instruction) || has_side_effects(instruction)
}

/// Reorder the movable instructions of one region.
///
/// The region is a slice of the block's instruction list containing no barriers.
/// `available` is the set of values defined before the region, plus everything
/// defined inside it so far.
fn schedule_region(region: &mut Vec<IrInstr>, incoming: &mut Vec<ValueId>) -> usize {
    if region.len() < 2 {
        return 0;
    }
    let count = region.len();
    // Dependence edges: `i` must follow `j` when `i` reads something `j` defines.
    // Pure total instructions define and use disjointly, so this is a real DAG
    // rather than a cycle -- an instruction cannot both define and read the same
    // value, because single-assignment forbids that and the verifier has said so.
    let mut before: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut position: HashMap<ValueId, usize> = HashMap::new();
    for (index, instruction) in region.iter().enumerate() {
        for result in defined_values(instruction) {
            position.entry(result).or_insert(index);
        }
    }
    for (index, instruction) in region.iter().enumerate() {
        for used in used_values(instruction) {
            if let Some(&producer) = position.get(&used) {
                if producer != index {
                    before[index].push(producer);
                }
            }
        }
    }

    // Kahn's algorithm, always taking the *earliest* ready instruction so the
    // result stays as close to the original order as the dependences allow.
    let mut remaining: Vec<usize> = (0..count).collect();
    let mut order: Vec<usize> = Vec::with_capacity(count);
    let mut emitted: Vec<bool> = vec![false; count];
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .copied()
            .find(|&index| before[index].iter().all(|&producer| emitted[producer]))
            .expect("a region of pure instructions always has a ready head");
        order.push(ready);
        emitted[ready] = true;
        remaining.retain(|&index| index != ready);
    }

    // Anything defined here is available to the next region.
    for instruction in &*region {
        incoming.extend(defined_values(instruction));
    }

    if order.iter().copied().eq(0..count) {
        return 0;
    }
    let original = std::mem::take(region);
    let mut rebuilt = Vec::with_capacity(count);
    for &index in &order {
        rebuilt.push(original[index].clone());
    }
    *region = rebuilt;
    count
}

/// Schedule one block, cutting it at every barrier.
fn schedule_block(instrs: &mut Vec<IrInstr>) -> usize {
    let original = std::mem::take(instrs);
    let mut rebuilt: Vec<IrInstr> = Vec::with_capacity(original.len());
    let mut region: Vec<IrInstr> = Vec::new();
    let mut moved = 0;
    let mut incoming: Vec<ValueId> = Vec::new();

    for instruction in original {
        if is_barrier(&instruction) {
            // The region ends here. Everything it defined is available to the
            // instructions after the barrier, which is recorded rather than
            // assumed, so a value crossing a barrier is handled by the same
            // map as one that does not.
            moved += schedule_region(&mut region, &mut incoming);
            rebuilt.append(&mut region);
            incoming.clear();
            incoming.extend(defined_values(&instruction));
            rebuilt.push(instruction);
        } else {
            region.push(instruction);
        }
    }
    moved += schedule_region(&mut region, &mut incoming);
    rebuilt.append(&mut region);
    *instrs = rebuilt;
    moved
}
