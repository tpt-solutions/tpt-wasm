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

use std::collections::{HashMap, HashSet};

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
        // What threading reads about each block, taken once per round as the round's
        // snapshot. Only the terminator, emptiness and parameter count are needed, so
        // the instructions are not copied: copying every block per round made this
        // pass quadratic in the nesting depth.
        let by_id: HashMap<BlockId, Shape> = function
            .blocks
            .iter()
            .map(|block| {
                (
                    block.id,
                    Shape {
                        empty: block.instrs.is_empty(),
                        params: block.params.len(),
                        terminator: block.terminator.clone(),
                    },
                )
            })
            .collect();
        // Which blocks branch to each block, in the same round snapshot. A block that
        // is already a branch target is checked against this, so the check is a lookup
        // rather than a scan of every block.
        let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        for block in &function.blocks {
            for successor in block.terminator.successors() {
                predecessors.entry(successor).or_default().push(block.id);
            }
        }
        let mut memo: HashMap<BlockId, Option<BlockId>> = HashMap::new();
        let mut step = 0;
        for block in &mut function.blocks {
            let Some((target, values)) = thread_target(&block.terminator, &by_id, &mut memo) else {
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
            if destination.params != values.len() {
                continue;
            }
            // A block that is *already* a branch target must keep being one: a
            // second edge into it would bind its parameters a second time, from a
            // possibly different set of values.
            let entered_from_elsewhere = predecessors
                .get(&block.id)
                .is_some_and(|sources| sources.iter().any(|source| *source != block.id));
            if entered_from_elsewhere {
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

/// The parts of a block that threading reads, as they stood at the start of a round.
struct Shape {
    empty: bool,
    params: usize,
    terminator: Terminator,
}

/// The branch an empty-block chain resolves to, if the whole chain is empty.
///
/// The chain's end depends only on the chain itself, not on the edge being threaded, so
/// the ends are memoized per round in `memo`. Each block is walked once per round rather
/// than once per block that reaches it, which is what made a long chain quadratic.
fn thread_target(
    terminator: &Terminator,
    by_id: &HashMap<BlockId, Shape>,
    memo: &mut HashMap<BlockId, Option<BlockId>>,
) -> Option<(BlockId, Vec<ValueId>)> {
    let Terminator::Branch { target, values } = terminator else {
        return None;
    };
    let end = chain_end(*target, by_id, memo)?;
    Some((end, values.clone()))
}

/// The first non-empty block reached from `start` along a chain of empty blocks, or
/// `None` when the chain cannot be skipped.
///
/// A block with parameters cannot be skipped: the edge arriving at it is what binds
/// those parameters, and redirecting past the block would leave them unbound. This is
/// the case an `if`/`else` produces when one arm is empty -- both arms carry a value into
/// the merge block, and threading the empty arm would turn that one value into zero.
///
/// A link is only followed when the block's branch supplies exactly the next block's
/// parameter count, and a cycle makes the chain unskippable: a block already on the path
/// is a failure for every block on it.
fn chain_end(
    start: BlockId,
    by_id: &HashMap<BlockId, Shape>,
    memo: &mut HashMap<BlockId, Option<BlockId>>,
) -> Option<BlockId> {
    let mut path: Vec<BlockId> = Vec::new();
    let mut on_path: HashSet<BlockId> = HashSet::new();
    let mut current = start;
    let outcome = loop {
        if let Some(&known) = memo.get(&current) {
            break known;
        }
        if !on_path.insert(current) {
            break None;
        }
        path.push(current);
        let Some(shape) = by_id.get(&current) else {
            break None;
        };
        if !shape.empty {
            break Some(current);
        }
        if shape.params != 0 {
            break None;
        }
        // Nor can a block be skipped when its branch supplies values for a successor
        // that declares parameters: the target would be entered with the wrong values.
        let Terminator::Branch {
            target: next_target,
            values: next_values,
        } = &shape.terminator
        else {
            break None;
        };
        let Some(next) = by_id.get(next_target) else {
            break None;
        };
        if next.params != next_values.len() {
            break None;
        }
        current = *next_target;
    };
    for block in path {
        memo.insert(block, outcome);
    }
    outcome
}

/// Append a block to its single predecessor when that is unambiguous.
fn merge_blocks(function: &mut IrFunction) -> usize {
    // The graph is built once and kept up to date, rather than rebuilt for every merge.
    // A merge changes predecessor sets in one place: the target's successors now have
    // the predecessor as their source instead of the target. So the sets are updated
    // as each merge is made, and a merge costs the update plus the splice of the
    // target's body. Rebuilding the whole graph per merge made this pass quadratic in
    // the function's size.
    //
    // Removal is deferred to the end, so block indices stay valid while the pass runs.
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
    let mut merged = 0;
    let cfg = Cfg::new(function);
    let index_of: HashMap<BlockId, usize> = function
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect();
    // Distinct predecessors of each block, by index, as the graph stood before the pass.
    let mut predecessors: Vec<Vec<usize>> = cfg.predecessors.clone();
    let mut removed = vec![false; function.blocks.len()];
    let mut substitution: HashMap<ValueId, ValueId> = HashMap::new();
    let limit = function.blocks.len() + 1;

    let mut progressed = true;
    while progressed && merged <= limit {
        progressed = false;
        for index in 0..function.blocks.len() {
            // A block can absorb several successors in a row, so keep going from it
            // until it cannot.
            while !removed[index] && merged <= limit {
                let Some(target) =
                    mergeable_target(function, &cfg, &index_of, &predecessors, &removed, index)
                else {
                    break;
                };
                // The target's successors now have this block as their predecessor.
                let successors: Vec<usize> = function.blocks[target]
                    .terminator
                    .successors()
                    .iter()
                    .filter_map(|id| index_of.get(id).copied())
                    .collect();
                apply_merge(function, index, target, &mut substitution);
                removed[target] = true;
                for successor in successors {
                    let sources = &mut predecessors[successor];
                    sources.retain(|source| *source != target);
                    if !sources.contains(&index) {
                        sources.push(index);
                    }
                }
                predecessors[target].clear();
                merged += 1;
                progressed = true;
            }
        }
    }

    if merged > 0 {
        let mut position = 0;
        function.blocks.retain(|_| {
            let keep = !removed[position];
            position += 1;
            keep
        });
    }
    merged
}

/// The index of the block `from` may be merged with now, if there is one.
///
/// Mirrors the conditions of `find_merge`, evaluated against the graph as the pass has
/// left it: the branch is unconditional, its target is neither the entry nor `from`
/// and has not been merged away, the target has exactly one predecessor (this block),
/// the branch supplies exactly the target's parameter count, and this block was
/// reachable before the pass. A merge only moves a block's body into its predecessor,
/// so reachability is not changed by it.
fn mergeable_target(
    function: &IrFunction,
    cfg: &Cfg,
    index_of: &HashMap<BlockId, usize>,
    predecessors: &[Vec<usize>],
    removed: &[bool],
    from: usize,
) -> Option<usize> {
    let Terminator::Branch { target, values } = &function.blocks[from].terminator else {
        return None;
    };
    let to = *index_of.get(target)?;
    if removed[to] || to == cfg.entry || to == from {
        return None;
    }
    if !cfg.is_reachable(from) {
        return None;
    }
    if predecessors[to] != [from] {
        return None;
    }
    if values.len() != function.blocks[to].params.len() {
        return None;
    }
    Some(to)
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
    // The pairs are collected before any insertion, and every value is resolved
    // against the map as it stood before this merge; inserting after resolving is what
    // makes the composition order independent, so two parameters that resolve to the
    // same value cannot see each other's partial update.
    let pairs: Vec<(ValueId, ValueId)> =
        params.iter().copied().zip(values.iter().copied()).collect();
    let resolved: Vec<(ValueId, ValueId)> = pairs
        .iter()
        .map(|(param, value)| (*param, resolve(substitution, *value)))
        .collect();
    for (param, value) in resolved {
        substitution.insert(param, value);
    }
    // The map is not mutated after this point, so the lookup can borrow it directly.
    let lookup = |value: ValueId| -> ValueId { resolve(substitution, value) };

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
    // The target is not removed here: `merge_blocks` removes every merged block at the
    // end, so indices stay valid while the pass runs.
    let _ = target_id;
}

/// Follow the cumulative substitution from `value` to the value it stands for.
///
/// Bounded by the map's size, which is how a cycle would be cut if one ever arose. A
/// chain of merges is as long as the nesting, so this walk is the one place a long
/// chain costs time, and it is taken once per operand of each merged body.
fn resolve(substitution: &HashMap<ValueId, ValueId>, value: ValueId) -> ValueId {
    let mut current = value;
    for _ in 0..=substitution.len() + 1 {
        match substitution.get(&current) {
            Some(&next) if next != current => current = next,
            _ => break,
        }
    }
    current
}
