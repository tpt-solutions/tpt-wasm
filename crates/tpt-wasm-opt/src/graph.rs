// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Control-flow graph simplification.
//!
//! ## Precondition
//!
//! Verified IR.
//!
//! ## Transformations
//!
//! 1. **Unreachable-block removal.** A block no path from the entry reaches is
//!    removed. Wasm explicitly permits unreachable code after an unconditional
//!    branch, so this is a real optimization rather than an error case.
//! 2. **Empty-block threading.** An edge into a block that only branches onward
//!    is redirected to that block's target. This is jump threading, and it is
//!    what removes the empty merge blocks a lowering pass leaves behind.
//! 3. **Block merging.** A block with exactly one predecessor that ends in an
//!    unconditional branch is appended to that predecessor, with the target's
//!    parameters replaced by the branch's operands.
//! 4. **Constant-condition folding.** A `CondBranch` whose condition is a known
//!    constant becomes the `Branch` that condition selects.
//!
//! ## Semantic invariant
//!
//! The set of executions is unchanged: the same block is entered under the same
//! conditions, with the same parameter values, running the same terminator. Each
//! transformation preserves both the entry-to-terminator paths and the values
//! carried along them, so no trap, store, or host call changes position or
//! disappears.
//!
//! Merging is the one that needs care, and the restriction to a *single*
//! predecessor is exactly about the block parameter: a merge block's parameters
//! are bound per incoming edge, so a block with two predecessors has no single
//! set of values to substitute.

use std::collections::HashMap;

use tpt_wasm_ir::{BlockId, IrFunction, IrInstr, IrModule, Terminator, ValueId};

use crate::analysis::{map_operands, map_terminator_operands, Cfg};
use crate::PassOutcome;

/// Simplify the control-flow graph of every function.
pub fn run(module: &mut IrModule) -> PassOutcome {
    let mut blocks_removed = 0;
    let mut rewrites = 0;
    for function in &mut module.functions {
        let (removed, changed) = simplify(function);
        blocks_removed += removed;
        rewrites += changed;
    }
    PassOutcome {
        blocks_removed,
        rewrites,
        ..PassOutcome::default()
    }
}

/// Simplify one function. Returns `(blocks removed, other rewrites)`.
pub fn simplify(function: &mut IrFunction) -> (usize, usize) {
    let mut blocks_removed = 0;
    let mut rewrites = 0;
    // The rewrites feed each other -- merging exposes new threads, threading
    // exposes new merges -- so this iterates to a fixed point. Every round
    // strictly reduces the block count or the edge count, so it terminates; the
    // bound is only a backstop against a future rewrite that does not.
    for _ in 0..=function.blocks.len() * 2 + 2 {
        let folded = fold_constant_conditions(function);
        let removed = remove_unreachable(function);
        let threaded = thread_empty_blocks(function);
        let merged = merge_blocks(function);
        rewrites += folded + threaded + merged;
        blocks_removed += removed + merged;
        if folded + threaded + merged + removed == 0 {
            break;
        }
    }
    (blocks_removed, rewrites)
}

/// Replace a `CondBranch` on a known-constant condition with the branch it picks.
///
/// The surviving edge is the one the condition selects, carrying exactly the
/// values that arm supplied, so the taken path is bit-identical. The other arm's
/// values are dropped along with the arm, which is sound precisely because the
/// condition is a constant and the arm could never be entered.
///
/// Only instruction results are consulted. A condition that is a *local* is not
/// safe to treat as constant here, because a local is storage that a branch may
/// or may not have written, and this pass cannot see which.
fn fold_constant_conditions(function: &mut IrFunction) -> usize {
    let mut conditions: HashMap<ValueId, i32> = HashMap::new();
    for block in &function.blocks {
        for instruction in &block.instrs {
            if let IrInstr::ConstI32 { result, value } = instruction {
                conditions.insert(*result, *value);
            }
        }
    }
    let locals: Vec<ValueId> = function.locals.clone();
    let mut changes = 0;
    for block in &mut function.blocks {
        let Terminator::CondBranch {
            condition,
            then_target,
            then_values,
            else_target,
            else_values,
        } = &block.terminator
        else {
            continue;
        };
        let (condition, then_target, then_values, else_target, else_values) = (
            *condition,
            *then_target,
            then_values.clone(),
            *else_target,
            else_values.clone(),
        );
        // A local's identity does not determine its content, so a local is never
        // a constant here even if it happens to have been set from one.
        if locals.contains(&condition) {
            continue;
        }
        let Some(&value) = conditions.get(&condition) else {
            continue;
        };
        block.terminator = if value != 0 {
            Terminator::Branch {
                target: then_target,
                values: then_values,
            }
        } else {
            Terminator::Branch {
                target: else_target,
                values: else_values,
            }
        };
        changes += 1;
    }
    changes
}

