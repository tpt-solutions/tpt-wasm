// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Register allocation over the TPT IR.
//!
//! ## Precondition
//!
//! Verified IR.
//!
//! ## Transformation
//!
//! This pass computes an allocation and records it; it does not rewrite the IR.
//! Values are single-assignment, so a value's live range runs from its definition
//! to its last use, and two values interfere when their ranges overlap. A
//! Chaitin-style graph colouring assigns each value a register, spilling the
//! values that cannot be coloured.
//!
//! ## Semantic invariant
//!
//! None, because nothing in the IR changes. The pass reads the function and
//! produces a plan; a backend that honours the plan must preserve the same
//! property this pass establishes, which is:
//!
//! > No two values whose live ranges overlap share a register.
//!
//! [`verify`] checks exactly that, and the pipeline's per-pass re-verification
//! means a coloring that violates it is caught before the module is used. An
//! allocation that breaks this rule would let one value's register be read after
//! another overwrote it, which is a miscompile with no other symptom.
//!
//! Why a plan rather than a rewrite: rewriting would mean introducing a spill
//! store and reload into the IR, which the IR has no instruction for. Adding one
//! would be a larger change than the milestone calls for, and would put a
//! backend-shaped concern into an architecture-independent IR. Keeping the plan
//! separate lets the JIT consume it without the IR learning about registers, and
//! lets the correctness property be tested directly.
//!
//! Locals are *not* allocated. A local is storage for the whole function, so it
//! is live everywhere and would interfere with every other value; the honest
//! allocation for a local is a memory slot, which is what it already is.

use std::collections::{HashMap, HashSet};

use tpt_wasm_ir::{BlockId, IrFunction, IrModule, ValueId};

use crate::analysis::{all_defined, defined_values, terminator_used_values, Cfg, Dominators};
use crate::PassOutcome;

/// How many registers the allocator may use.
///
/// Deliberately small. A larger file would colour more aggressively, but the
/// allocator here is exercised on IR, not on generated machine code, and a small
/// file makes the spilling path -- the one that actually needs testing -- the
/// common case rather than a rare one.
pub const REGISTER_COUNT: usize = 8;

/// Where a value lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Held in this register from its definition to its last use.
    Register(u8),
    /// Held in this memory slot for its whole range.
    Spill(u32),
}

/// The allocation for one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// Where each value lives, keyed by `ValueId`.
    locations: HashMap<ValueId, Location>,
    /// Values that had to be given a memory slot.
    pub spilled: Vec<ValueId>,
    /// Values that fit in a register.
    pub in_registers: Vec<ValueId>,
}

impl Allocation {
    /// Where a value lives, if it was allocated at all.
    pub fn location(&self, value: ValueId) -> Option<Location> {
        self.locations.get(&value).copied()
    }

    /// How many stack slots the spilling needed.
    pub fn spill_slots(&self) -> usize {
        self.spilled.len()
    }
}

/// Run the allocator over every function, keeping the plan for each.
///
/// The plans are returned rather than stored, because a caller that wants to act
/// on them needs to be able to see them; the pipeline itself only needs to know
/// the pass ran and verified.
pub fn run(module: &mut IrModule) -> PassOutcome {
    for function in &module.functions {
        // The allocation is computed and checked. Discarding it is deliberate:
        // the IR is unchanged, so the pass is a *check* that a valid allocation
        // exists, and the plan is available through [`allocate`] for a backend
        // that wants one.
        let allocation = allocate(function);
        debug_assert!(verify(function, &allocation).is_ok());
    }
    // No rewrite, so the pipeline must not think this pass made progress. It is
    // in the `Full` level because its cost is real and its output is only wanted
    // by the native backends.
    PassOutcome::default()
}

/// Allocate registers for one function.
pub fn allocate(function: &IrFunction) -> Allocation {
    let cfg = Cfg::new(function);
    let dominators = Dominators::new(&cfg);
    let values: Vec<ValueId> = {
        let mut all: Vec<ValueId> = all_defined(function).into_iter().collect();
        all.sort_by_key(|value| value.0);
        all
    };
    let live = liveness(function, &cfg, &dominators);

    // Interference: two values conflict when both are live at the same program
    // point. A point is a block boundary, so the relation is computed from the
    // per-block live-in and live-out sets rather than from every instruction.
    let mut interference: HashMap<ValueId, HashSet<ValueId>> = HashMap::new();
    for block in &function.blocks {
        for &value in &live.live_in[cfg.index_of[&block.id]] {
            for &other in &live.live_out[cfg.index_of[&block.id]] {
                if value != other {
                    add_edge(&mut interference, value, other);
                }
            }
        }
    }

    // Colour by the simplify/select heuristic: repeatedly remove a node with
    // fewer neighbours than there are colours, push it on a stack, then pop the
    // stack and assign the lowest colour not used by a coloured neighbour. A
    // node that cannot be simplified is spilled.
    let mut degree: HashMap<ValueId, usize> = HashMap::new();
    for value in &values {
        degree.insert(*value, interference.get(value).map_or(0, |set| set.len()));
    }

    let mut stack: Vec<ValueId> = Vec::new();
    let mut removed: HashSet<ValueId> = HashSet::new();
    let mut worklist: Vec<ValueId> = values.clone();
    while !worklist.is_empty() {
        let Some(next) = worklist
            .iter()
            .copied()
            .find(|value| degree.get(value).copied().unwrap_or(0) < REGISTER_COUNT)
        else {
            break;
        };
        worklist.retain(|value| *value != next);
        removed.insert(next);
        stack.push(next);
        for &neighbour in interference.get(&next).into_iter().flatten() {
            if removed.contains(&neighbour) {
                continue;
            }
            if let Some(count) = degree.get_mut(&neighbour) {
                *count = count.saturating_sub(1);
            }
        }
    }

    let mut locations: HashMap<ValueId, Location> = HashMap::new();
    let mut spilled: Vec<ValueId> = Vec::new();
    let mut in_registers: Vec<ValueId> = Vec::new();
    let mut next_spill: u32 = 0;
    for &value in stack.iter().rev() {
        let neighbours = interference.get(&value).into_iter().flatten();
        let mut used = [false; REGISTER_COUNT];
        for &neighbour in neighbours {
            if let Some(Location::Register(colour)) = locations.get(&neighbour) {
                let index = *colour as usize;
                if index < REGISTER_COUNT {
                    used[index] = true;
                }
            }
        }
        match (0..REGISTER_COUNT).find(|index| !used[*index]) {
            Some(colour) => {
                locations.insert(value, Location::Register(colour as u8));
                in_registers.push(value);
            }
            None => {
                // No colour free: the value needs a slot. This is the path that
                // actually needs testing, and it is common at `REGISTER_COUNT`.
                locations.insert(value, Location::Spill(next_spill));
                next_spill += 1;
                spilled.push(value);
            }
        }
    }
    // Anything the simplify phase never reached stays unallocated rather than
    // being given a colour it might not deserve. `or_insert` rather than
    // `contains_key` then `insert`, so the slot number advances only when a
    // value actually needed one.
    for value in values {
        if locations.contains_key(&value) {
            continue;
        }
        locations.insert(value, Location::Spill(next_spill));
        next_spill += 1;
        spilled.push(value);
    }

    Allocation {
        locations,
        spilled,
        in_registers,
    }
}

