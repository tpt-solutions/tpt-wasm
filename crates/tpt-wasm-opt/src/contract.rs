// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The compiler optimization contract.
//!
//! `spec.txt` section 67 names what every optimization must preserve:
//!
//! ```text
//! values, control flow, traps, memory effects, global effects,
//! table effects, host effects, observable ordering
//! ```
//!
//! and says, of the last clause, that not merely return values are preserved --
//! "an optimization that changes *when* a trap occurs may be incorrect even if the
//! eventual return value looks identical".
//!
//! ## What is checked here, and what is not
//!
//! This module checks the properties that can be decided by *comparing two IR
//! modules*: the set of observable effects, their order, and the traps the
//! unoptimized module can statically prove it will take. That is a real check and
//! it catches the class of bug this contract exists to catch -- a pass that drops
//! a store, reorders a host call, or deletes a trap.
//!
//! It is not a proof of equivalence, and does not claim to be. Deciding that two
//! CFGs compute the same function is undecidable in general, and the honest way
//! to establish the remaining clauses is the one the project already uses
//! elsewhere: differential testing against Micro, which is the golden machine.
//! `docs/compiler/optimization.md` records which clause is discharged by which
//! mechanism.
//!
//! The comparison is deliberately *syntactic on effects and semantic on values*.
//! Values are compared by running both modules; effects are compared by reading
//! the two IRs. Reading the IR is what makes this check able to see a host call
//! that moved, which running the module would only show as a difference the test
//! then has to attribute.

use std::collections::HashMap;

use tpt_wasm_ir::{IrInstr, IrModule, Terminator, ValueId};
use tpt_wasm_types::Trap;

use crate::analysis::{Cfg, Dominators};

/// One observable thing a program does, in program order.
///
/// Two modules with equal effect traces perform the same observable work in the
/// same order. That is the contract's "observable ordering" clause, and it is
/// checked by comparing these traces.
///
/// ## What is deliberately *not* recorded
///
/// **Block identity.** Control-flow simplification legitimately merges two blocks
/// into one, so a trace that named blocks would report a difference for a
/// transformation that changed nothing observable. What must match instead is the
/// *shape* of the control flow -- which is checked separately, by
/// [`control_flow_shape`] -- not which block a given instruction ended up in.
///
/// **Pure computation.** Arithmetic is not recorded, because an optimizer exists to
/// remove it; recording it would make the contract "the optimizer does nothing".
/// The value clause is discharged by differential testing against Micro, not
/// here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    /// A store of `width` bytes at `offset` within a known-constant address.
    MemoryStore {
        address: u64,
        offset: u32,
        width: u32,
    },
    /// A write to a module global.
    GlobalSet(u32),
    /// A read of a module global. Recorded because a redundant global read
    /// removed by the optimizer is a legitimate optimization only if the value
    /// was genuinely not needed; recording it makes an unexpected disappearance
    /// visible.
    GlobalGet(u32),
    /// `memory.grow` by a number of pages.
    MemoryGrow,
    /// `memory.size`.
    MemorySize,
    /// A call to an imported function: the host boundary, crossed by index.
    HostCall(u32),
    /// A call to another function in the module.
    Call(u32),
    /// An indirect call through the table.
    CallIndirect { type_index: u32, table: u32 },
    /// A trap this module can reach, by the `Trap` value it carries.
    Trap(String),
    /// The function ended, returning this many values.
    Return(usize),
    /// The function ended by falling off the end, which is unreachable.
    Unreachable,
}

/// Why the contract was not satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractViolation {
    /// The two modules perform different observable work.
    EffectMismatch {
        function: usize,
        /// Where in the trace the two diverged.
        position: usize,
        expected: Effect,
        actual: Effect,
    },
    /// The optimized module reaches a block the original never reaches, or the
    /// reverse. A control-flow divergence, reported separately because it
    /// explains most effect mismatches.
    ControlFlowMismatch {
        function: usize,
        expected: usize,
        actual: usize,
    },
    /// The optimized module can trap where the original cannot.
    LostTrap {
        function: usize,
        position: usize,
        trap: String,
    },
    /// A property of the control-flow *shape* changed in a way no merge explains.
    ShapeMismatch {
        function: usize,
        /// Which property: exits, merge points, or edges.
        property: &'static str,
        expected: usize,
        actual: usize,
    },
}