/// Drop every block the entry cannot reach.
///
/// A branch may still name a removed block, and the verifier rejects a dangling
/// target even when the block is unreachable, so a block that any terminator
/// still names is kept and left unreachable. Removing it would mean inventing a
/// semantics for an edge that cannot execute, and keeping it costs nothing that
/// the backend does not already skip.
///
/// ## Why "kept and left unreachable" is still a problem
///
/// A backend lowers *every* block, unreachable ones included, and it binds each
/// block's parameters from the edges that enter it. An unreachable block whose
/// only incoming edge came from a block the optimizer deleted therefore has a
/// parameter nothing supplies, and the backend reports it as an unknown value --
/// an error that names the backend when the cause was the optimizer.
///
/// The fix is to make such a block *provably* empty rather than merely
/// unreachable: an unreachable block is replaced by an `Unreachable` terminator
/// with no parameters and no instructions. It still exists, so no edge dangles,
/// and it has nothing left to bind, so no backend can trip over it. The cost is
/// one block that can never execute, which the executor never reaches.
fn remove_unreachable(function: &mut IrFunction) -> usize {
    let cfg = Cfg::new(function);
    if function
        .blocks
        .iter()
        .enumerate()
        .all(|(i, _)| cfg.is_reachable(i))
    {
        return 0;
    }
    // Only a *reachable* block's edge can bind a target's parameters. An edge out
    // of an unreachable block binds nothing a backend has to resolve.
    //
    // Counting every edge is what left modules the backend could not lower. A dead
    // tail branched into a live block, the tail's reference kept that block's
    // parameter alive, and the parameter's definition was then merged out from
    // under the dead edge -- so a `Branch` in the tail carried a value no block
    // defined. The tail cannot execute, but a backend lowers it anyway and reports
    // an unknown value, which reads as a backend bug when it is this one.
    let mut referenced = vec![false; function.blocks.len()];
    for (index, block) in function.blocks.iter().enumerate() {
        if !cfg.is_reachable(index) {
            continue;
        }
        for successor in block.terminator.successors() {
            if let Some(&target) = cfg.index_of.get(&successor) {
                referenced[target] = true;
            }
        }
    }
    let removable: Vec<bool> = (0..function.blocks.len())
        .map(|index| !cfg.is_reachable(index) && !referenced[index])
        .collect();
    if !removable.iter().any(|&remove| remove) {
        return 0;
    }
    let before = function.blocks.len();
    function
        .blocks
        .retain(|block| !removable[cfg.index_of[&block.id]]);
    let removed = before - function.blocks.len();

    // A block that is going to be removed may still be named by an *unreachable*
    // block, and removing it would leave that name dangling. Rather than invent a
    // successor for an edge that cannot execute, the naming block is emptied: its
    // instructions and parameters go, and it ends in `Unreachable`.
    //
    // This is the only way a removed block can still be named, because a reachable
    // reference is exactly what `referenced` protects. So the invariant holds --
    // no block is removed while a block that a backend must lower still names it.
    for block in &mut function.blocks {
        if block.terminator.successors().iter().any(|successor| {
            cfg.index_of
                .get(successor)
                .is_some_and(|index| removable[*index])
        }) {
            block.instrs.clear();
            block.params.clear();
            block.terminator = Terminator::Unreachable;
        }
    }
    removed
}