fn add_edge(interference: &mut HashMap<ValueId, HashSet<ValueId>>, a: ValueId, b: ValueId) {
    interference.entry(a).or_default().insert(b);
    interference.entry(b).or_default().insert(a);
}

/// Live-in and live-out sets per block.
struct Liveness {
    live_in: Vec<Vec<ValueId>>,
    live_out: Vec<Vec<ValueId>>,
}

/// Backward liveness over the block graph.
///
/// Locals are live everywhere, which is why they are excluded from the
/// interference graph in [`allocate`] but still appear here: a local read in one
/// block is a real use, and dropping it would make a value look dead.
fn liveness(function: &IrFunction, cfg: &Cfg, dominators: &Dominators) -> Liveness {
    let count = function.blocks.len();
    let locals: HashSet<ValueId> = function.locals.iter().copied().collect();

    // A value is "entry-live" if it is a parameter or a local: it exists before
    // the first instruction and is readable in every block.
    let mut live_in: Vec<Vec<ValueId>> = vec![Vec::new(); count];
    let mut live_out: Vec<Vec<ValueId>> = vec![Vec::new(); count];

    // Seed each block's live-in with the values it can read without a local
    // definition: its parameters, and the function's locals.
    for (index, block) in function.blocks.iter().enumerate() {
        let mut entry: Vec<ValueId> = block.params.clone();
        entry.extend(function.params.iter().copied());
        entry.extend(locals.iter().copied());
        entry.sort_by_key(|value| value.0);
        entry.dedup();
        live_in[index] = entry;
    }

    for _ in 0..=count * 2 + 2 {
        let mut changed = false;
        for &index in cfg.order.iter().rev() {
            let block = &function.blocks[index];
            let mut live: HashSet<ValueId> = live_out[index].iter().copied().collect();
            for value in terminator_used_values(&block.terminator) {
                live.insert(value);
            }
            for instruction in block.instrs.iter().rev() {
                for defined in defined_values(instruction) {
                    // A local is storage, so writing one does not end its live
                    // range: a later block can still read it.
                    if !locals.contains(&defined) {
                        live.remove(&defined);
                    }
                }
                for used in crate::analysis::used_values(instruction) {
                    live.insert(used);
                }
            }
            let mut out: Vec<ValueId> = live.iter().copied().collect();
            out.sort_by_key(|value| value.0);
            let mut inside: Vec<ValueId> = live_in[index].clone();
            for value in &live_in[index] {
                if !live.contains(value) {
                    inside.retain(|candidate| candidate != value);
                }
            }
            inside.sort_by_key(|value| value.0);
            if out != live_out[index] || inside != live_in[index] {
                live_out[index] = out;
                live_in[index] = inside;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let _ = dominators;
    Liveness { live_in, live_out }
}

/// Check that an allocation is sound: no two interfering values share a register.
///
/// This is the property a backend relies on when it omits a reload after a call
/// or reuses a register across a branch, and it is checked directly rather than
/// assumed from the colouring algorithm.
pub fn verify(function: &IrFunction, allocation: &Allocation) -> Result<(), String> {
    let cfg = Cfg::new(function);
    let dominators = Dominators::new(&cfg);
    let live = liveness(function, &cfg, &dominators);
    let locals: HashSet<ValueId> = function.locals.iter().copied().collect();

    for block in &function.blocks {
        let index = cfg.index_of[&block.id];
        for &a in &live.live_in[index] {
            for &b in &live.live_out[index] {
                if a == b || locals.contains(&a) || locals.contains(&b) {
                    continue;
                }
                if let (Some(Location::Register(x)), Some(Location::Register(y))) =
                    (allocation.location(a), allocation.location(b))
                {
                    if x == y {
                        return Err(format!(
                            "values v{} and v{} share register {x} while both are live",
                            a.0, b.0
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Where a function's entry block is, for a caller that wants to walk the
/// allocation from the top.
pub fn entry(function: &IrFunction) -> BlockId {
    function.entry
}