impl std::fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EffectMismatch {
                function,
                position,
                expected,
                actual,
            } => write!(
                f,
                "function {function}: effect {position} is {actual:?}, but the unoptimized \
                 module has {expected:?}"
            ),
            Self::ControlFlowMismatch {
                function,
                expected,
                actual,
            } => write!(
                f,
                "function {function}: the optimized module performs {actual} observable \
                 effects, the unoptimized {expected}"
            ),
            Self::LostTrap {
                function,
                position,
                trap,
            } => write!(
                f,
                "function {function}: the unoptimized module traps with {trap} at effect \
                 {position}, and the optimized module does not"
            ),
            Self::ShapeMismatch {
                function,
                property,
                expected,
                actual,
            } => write!(
                f,
                "function {function}: the control-flow {property} changed from {expected} to \
                 {actual}, which no block merge explains"
            ),
        }
    }
}

impl std::error::Error for ContractViolation {}

/// Check the contract between an unoptimized module and an optimized one.
///
/// Returns the first violation, or `Ok(())` when every clause that can be decided
/// from the IR alone is satisfied. The *value* clause is not among them: it needs
/// execution, and it is discharged by differential testing against Micro.
pub fn verify_contract(original: &IrModule, optimized: &IrModule) -> Result<(), ContractViolation> {
    if original.functions.len() != optimized.functions.len() {
        return Err(ContractViolation::ControlFlowMismatch {
            function: original.functions.len().min(optimized.functions.len()),
            expected: original.functions.len(),
            actual: optimized.functions.len(),
        });
    }
    for (index, (before, after)) in original
        .functions
        .iter()
        .zip(&optimized.functions)
        .enumerate()
    {
        if before.function_type != after.function_type {
            return Err(ContractViolation::EffectMismatch {
                function: index,
                position: 0,
                expected: Effect::Return(before.function_type.results.0.len()),
                actual: Effect::Return(after.function_type.results.0.len()),
            });
        }
        verify_function(index, before, after)?;
    }
    Ok(())
}

/// Check one function's observable effects.
fn verify_function(
    index: usize,
    before: &tpt_wasm_ir::IrFunction,
    after: &tpt_wasm_ir::IrFunction,
) -> Result<(), ContractViolation> {
    // Control flow first. Merging two blocks and replacing a proven trap with a
    // terminator are both legitimate rewrites, so the check is on the property
    // neither can break: the *number of distinct terminators* a function can
    // reach must not fall.
    //
    // A fall is legitimate because two paths that ended the function through the
    // same `Return` block now end through different ones -- a proven trap splits
    // one shared exit into a trap and a return. The count of *kinds* is what
    // matters: a function that could return and trap still can, and one that could
    // only return still can only return.
    let before_shape = control_flow_shape(before);
    let after_shape = control_flow_shape(after);
    let before_kinds = before_shape.kinds();
    let after_kinds = after_shape.kinds();
    if after_kinds.len() < before_kinds.len() {
        return Err(ContractViolation::ShapeMismatch {
            function: index,
            property: "exit kinds",
            expected: before_kinds.len(),
            actual: after_kinds.len(),
        });
    }
    if after_shape.merges > before_shape.merges {
        return Err(ContractViolation::ShapeMismatch {
            function: index,
            property: "merge points",
            expected: before_shape.merges,
            actual: after_shape.merges,
        });
    }
    if after_shape.edges > before_shape.edges {
        return Err(ContractViolation::ShapeMismatch {
            function: index,
            property: "edges",
            expected: before_shape.edges,
            actual: after_shape.edges,
        });
    }

    // The effect trace, with `Return` and `Unreachable` left out.
    //
    // Both are *structural*, not observable work: a function that returns once and
    // a function whose two return blocks were merged into one do the same
    // observable thing, and the exit-kind check above already establishes that the
    // function can still return at all. Counting them would make block merging --
    // the single most valuable thing this pass does -- look like a contract
    // violation.
    let expected = trace(before);
    let actual = trace(after);

    // A length difference is reported as a control-flow divergence rather than
    // as a missing effect, because that is the better description: one of the
    // modules is doing a different amount of work, not the same work in a
    // different order.
    if expected.len() != actual.len() {
        return Err(ContractViolation::ControlFlowMismatch {
            function: index,
            expected: expected.len(),
            actual: actual.len(),
        });
    }

    for (position, (want, got)) in expected.iter().zip(&actual).enumerate() {
        if want == got {
            continue;
        }
        // A trap present in the original and absent from the optimized module is
        // the most serious possible failure -- it is a program that used to stop
        // and now runs to completion -- so it is named as such rather than
        // reported as a generic mismatch.
        if let Effect::Trap(trap) = want {
            if matches!(got, Effect::Trap(_)) {
                // A different trap at the same point. Reported as a mismatch,
                // which is accurate: the program's observable failure changed.
                return Err(ContractViolation::EffectMismatch {
                    function: index,
                    position,
                    expected: want.clone(),
                    actual: got.clone(),
                });
            }
            return Err(ContractViolation::LostTrap {
                function: index,
                position,
                trap: trap.clone(),
            });
        }
        return Err(ContractViolation::EffectMismatch {
            function: index,
            position,
            expected: want.clone(),
            actual: got.clone(),
        });
    }
    Ok(())
}