/// Redirect an edge that enters a block which does nothing but branch onward.
///
/// A block qualifies when it has no instructions, no parameters, and an
/// unconditional branch. The chain is followed to its end, with a visited set so a
/// cycle of empty blocks is left alone rather than spun on.
///
/// ## The arity rule
///
/// An edge may only be redirected when the block it lands on will accept the
/// *original* edge's values. That is not automatic: threading past a block replaces
/// one edge with another, and the values on the new edge are the ones the *skipped*
/// block was going to supply. If those differ in number from the final target's
/// parameters, the target is entered with the wrong count -- a block parameter left
/// bound to a value that does not exist.
///
/// That is not hypothetical. The chain `4 -> 5 -> 6 -> 7` in a lowered
/// `br_table` carries a value through blocks 5 and 6 to block 7's parameter, and
/// threading 5's edge (which supplies *no* values) past block 5 delivers nothing
/// to a target that expects one.
fn thread_empty_blocks(function: &mut IrFunction) -> usize {
    let mut changes = 0;
    for _ in 0..=function.blocks.len() {
        let mut step = 0;
        let by_id: HashMap<BlockId, tpt_wasm_ir::BasicBlock> = function
            .blocks
            .iter()
            .map(|block| (block.id, block.clone()))
            .collect();
        for block in &mut function.blocks {
            let Some((target, values)) = thread_target(&block.terminator, &by_id) else {
                continue;
            };
            if target == block.id {
                continue;
            }
            // The redirected edge must satisfy the target it now names, using the
            // values it now carries. Without this check the rewrite is accepted
            // and the module is left referencing a parameter nothing supplies.
            let Some(destination) = by_id.get(&target) else {
                continue;
            };
            if destination.params.len() != values.len() {
                continue;
            }
            // A block that is *already* a branch target must keep being one: a
            // second edge into it would bind its parameters a second time, from a
            // possibly different set of values.
            if by_id.values().any(|other| {
                other.id != block.id && other.terminator.successors().contains(&block.id)
            }) {
                continue;
            }
            block.terminator = Terminator::Branch { target, values };
            step += 1;
        }
        changes += step;
        if step == 0 {
            break;
        }
    }
    changes
}

/// The branch an empty-block chain resolves to, if the whole chain is empty.
fn thread_target(
    terminator: &Terminator,
    by_id: &HashMap<BlockId, tpt_wasm_ir::BasicBlock>,
) -> Option<(BlockId, Vec<ValueId>)> {
    let Terminator::Branch { target, values } = terminator else {
        return None;
    };
    let mut current = by_id.get(target)?;
    let mut carried = values.clone();
    let mut seen: Vec<BlockId> = vec![current.id];
    loop {
        if !current.instrs.is_empty() {
            return Some((current.id, carried));
        }
        // A block with parameters cannot be skipped: the edge arriving at it is
        // what binds those parameters, and redirecting past the block would leave
        // them unbound. This is the case an `if`/`else` produces when one arm is
        // empty -- both arms carry a value into the merge block, and threading the
        // empty arm would turn that one value into zero.
        if !current.params.is_empty() {
            return None;
        }
        // Nor can a block be skipped when its branch supplies values for a
        // successor that declares parameters. Threading the edge past it would
        // deliver the *original* edge's values to a target that expects the
        // skipped block's, which is the same class of bug as dropping the values
        // entirely -- the target is entered with the wrong ones.
        let Terminator::Branch {
            target: next_target,
            values: next_values,
        } = &current.terminator
        else {
            return None;
        };
        let next = by_id.get(next_target)?;
        if next.params.len() != next_values.len() {
            return None;
        }
        if seen.contains(&next.id) {
            return None;
        }
        seen.push(next.id);
        carried = values.clone();
        current = next;
    }
}

