// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Constant propagation across basic blocks.
//!
//! ## Precondition
//!
//! Verified IR.
//!
//! ## Transformation
//!
//! A forward dataflow over the block graph gives every value a lattice element:
//! `Unknown`, or a specific constant. A block's entry state is the *meet* of its
//! predecessors' exit states, so two predecessors that disagree yield `Unknown`.
//! Any instruction that may trap, has a side effect, or writes a local
//! invalidates the locals, because anything after it may see different values.
//!
//! The rewrite is deliberately narrow: a `local.get` whose slot is known to hold
//! a constant becomes a constant definition *of its own existing result*. No new
//! value is introduced, so nothing about the function's value numbering changes
//! and there is no opportunity to leave a value declared but undefined.
//!
//! ## Semantic invariant
//!
//! Values are unchanged. A use is replaced only when every path reaching it
//! agrees on the constant, and a local's fact is dropped at every assignment, so
//! a propagation can never read a slot as holding a value that was overwritten.
//!
//! Locals are the whole difficulty. Unlike an instruction result, a local's
//! identity does not determine its content: `local.set` re-assigns it, a branch
//! may or may not run, and a call may write it. The lattice invalidates on
//! assignment, but that is *incomplete across blocks* -- a call in another block
//! can write this function's locals -- so every local is treated as `Unknown`
//! after any block containing a call, a trap, or a side effect. Being
//! conservative there costs optimizations and buys correctness; assuming a call
//! is local-pure is exactly the bug that makes an optimizer lie about behavior.
//!
//! A local therefore only carries a constant *up to the first barrier*, which is
//! still the common case: a straight-line numeric function has no barriers at
//! all, and that is where propagation pays.

use std::collections::HashMap;

use tpt_wasm_ir::{IrFunction, IrInstr, IrModule, ValueId};

use crate::analysis::{has_side_effects, may_trap, Cfg};
use crate::numeric::Const;
use crate::PassOutcome;

/// What is known about a value at a program point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fact {
    /// Not known, or known to differ between paths.
    Unknown,
    /// Known to hold this constant on every path to the point.
    Constant(Const),
}

impl Fact {
    fn constant(self) -> Option<Const> {
        match self {
            Self::Constant(value) => Some(value),
            Self::Unknown => None,
        }
    }

    /// The meet of two facts. Agreement keeps the constant; disagreement and
    /// `Unknown` both give `Unknown`.
    fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            (Self::Constant(a), Self::Constant(b)) if a == b => Self::Constant(a),
            _ => Self::Unknown,
        }
    }
}

type State = Vec<Fact>;

/// Propagate constants in every function.
pub fn run(module: &mut IrModule) -> PassOutcome {
    let mut rewrites = 0;
    for function in &mut module.functions {
        rewrites += propagate(function);
    }
    PassOutcome {
        rewrites,
        ..PassOutcome::default()
    }
}