/// The observable effects of a function, in program order.
///
/// Built from the block graph, not from block order, so a block whose position in
/// the vector is unrelated to when it runs cannot reorder the trace. Within a
/// block, instructions are read in order, and the terminator last.
fn trace(function: &tpt_wasm_ir::IrFunction) -> Vec<Effect> {
    let cfg = Cfg::new(function);
    let dominators = Dominators::new(&cfg);
    let constants = constant_table(function);
    let mut effects: Vec<Effect> = Vec::new();

    // A block is walked once, on first arrival from a dominating path, which for
    // an acyclic region is the only arrival. A loop would need a fixpoint, so
    // each block is walked at most once: a loop body contributes its effects
    // once, which is the right comparison for "does the same work happen", and
    // the value clause is what covers the iteration count.
    //
    // The walk starts at the entry and follows *edges* below, so a block is only
    // ever reached by an edge some predecessor actually takes. That is what makes
    // an effect inside a never-taken branch invisible, which is the whole point:
    // deleting such an effect is a legitimate optimization, not lost work.
    let mut visited: Vec<bool> = vec![false; function.blocks.len()];
    walk_block(
        function,
        &cfg,
        &dominators,
        cfg.entry,
        &constants,
        &mut visited,
        &mut effects,
    );
    effects
}

#[allow(clippy::too_many_arguments)]
fn walk_block(
    function: &tpt_wasm_ir::IrFunction,
    _cfg: &Cfg,
    dominators: &Dominators,
    block_index: usize,
    constants: &HashMap<ValueId, i64>,
    visited: &mut [bool],
    effects: &mut Vec<Effect>,
) {
    if visited[block_index] {
        return;
    }
    visited[block_index] = true;
    let block = &function.blocks[block_index];
    for instruction in &block.instrs {
        if let Some(effect) = effect_of(instruction, constants) {
            effects.push(effect);
            continue;
        }
        if let Some(trap) = proven_trap(instruction, constants) {
            // An operation that provably traps is recorded as the trap it is,
            // even though it has no effect of its own. Without this the trace
            // would not see it at all, and the folder's rewriting of it into a
            // terminator would look like a trap appearing from nowhere.
            effects.push(Effect::Trap(trap));
            // The trap ends the block: nothing after it in this block runs, and
            // no successor is reached. Modelling that is what lets the folder
            // truncate the dead tail without the checker reading the truncation
            // as lost work.
            return;
        }
    }
    match &block.terminator {
        // `Return` and `Unreachable` are not recorded. Both are structural: the
        // exit-kind check above establishes that a function which could return
        // still can, and counting returns would make block merging -- a
        // legitimate rewrite -- look like lost work.
        Terminator::Return(_) => {}
        Terminator::Trap(trap) => {
            effects.push(Effect::Trap(format!("{trap:?}")));
            return;
        }
        Terminator::Unreachable => return,
        Terminator::Branch { .. } | Terminator::CondBranch { .. } => {}
    }
    // Which successors can actually be taken.
    //
    // A `CondBranch` on a *known constant* condition has only one live edge: the
    // other arm is unreachable, and an effect in it -- a call, a store -- does
    // not happen. The optimizer is expected to delete that arm, so walking both
    // would report the deletion as lost work when it is the opposite.
    //
    // This is the same reasoning as the exit-kind check, applied to edges: the
    // contract compares what the program *does*, and a branch that is never
    // taken is not something it does.
    let successors: Vec<usize> = match &block.terminator {
        Terminator::CondBranch {
            condition,
            then_target,
            else_target,
            ..
        } => {
            match constants.get(condition) {
                Some(&value) => {
                    let taken = if value != 0 {
                        *then_target
                    } else {
                        *else_target
                    };
                    _cfg.successors[block_index]
                        .iter()
                        .copied()
                        .filter(|target| *target == _cfg.index_of[&taken])
                        .collect()
                }
                // An unknown condition means both arms are live, so both are
                // walked and both arms' effects are required to survive.
                None => _cfg.successors[block_index].clone(),
            }
        }
        _ => _cfg.successors[block_index].clone(),
    };
    // Sorted deterministically, so two runs over equal modules produce equal
    // traces regardless of how the successor list was built.
    let mut successors = successors;
    successors.sort_by_key(|target| {
        dominators
            .immediate(*target)
            .map(|parent| usize::MAX - parent)
            .unwrap_or(usize::MAX)
    });
    for target in successors {
        walk_block(
            function, _cfg, dominators, target, constants, visited, effects,
        );
    }
}