/// Append a block to its single predecessor when that is unambiguous.
fn merge_blocks(function: &mut IrFunction) -> usize {
    let mut merged = 0;
    // One merge per round, recomputing the graph each time: a merge changes
    // predecessor counts, so a batch computed from one snapshot would be wrong.
    //
    // The substitution map is *carried* across merges in this call, and that is
    // the point. Merging is a chain: merging block 4 into 5 renames `v1` to `v2`,
    // and merging the result into 6 has to carry that rename with it, because the
    // body now arriving at 6 still names `v1`. A fresh map per merge loses the
    // first rename, and the operand ends up pointing at a name nothing defines.
    //
    // Carrying it is safe *only* because the map is applied to the predecessor's
    // parameters and to the incoming body -- never to unrelated blocks. A value id
    // is reused across a function, so rewriting an operand the rename does not
    // govern would silently change which value it denotes.
    let mut substitution: HashMap<ValueId, ValueId> = HashMap::new();
    loop {
        let cfg = Cfg::new(function);
        let Some((from, to)) = find_merge(function, &cfg) else {
            break;
        };
        apply_merge(function, from, to, &mut substitution);
        merged += 1;
        if merged > function.blocks.len() + 1 {
            break;
        }
    }
    merged
}

/// Perform one merge: append the target's body to its predecessor.
fn apply_merge(
    function: &mut IrFunction,
    from_index: usize,
    to_index: usize,
    substitution: &mut HashMap<ValueId, ValueId>,
) {
    let Terminator::Branch { values, .. } = &function.blocks[from_index].terminator else {
        return;
    };
    let values = values.clone();
    let target_id = function.blocks[to_index].id;
    let params = function.blocks[to_index].params.clone();
    let body = function.blocks[to_index].instrs.clone();
    let terminator = function.blocks[to_index].terminator.clone();

    // Compose this merge's own parameter substitution into the cumulative one,
    // resolving through what is already there so a chain collapses.
    //
    // The pairs are collected before any insertion because the resolver borrows
    // the map immutably and the inserts need it mutably; resolving everything
    // first and inserting after is also what makes the composition order
    // independent, so two parameters that resolve to the same value cannot see
    // each other's partial update.
    let pairs: Vec<(ValueId, ValueId)> =
        params.iter().copied().zip(values.iter().copied()).collect();
    let resolved: Vec<(ValueId, ValueId)> = pairs
        .iter()
        .map(|(param, value)| {
            let mut current = *value;
            for _ in 0..=substitution.len() + 1 {
                match substitution.get(&current) {
                    Some(&next) if next != current => current = next,
                    _ => break,
                }
            }
            (*param, current)
        })
        .collect();
    for (param, value) in resolved {
        substitution.insert(param, value);
    }
    // The lookup is a plain closure over a *snapshot*: the map is no longer
    // mutated after this point, so the borrow is unambiguous and the resolver
    // cannot observe a half-applied merge.
    let snapshot = substitution.clone();
    let lookup = move |mut value: ValueId| -> ValueId {
        for _ in 0..=snapshot.len() + 1 {
            match snapshot.get(&value) {
                Some(&next) if next != value => value = next,
                _ => return value,
            }
        }
        value
    };

    // The target's parameters are substituted only inside the body and terminator
    // that are about to be spliced into the predecessor.
    //
    // Nothing else is touched. An earlier version also renamed the predecessor's
    // own parameters and rewrote *every* block's terminator with the same map; both
    // are unsound here, because value ids are reused across a function. A value
    // defined in a block this pass has already merged away can share an id with a
    // parameter this merge renames, so the rewrite hits an unrelated definition and
    // an edge ends up carrying a value that exists but is the *wrong* one -- a
    // mistake no local check catches, because every individual rewrite is valid.
    // The target's parameters are substituted in the body and terminator being
    // spliced in, and in the predecessor's own *parameters*.
    //
    // Those are the only three places a parameter can be named: inside the
    // target's own instructions, inside its terminator, and in a block's parameter
    // list. Renaming anywhere else -- in particular inside other blocks'
    // instructions -- reaches operands the rename has no meaning for, and because
    // value ids are reused across a function those operands can collide with the
    // parameter being renamed. That turns a value the operand legitimately named
    // into a different one, and the module compiles and then computes the wrong
    // answer.
    //
    // The predecessor's parameters are included because the substitution is
    // transitive across a chain of merges: merging 4 into 5 renames `v1` to `v2`,
    // and merging the result into a block that also declares `v2` must carry the
    // earlier rename with it, or the incoming body is bound to a name whose
    // meaning has already moved.
    for param in &mut function.blocks[from_index].params {
        *param = lookup(*param);
    }

    let mut body = body;
    for instruction in &mut body {
        map_operands(instruction, &lookup);
    }
    let mut terminator = terminator;
    map_terminator_operands(&mut terminator, &lookup);

    function.blocks[from_index].instrs.extend(body);
    function.blocks[from_index].terminator = terminator;
    function.blocks.retain(|block| block.id != target_id);
}