/// Propagate constants in one function, returning how many reads it rewrote.
pub fn propagate(function: &mut IrFunction) -> usize {
    let cfg = Cfg::new(function);
    let width = state_len(function);

    // `exit[block]` is the state after running that block. `entry[block]` is the
    // meet over predecessors, recomputed each round from the exits.
    let mut exit: Vec<State> = vec![vec![Fact::Unknown; width]; function.blocks.len()];
    let mut entry: Vec<State> = vec![vec![Fact::Unknown; width]; function.blocks.len()];

    // Reverse postorder visits a block after the predecessors that can reach it,
    // which is what makes one sweep usually enough. The fixpoint loop covers the
    // loops: on a back edge the header's entry state improves and the loop body
    // is re-run. The bound is a backstop, not the expected iteration count.
    for _ in 0..=function.blocks.len() * 2 + 2 {
        let mut changed = false;
        for &block_index in &cfg.order {
            let merged = merge_predecessors(&cfg, &exit, block_index, width);
            if merged != entry[block_index] {
                entry[block_index] = merged;
                changed = true;
            }
            let out = transfer(function, block_index, &entry[block_index]);
            if out != exit[block_index] {
                exit[block_index] = out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut rewrites = 0;
    for (block_index, state) in entry.iter().enumerate() {
        if cfg.is_reachable(block_index) {
            rewrites += rewrite_block(function, block_index, state);
        }
    }
    rewrites
}

fn state_len(function: &IrFunction) -> usize {
    function
        .values
        .iter()
        .map(|value| value.id.0 as usize + 1)
        .max()
        .unwrap_or(0)
}

fn get(state: &State, value: ValueId) -> Fact {
    state
        .get(value.0 as usize)
        .copied()
        .unwrap_or(Fact::Unknown)
}

fn set(state: &mut State, value: ValueId, fact: Fact) {
    let index = value.0 as usize;
    if index >= state.len() {
        state.resize(index + 1, Fact::Unknown);
    }
    state[index] = fact;
}

/// The meet of every reachable predecessor's exit state.
fn merge_predecessors(cfg: &Cfg, exit: &[State], block_index: usize, width: usize) -> State {
    if block_index == cfg.entry {
        return vec![Fact::Unknown; width];
    }
    let mut merged: Option<State> = None;
    for &predecessor in &cfg.predecessors[block_index] {
        if !cfg.is_reachable(predecessor) {
            continue;
        }
        let state = &exit[predecessor];
        merged = Some(match merged {
            None => state.clone(),
            Some(current) => (0..width.max(current.len()))
                .map(|index| {
                    current
                        .get(index)
                        .copied()
                        .unwrap_or(Fact::Unknown)
                        .meet(state.get(index).copied().unwrap_or(Fact::Unknown))
                })
                .collect(),
        });
    }
    merged.unwrap_or_else(|| vec![Fact::Unknown; width])
}

/// The state after running one block's instructions.
///
/// ## Why a local's fact does not survive this block
///
/// The `LocalSet` handler below records the local's new value, and that is
/// correct *for the rest of this block*. It is deliberately then dropped again
/// before the block's state is returned, because this function's result is
/// merged into every successor's entry state -- and a successor may have been
/// reached by a path that skipped the `local.set` entirely.
///
/// The concrete failure that motivated this is the shape a Wasm `br_if` lowers
/// to:
///
/// ```text
/// (block (local.set 0 (i32.const 5))
///        (br_if 0 (i32.const 0))
///        (local.set 0 (i32.const 7)))
/// (return (local.get 0))
/// ```
///
/// The `local.get` is reachable both with `5` and with `7`. Carrying `5` across
/// the block would propagate a constant that is wrong on one of the two paths,
/// and the symptom is a module that returns `5` where the program returns `7` --
/// a miscompile that no amount of downstream verification catches, because the
/// resulting IR is perfectly well-formed.
///
/// So the rule is: a local's constant is known only inside the straight-line
/// sequence that wrote it, and the exit state forgets every local. Constants
/// still propagate through instruction results, which are single-assignment and
/// cannot be conditionally written -- which is where nearly all the real wins are
/// anyway, since folding `2 + 3` does not go through a local.
fn transfer(function: &IrFunction, block_index: usize, input: &State) -> State {
    let mut state = input.clone();
    for instruction in &function.blocks[block_index].instrs {
        if let IrInstr::LocalSet { local, value } | IrInstr::LocalTee { local, value, .. } =
            instruction
        {
            let fact = get(&state, *value);
            set(&mut state, *local, fact);
        }
        if may_trap(instruction) || has_side_effects(instruction) {
            // A call can write any local of this function, and a trap ends the
            // block, so nothing survives past one.
            for local in &function.locals {
                set(&mut state, *local, Fact::Unknown);
            }
        }
        if let Some(result) = crate::analysis::defines(instruction) {
            set(&mut state, result, fact_of(instruction));
        }
    }
    // Locals do not survive the block: see the doc comment above.
    for local in &function.locals {
        set(&mut state, *local, Fact::Unknown);
    }
    state
}

/// The fact a single instruction establishes, if any.
fn fact_of(instruction: &IrInstr) -> Fact {
    match instruction {
        IrInstr::ConstI32 { value, .. } => Fact::Constant(Const::I32(*value)),
        IrInstr::ConstI64 { value, .. } => Fact::Constant(Const::I64(*value)),
        IrInstr::ConstF32 { value, .. } => Fact::Constant(Const::F32(*value)),
        IrInstr::ConstF64 { value, .. } => Fact::Constant(Const::F64(*value)),
        _ => Fact::Unknown,
    }
}

/// Replace reads of a known-constant local with a constant definition.
///
/// The replacement reuses the `local.get`'s own result, so the function's value
/// numbering is untouched. The result's declared type is checked against the
/// constant's before the swap: `local.get` produces the local's type, so a
/// mismatch would mean the local's declared type and its known value disagree,
/// and refusing is the only safe response.
fn rewrite_block(function: &mut IrFunction, block_index: usize, input: &State) -> usize {
    let types: HashMap<ValueId, tpt_wasm_types::ValueType> = function
        .values
        .iter()
        .map(|value| (value.id, value.value_type))
        .collect();

    let mut state = input.clone();
    let mut rewrites = 0;
    let block = &mut function.blocks[block_index];
    for instruction in &mut block.instrs {
        // The result and the fact are read out before the swap, so the pattern's
        // borrow does not overlap the assignment that invalidates it.
        let candidate = match instruction {
            IrInstr::LocalGet { result, local } => get(&state, *local)
                .constant()
                .filter(|constant| {
                    types
                        .get(result)
                        .map(|declared| *declared == constant.value_type())
                        .unwrap_or(false)
                })
                .map(|constant| (*result, constant)),
            _ => None,
        };
        if let Some((result, constant)) = candidate {
            *instruction = constant_instruction(result, constant);
            set(&mut state, result, Fact::Constant(constant));
            rewrites += 1;
            continue;
        }
        // Advance the state past this instruction, exactly as `transfer` does.
        if may_trap(instruction) || has_side_effects(instruction) {
            for local in &function.locals {
                set(&mut state, *local, Fact::Unknown);
            }
        }
        if let IrInstr::LocalSet { local, value } | IrInstr::LocalTee { local, value, .. } =
            instruction
        {
            let fact = get(&state, *value);
            set(&mut state, *local, fact);
        }
        if let Some(result) = crate::analysis::defines(instruction) {
            set(&mut state, result, fact_of(instruction));
        }
    }
    rewrites
}

fn constant_instruction(result: ValueId, constant: Const) -> IrInstr {
    match constant {
        Const::I32(value) => IrInstr::ConstI32 { result, value },
        Const::I64(value) => IrInstr::ConstI64 { result, value },
        Const::F32(value) => IrInstr::ConstF32 { result, value },
        Const::F64(value) => IrInstr::ConstF64 { result, value },
    }
}