/// The observable effect of an instruction, if it has one.
///
/// An address that is not a constant is recorded as a *distinct kind* of nothing
/// -- the effect is still recorded, just with a sentinel address -- so that a
/// load or store whose address became constant through optimization shows up as
/// a difference rather than being compared against a different address.
fn effect_of(instruction: &IrInstr, constants: &HashMap<ValueId, i64>) -> Option<Effect> {
    let address_of = |value: ValueId| {
        constants
            .get(&value)
            .map(|v| *v as u32 as u64)
            .unwrap_or(u64::MAX)
    };
    Some(match instruction {
        IrInstr::Store {
            address,
            offset,
            operation,
            ..
        } => Effect::MemoryStore {
            address: address_of(*address),
            offset: *offset,
            width: operation.width(),
        },
        IrInstr::GlobalSet { global, .. } => Effect::GlobalSet(*global),
        IrInstr::GlobalGet { global, .. } => Effect::GlobalGet(*global),
        IrInstr::MemoryGrow { .. } => Effect::MemoryGrow,
        IrInstr::MemorySize { .. } => Effect::MemorySize,
        IrInstr::CallHost { import, .. } => Effect::HostCall(*import),
        IrInstr::Call { function, .. } => Effect::Call(*function),
        IrInstr::CallIndirect {
            type_index, table, ..
        } => Effect::CallIndirect {
            type_index: *type_index,
            table: *table,
        },
        _ => return None,
    })
}

fn constant_table(function: &tpt_wasm_ir::IrFunction) -> HashMap<ValueId, i64> {
    let mut table = HashMap::new();
    for block in &function.blocks {
        for instruction in &block.instrs {
            match instruction {
                IrInstr::ConstI32 { result, value } => {
                    table.insert(*result, i64::from(*value));
                }
                IrInstr::ConstI64 { result, value } => {
                    table.insert(*result, *value);
                }
                _ => {}
            }
        }
    }
    table
}

/// The observable effect trace of a whole module, concatenated per function.
pub fn module_trace(module: &IrModule) -> Vec<Effect> {
    module.functions.iter().flat_map(trace).collect()
}
/// The trap an instruction provably raises, if its operands are constant.
///
/// The contract needs this because the folder is *allowed* to turn a provably
/// trapping operation into a `Trap` terminator, and the trace then has the trap
/// in a different place: as a terminator rather than as an instruction with no
/// effect. Without recognizing it, the checker would report a violation for
/// exactly the transformation the contract is supposed to permit.
///
/// Reading only constants is the right limit. Deciding that a non-constant
/// divisor is zero needs execution, and that is the differential test's job, not
/// this function's.
fn proven_trap(instruction: &IrInstr, constants: &HashMap<ValueId, i64>) -> Option<String> {
    // Only `i32` and `i64` constants are in the table. A miss is safe: it makes
    // the unoptimized module's trace *longer*, which the comparison then reports
    // rather than absorbing.
    let (left, right, width) = match instruction {
        IrInstr::I32DivS { left, right, .. }
        | IrInstr::I32DivU { left, right, .. }
        | IrInstr::I32RemS { left, right, .. }
        | IrInstr::I32RemU { left, right, .. } => (left, right, 32),
        IrInstr::I64DivS { left, right, .. }
        | IrInstr::I64DivU { left, right, .. }
        | IrInstr::I64RemS { left, right, .. }
        | IrInstr::I64RemU { left, right, .. } => (left, right, 64),
        _ => return None,
    };
    let left = constants.get(left).copied()?;
    let right = constants.get(right).copied()?;
    if right == 0 {
        return Some(format!("{:?}", Trap::IntegerDivisionByZero));
    }
    // `INT_MIN / -1` overflows, at the *instruction's* width. Comparing in a
    // widened `i64` would miss the `i32` case, because `i32::MIN as i64` is
    // not `i64::MIN` -- and that is the case the corpus exercises.
    let signed = matches!(
        instruction,
        IrInstr::I32DivS { .. } | IrInstr::I64DivS { .. }
    );
    if !signed || right != -1 {
        return None;
    }
    let is_min = match width {
        32 => i32::try_from(left).ok() == Some(i32::MIN),
        _ => left == i64::MIN,
    };
    is_min.then(|| format!("{:?}", Trap::IntegerOverflow))
}