/// The `(predecessor, target)` pair to merge, if any.
///
/// The conditions are deliberately narrow, because each one rules out a specific
/// way a merge could change the program's meaning:
///
/// * the predecessor must end in an *unconditional* branch, so there is one edge
///   and one set of values to inline;
/// * the target must have exactly one predecessor, so its block parameters are
///   bound by exactly one set of values -- a block with two predecessors is a
///   real merge point, and substituting one predecessor's values would break the
///   other;
/// * the target must not be the entry, whose parameters are the function's own;
/// * the branch must supply *exactly* the target's parameter count. This is
///   checked rather than assumed: a mismatch here would leave the target bound
///   with the wrong number of values, and the IR verifier would reject the
///   result with an arity error attributed to a pass that did nothing wrong
///   locally. Verifying it here means the merge is only attempted when it is
///   actually well-formed.
fn find_merge(function: &IrFunction, cfg: &Cfg) -> Option<(usize, usize)> {
    for (index, block) in function.blocks.iter().enumerate() {
        let Terminator::Branch { target, values } = &block.terminator else {
            continue;
        };
        let target_index = *cfg.index_of.get(target)?;
        // Never merge *into* the entry: it is where execution starts, and moving
        // a predecessor's body into it would place instructions before the
        // function's parameters are bound.
        if target_index == cfg.entry || target_index == index {
            continue;
        }
        // Never merge from an unreachable block.
        //
        // Merging substitutes the target's parameters away in *every* block, on
        // the grounds that a rename is safe anywhere. That holds for a block that
        // executes, whose operands are supplied afresh on each entry -- and it
        // fails for one that cannot, whose operand was already gone before the
        // rename and is not restored by it. A renamed edge into a block the
        // optimizer has since emptied then supplies a value nothing defines, and
        // the backend reports it as unknown even though the path is dead.
        //
        // Leaving unreachable blocks alone is not a missed optimization: nothing
        // about them is observable, and `remove_unreachable` drops them once
        // nothing names them.
        if !cfg.is_reachable(index) {
            continue;
        }
        // The branch must supply *exactly* the target's parameter count, and the
        // target must have exactly one predecessor.
        //
        // Together these are the whole correctness condition, and they are checked
        // rather than assumed. A single predecessor means the target's parameters
        // are bound by exactly one set of values -- the ones this branch supplies --
        // and the arity check then guarantees the substitution covers them all.
        // Getting this wrong does not produce a malformed module: the substituted
        // value simply no longer exists, and the resulting IR reads a slot nothing
        // wrote.
        let predecessors = &cfg.predecessors[target_index];
        if predecessors.len() != 1 || predecessors[0] != index {
            continue;
        }
        if values.len() != function.blocks[target_index].params.len() {
            continue;
        }
        return Some((index, target_index));
    }
    None
}