/// The control-flow shape of a function, in a form a merge cannot change.
///
/// Control-flow simplification is allowed to merge two blocks into one, so
/// comparing the *number of blocks* would report a difference for a
/// transformation that changed nothing observable. What must survive is the
/// *shape*: how many distinct ways the function can end, and how many blocks each
/// path visits.
///
/// The counts are deliberately coarse. A finer measure -- comparing the whole
/// graph, or the block each instruction sits in -- would be defeated by every
/// legitimate rewrite and would make the check a barrier to optimization rather
/// than a guard against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFlowShape {
    /// Reachable blocks, counted once each.
    pub blocks: usize,
    /// Edges between reachable blocks, a `CondBranch` counting one per arm.
    pub edges: usize,
    /// Blocks that end the function: `Return`, `Trap`, or `Unreachable`.
    ///
    /// Reported for information. The contract does *not* compare it, because
    /// folding a provably trapping division into a `Trap` terminator splits one
    /// shared exit into two, which changes this count and changes nothing
    /// observable. [`exit_kinds`](Self::kinds) is what is compared.
    pub exits: usize,
    /// Blocks with more than one predecessor: real merge points.
    ///
    /// A merge *into* a two-predecessor block is never performed, so this count
    /// may fall when an unreachable block is removed but may never rise.
    pub merges: usize,
    /// Every exit kind a reachable path ends on, with duplicates.
    pub raw_kinds: Vec<ExitKind>,
}

/// What kind of terminator a function path can end on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    Return,
    Trap,
    Unreachable,
}

impl ControlFlowShape {
    /// The distinct exit kinds this shape permits, in a stable order.
    pub fn kinds(&self) -> Vec<ExitKind> {
        let mut kinds = self.raw_kinds.clone();
        kinds.sort_by_key(|kind| match kind {
            ExitKind::Return => 0,
            ExitKind::Trap => 1,
            ExitKind::Unreachable => 2,
        });
        kinds.dedup();
        kinds
    }
}

/// Measure a function's control-flow shape.
pub fn control_flow_shape(function: &tpt_wasm_ir::IrFunction) -> ControlFlowShape {
    let cfg = Cfg::new(function);
    let mut blocks = 0;
    let mut edges = 0;
    let mut exits = 0;
    let mut merges = 0;
    let mut raw_kinds: Vec<ExitKind> = Vec::new();
    for (index, block) in function.blocks.iter().enumerate() {
        if !cfg.is_reachable(index) {
            continue;
        }
        blocks += 1;
        edges += cfg.successors[index].len();
        if cfg.predecessors[index].len() > 1 {
            merges += 1;
        }
        if !block.terminator.successors().is_empty() {
            continue;
        }
        // A terminator with no successors ends the function. `Unreachable` and
        // `Trap` are both ends, and so is a `Return`.
        let kind = match block.terminator {
            tpt_wasm_ir::Terminator::Return(_) => ExitKind::Return,
            tpt_wasm_ir::Terminator::Trap(_) => ExitKind::Trap,
            tpt_wasm_ir::Terminator::Unreachable => ExitKind::Unreachable,
            _ => continue,
        };
        exits += 1;
        raw_kinds.push(kind);
    }
    ControlFlowShape {
        blocks,
        edges,
        exits,
        merges,
        raw_kinds,
    }
}
