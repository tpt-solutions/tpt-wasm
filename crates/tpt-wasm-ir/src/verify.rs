// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Structural and type verification for the current TPT IR slice.

use std::collections::{HashMap, HashSet};
use std::fmt;

use tpt_wasm_types::{ReferenceType, ValueType};

use super::{IrModule, ValueId};
use crate::{lower_module, LoweringError};

/// Errors found while verifying a lowered IR module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    DuplicateValue(ValueId),
    MissingBlock(super::BlockId),
    DuplicateBlock(super::BlockId),
    UnreachableBlock {
        function: usize,
    },
    BlockParameterArity {
        function: usize,
        block: super::BlockId,
        expected: usize,
        actual: usize,
    },
    DoesNotDominate {
        function: usize,
        value: ValueId,
        defined_in: super::BlockId,
        used_in: super::BlockId,
    },
    UnknownFunction(u32),
    CallArity {
        function: u32,
        expected: usize,
        actual: usize,
    },
    CallResultArity {
        function: u32,
        expected: usize,
        actual: usize,
    },
    UnsupportedBlockCount {
        function: usize,
        actual: usize,
    },
    RedefinedValue(ValueId),
    UndefinedValue(ValueId),
    UseBeforeDefinition(ValueId),
    /// A memory or global index with no corresponding declaration.
    UnknownGlobal(u32),
    /// A table index with no corresponding declaration.
    UnknownTable(u32),
    /// `call_indirect` through a table that does not hold function references.
    NotAFunctionTable(u32),
    /// A call naming a type index the module does not declare.
    UnknownType(u32),
    /// A host call naming an import the module does not declare.
    UnknownImport(u32),
    /// `global.set` naming a global declared immutable.
    ImmutableGlobal(u32),
    /// A memory instruction in a module that declares no memory.
    NoMemory,
    /// A memory access claiming its bounds were proven, where the claim does not
    /// hold against the memory's declared minimum size.
    ///
    /// The verifier re-derives the proof rather than trusting the claim, so a
    /// wrong `Proven` is caught here instead of becoming an unchecked access that
    /// reads or writes outside the buffer.
    UnprovenBounds {
        function: usize,
        /// The last byte the access needs, exclusive.
        required: u64,
        /// The memory's guaranteed size in bytes, from its declared minimum.
        available: u64,
    },
    ParameterArity {
        function: usize,
        expected: usize,
        actual: usize,
    },
    ReturnArity {
        function: usize,
        expected: usize,
        actual: usize,
    },
    TypeMismatch {
        function: usize,
        value: ValueId,
        expected: ValueType,
        actual: ValueType,
    },
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for VerificationError {}

/// Proof that an IR module passed the current structural verifier.
///
/// The private marker ensures callers obtain this value only through
/// [`verify_module`] or [`lower_and_verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrVerificationCertificate {
    _private: (),
}

/// A verified module that has not yet been mutated by compiler passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIrModule {
    module: IrModule,
    certificate: IrVerificationCertificate,
}

impl VerifiedIrModule {
    pub fn module(&self) -> &IrModule {
        &self.module
    }

    pub fn certificate(&self) -> &IrVerificationCertificate {
        &self.certificate
    }

    pub fn into_module(self) -> IrModule {
        self.module
    }
}

/// Combined errors from the lowering and verification stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerAndVerifyError {
    Lowering(LoweringError),
    Verification(VerificationError),
}

impl fmt::Display for LowerAndVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LowerAndVerifyError {}

/// Lower and verify a module without exposing an unverified intermediate value.
pub fn lower_and_verify(
    validated: &tpt_wasm_validate::ValidatedModule,
) -> Result<VerifiedIrModule, LowerAndVerifyError> {
    let module = lower_module(validated).map_err(LowerAndVerifyError::Lowering)?;
    let certificate = verify_module(&module).map_err(LowerAndVerifyError::Verification)?;
    Ok(VerifiedIrModule {
        module,
        certificate,
    })
}

/// Verify value definitions, instruction types, control flow, and returns.
pub fn verify_module(module: &IrModule) -> Result<IrVerificationCertificate, VerificationError> {
    for (function_index, function) in module.functions.iter().enumerate() {
        verify_function(function, function_index, module)?;
    }
    Ok(IrVerificationCertificate { _private: () })
}

fn verify_function(
    function: &super::IrFunction,
    function_index: usize,
    module: &IrModule,
) -> Result<(), VerificationError> {
    let values = value_types(&function.values)?;
    let order = verify_blocks(function, function_index)?;
    verify_parameters(function, function_index, &values)?;
    let scope = verify_dominance(function, function_index, &order)?;

    // Within a block, operands must already be defined. Cross-block visibility
    // comes from dominance: a block sees every value defined in a block that
    // dominates it, which is what `verify_dominance` has just established. The
    // block's *own* results are deliberately not pre-seeded, so an operand used
    // before the instruction that defines it in the same block is still caught.
    // Values that are statically known `i32` constants, used to re-derive a
    // memory access's bounds claim. A local is deliberately excluded: a local is
    // storage that `local.set` re-assigns, so "the constant this value held at
    // its definition" is not a property the value still has. A constant is
    // immutable by construction, so the claim stays true wherever it is read.
    let mut constants: HashMap<ValueId, i32> = HashMap::new();
    for block in &function.blocks {
        for instruction in &block.instrs {
            if let super::IrInstr::ConstI32 { result, value } = instruction {
                constants.insert(*result, *value);
            }
        }
    }

    for &block_index in &order {
        let block = &function.blocks[block_index];
        // Parameters and locals are entry-block storage: they exist for the whole
        // function, so they are visible in every block including the entry.
        let mut defined: HashSet<ValueId> = function
            .params
            .iter()
            .chain(&function.locals)
            .copied()
            .collect();
        // A block parameter is bound by the edge that enters the block, so it is
        // in scope for that block.
        defined.extend(block.params.iter().copied());
        // A value defined in a strictly dominating block is already initialized
        // by the time control reaches this one. The block's own results are not
        // pre-seeded, so using one before the instruction that defines it in this
        // same block is still caught below.
        for (id, defining) in &scope.def_block {
            if *defining != block_index && scope.dominators.dominates(*defining, block_index) {
                defined.insert(*id);
            }
        }
        for instruction in &block.instrs {
            verify_instruction(
                instruction,
                function_index,
                module,
                &values,
                &mut defined,
                &constants,
            )?;
        }
        verify_terminator(
            &block.terminator,
            function_index,
            &values,
            &defined,
            &function.function_type.results.0,
        )?;
    }

    // Every edge in the function must name values that exist, whether or not the
    // edge can execute.
    //
    // The loop above only walks *reachable* blocks, which is correct for
    // dominance -- an unreachable block has no dominators to speak of. But a
    // backend compiles the whole function, unreachable regions included, and the
    // optimizer is free to delete the block that would have supplied a value.
    // Checking only reachability therefore lets a pass leave an edge that names a
    // value nothing defines, which the IR verifier accepts and the backend then
    // rejects with an unrelated-sounding error.
    //
    // This is a real defect class rather than a theoretical one: it is how an
    // optimizing compiler can produce a module that verifies and then fails to
    // compile, and the failure appears to be the backend's.
    verify_all_edges(function, &values, &scope)?;
    Ok(())
}

/// Check that every branch edge names declared values.
///
/// The operand *types* are already checked by `verify_branch_types` for reachable
/// blocks; what this adds is that the values exist at all, for every block.
fn verify_all_edges(
    function: &super::IrFunction,
    values: &HashMap<ValueId, ValueType>,
    scope: &Scope,
) -> Result<(), VerificationError> {
    for block in &function.blocks {
        let edges: Vec<&[ValueId]> = match &block.terminator {
            super::Terminator::Branch { values, .. } => vec![values],
            super::Terminator::CondBranch {
                then_values,
                else_values,
                ..
            } => vec![then_values, else_values],
            _ => Vec::new(),
        };
        for operands in edges {
            for operand in operands {
                // The value must be *declared* and defined *somewhere* in the
                // function. Requiring more than that would be wrong: an edge may
                // legitimately carry a value defined in a block that does not
                // dominate the edge's source. Requiring nothing is also wrong: a
                // pass that deletes the definition leaves a backend error about a
                // value it cannot find, several passes after the mistake.
                if !values.contains_key(operand) || !scope.def_block.contains_key(operand) {
                    return Err(VerificationError::UndefinedValue(*operand));
                }
            }
        }
    }
    Ok(())
}

fn value_types(
    values: &[super::IrValue],
) -> Result<HashMap<ValueId, ValueType>, VerificationError> {
    let mut types = HashMap::with_capacity(values.len());
    for value in values {
        if types.insert(value.id, value.value_type).is_some() {
            return Err(VerificationError::DuplicateValue(value.id));
        }
    }
    Ok(types)
}

/// Verify block structure and return block indices in reverse postorder.
///
/// Reverse postorder guarantees a block is visited before any block reachable
/// from it along a non-back edge, which is what makes the dominator fixpoint
/// converge. An unreachable block is reported rather than skipped so a dead but
/// malformed region cannot hide behind a live one.
fn verify_blocks(
    function: &super::IrFunction,
    function_index: usize,
) -> Result<Vec<usize>, VerificationError> {
    if function.blocks.is_empty() {
        return Err(VerificationError::MissingBlock(function.entry));
    }
    let mut index_of = HashMap::with_capacity(function.blocks.len());
    for (index, block) in function.blocks.iter().enumerate() {
        if index_of.insert(block.id, index).is_some() {
            return Err(VerificationError::DuplicateBlock(block.id));
        }
    }
    let entry = *index_of
        .get(&function.entry)
        .ok_or(VerificationError::MissingBlock(function.entry))?;

    // Resolve every edge once, reporting a dangling target here.
    let mut successors: Vec<Vec<usize>> = Vec::with_capacity(function.blocks.len());
    for block in &function.blocks {
        let mut edges = Vec::new();
        for successor in block.terminator.successors() {
            let target = *index_of
                .get(&successor)
                .ok_or(VerificationError::MissingBlock(successor))?;
            edges.push(target);
        }
        successors.push(edges);
    }

    // Iterative depth-first postorder, to avoid deep recursion on long bodies.
    let mut postorder = Vec::with_capacity(function.blocks.len());
    let mut visited = vec![false; function.blocks.len()];
    let mut stack = vec![(entry, false)];
    while let Some((index, finished)) = stack.pop() {
        if finished {
            postorder.push(index);
            continue;
        }
        if visited[index] {
            continue;
        }
        visited[index] = true;
        stack.push((index, true));
        for &successor in successors[index].iter().rev() {
            if !visited[successor] {
                stack.push((successor, false));
            }
        }
    }
    postorder.reverse();

    // A block the lowering allocated but never wired up is dead weight, not a
    // malformed CFG. Wasm explicitly permits unreachable code after an
    // unconditional branch, so unreachable blocks are not an error; they simply
    // cannot affect the function's result and are excluded from the
    // dominance, definition, and branch-type checks that follow.
    //
    // What is still required is that a *reachable* block never supplies a value
    // to a target whose parameters it does not fill, which `verify_branch_types`
    // checks below.

    // Every incoming edge must supply exactly the target's parameter count.
    for (index, block) in function.blocks.iter().enumerate() {
        for predecessor in predecessors_of(&successors, index) {
            let Some(incoming) = incoming_arity(&function.blocks[predecessor].terminator, block.id)
            else {
                // `predecessors_of` only reports blocks that actually target this
                // one, so a `None` here means the two disagree about the graph --
                // which is a verifier bug rather than a malformed module, and
                // reporting it as an arity error would point the reader at the
                // wrong thing.
                continue;
            };
            if incoming != block.params.len() {
                return Err(VerificationError::BlockParameterArity {
                    function: function_index,
                    block: block.id,
                    expected: block.params.len(),
                    actual: incoming,
                });
            }
        }
    }
    Ok(postorder)
}

/// Predecessor block indices of `target`, in block order.
fn predecessors_of(successors: &[Vec<usize>], target: usize) -> Vec<usize> {
    successors
        .iter()
        .enumerate()
        .filter(|(_, edges)| edges.contains(&target))
        .map(|(index, _)| index)
        .collect()
}

/// The number of values a terminator supplies to the block at `target_id`.
///
/// `target_id` is a *block id*, not a block position, and the two are not
/// interchangeable. Comparing a position against a `BlockId` happens to work only
/// while ids and positions coincide, which is why this bug survived: a function
/// whose blocks are laid out in id order made the two equal by construction.
///
/// Two further cases it got wrong:
///
/// * a `CondBranch` whose *then* arm targets a different block fell through to
///   the else arm's values, so an unrelated target could be checked against the
///   wrong list;
/// * a block that is the target of *neither* arm returned `0`, which happened to
///   be right for the caller only because such a block is not a predecessor --
///   but the arity check iterates predecessors, so the `0` case was only ever
///   reached through the same id/position confusion.
///
/// The function now answers for one specific id and is only ever called for a
/// block that is a real predecessor, so both arms are checked independently and a
/// non-matching target returns `None` rather than a misleading `0`.
fn incoming_arity(terminator: &super::Terminator, target_id: super::BlockId) -> Option<usize> {
    match terminator {
        super::Terminator::Branch { target, values } => {
            (*target == target_id).then_some(values.len())
        }
        super::Terminator::CondBranch {
            then_target,
            then_values,
            else_target,
            else_values,
            ..
        } => {
            // Both arms are checked: a `CondBranch` may target the same block from
            // either side, and each supplies its own values. Returning the first
            // match would silently accept an arm whose arity is wrong.
            if *then_target == target_id {
                return Some(then_values.len());
            }
            if *else_target == target_id {
                return Some(else_values.len());
            }
            None
        }
        _ => None,
    }
}

/// Immediate dominators by block index; `usize::MAX` means not yet computed.
struct Dominators {
    idom: Vec<usize>,
    depth: Vec<usize>,
}

impl Dominators {
    /// Does `ancestor` dominate `node`? A block always dominates itself.
    ///
    /// The walk is bounded by the tree depth so a malformed `idom` cycle cannot
    /// spin here; verification reports the structural problem separately.
    fn dominates(&self, ancestor: usize, node: usize) -> bool {
        let mut current = node;
        for _ in 0..=self.depth.len() {
            if current == ancestor {
                return true;
            }
            let next = self.idom[current];
            // The comparison must be strict. `next` may *be* the ancestor, in
            // which case the loop head is what recognizes it; bailing on
            // `depth[next] <= depth[ancestor]` would report the ancestor as not
            // dominating itself's descendants. Bailing only when `next` is
            // strictly above the ancestor is safe, because then the ancestor is
            // not on this path.
            if next == usize::MAX || next == current || self.depth[next] < self.depth[ancestor] {
                return false;
            }
            current = next;
        }
        false
    }
}

/// Compute immediate dominators with the iterative Cooper–Harvey–Kennedy
/// fixpoint over reverse postorder.
fn compute_dominators(
    function: &super::IrFunction,
    entry: usize,
    order: &[usize],
    successors: &[Vec<usize>],
) -> Dominators {
    let count = function.blocks.len();
    let mut position = vec![usize::MAX; count];
    for (rank, index) in order.iter().enumerate() {
        position[*index] = rank;
    }
    let mut idom = vec![usize::MAX; count];
    idom[entry] = entry;
    // The fixpoint converges in at most one pass per block. The bound is a
    // backstop so a malformed graph can never spin here.
    for _ in 0..=count {
        let mut changed = false;
        for &block in order {
            if block == entry {
                continue;
            }
            let mut candidate = usize::MAX;
            for predecessor in predecessors_of(successors, block) {
                if idom[predecessor] == usize::MAX {
                    continue;
                }
                candidate = match candidate {
                    usize::MAX => predecessor,
                    current => intersect(&idom, &position, current, predecessor),
                };
            }
            if candidate != usize::MAX && idom[block] != candidate {
                idom[block] = candidate;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Depth follows from the dominator tree, whose parent always has a smaller
    // reverse-postorder rank than its child.
    let mut depth = vec![0usize; count];
    for &block in order {
        if block != entry && idom[block] != usize::MAX {
            depth[block] = depth[idom[block]] + 1;
        }
    }
    Dominators { idom, depth }
}

/// Intersect two dominator paths, walking whichever is later in postorder up
/// until the two meet.
///
/// Both walks are bounded by the number of blocks. The entry dominates itself,
/// so a cycle through it must stop there rather than follow `idom` forever.
fn intersect(idom: &[usize], position: &[usize], mut a: usize, mut b: usize) -> usize {
    for _ in 0..=idom.len() {
        while a != b && position[a] > position[b] {
            let next = idom[a];
            if next == usize::MAX || next == a {
                return b;
            }
            a = next;
        }
        while a != b && position[b] > position[a] {
            let next = idom[b];
            if next == usize::MAX || next == b {
                return a;
            }
            b = next;
        }
        if a == b {
            return a;
        }
    }
    // The two paths never met; the later block is the safer answer.
    if position[a] <= position[b] {
        a
    } else {
        b
    }
}

/// Check that every used value is dominated by its definition, that no value is
/// defined twice, and that branch operands match the target parameter types.
///
/// Returns the dominator tree and the defining block of every value, so the
/// caller can scope each block's visible values the same way.
fn verify_dominance(
    function: &super::IrFunction,
    function_index: usize,
    order: &[usize],
) -> Result<Scope, VerificationError> {
    let values = value_types(&function.values)?;
    let entry = function
        .blocks
        .iter()
        .position(|block| block.id == function.entry)
        .ok_or(VerificationError::MissingBlock(function.entry))?;
    let successors = resolve_successors(function)?;
    let dominators = compute_dominators(function, entry, order, &successors);

    // Record the defining block of every value. Parameters and locals are
    // entry-block values; block parameters and instruction results are defined
    // in the block that introduces them.
    //
    // A local is storage rather than an SSA register: it exists for the whole
    // function and is re-assigned by `local.set`. Treating it as dominating
    // only the entry block would reject the ordinary pattern of writing a local
    // inside a branch and reading it afterwards, so locals are marked as
    // dominating the entire function.
    let mut def_block: HashMap<ValueId, usize> = HashMap::new();
    for id in function.params.iter().chain(&function.locals) {
        def_block.insert(*id, entry);
    }
    let local_set: HashSet<ValueId> = function.locals.iter().copied().collect();
    // Definitions are recorded for every block, not only reachable ones. A merge
    // block's parameters are declared even if the block is never entered, and
    // leaving them undefined would make an otherwise valid function look like
    // it reads an uninitialized value.
    for block_index in 0..function.blocks.len() {
        for id in &function.blocks[block_index].params {
            if def_block.insert(*id, block_index).is_some() {
                return Err(VerificationError::RedefinedValue(*id));
            }
        }
        for instruction in &function.blocks[block_index].instrs {
            for result in instruction_results(instruction) {
                if def_block.insert(result, block_index).is_some() {
                    return Err(VerificationError::RedefinedValue(result));
                }
            }
        }
    }

    for &block_index in order {
        let block = &function.blocks[block_index];
        let mut used_here: Vec<ValueId> = Vec::new();
        for instruction in &block.instrs {
            used_here.extend(instruction_operands(instruction));
        }
        used_here.extend(terminator_operands(&block.terminator));
        for used in used_here {
            let defining = *def_block
                .get(&used)
                .ok_or(VerificationError::UndefinedValue(used))?;
            // A local written in a branch is still readable on every path,
            // because the local's initial value is always present.
            if local_set.contains(&used) || dominators.dominates(defining, block_index) {
                continue;
            }
            return Err(VerificationError::DoesNotDominate {
                function: function_index,
                value: used,
                defined_in: function.blocks[defining].id,
                used_in: block.id,
            });
        }
    }

    // A declared value that no instruction, block parameter, parameter, or local
    // defines would be an uninitialized read. Every declaration is recorded in
    // `def_block` above, so a value absent from the table is one the lowering
    // never emitted a definition for.
    for value in &function.values {
        if !def_block.contains_key(&value.id) {
            return Err(VerificationError::UndefinedValue(value.id));
        }
    }
    verify_branch_types(function, function_index, &values)?;
    Ok(Scope {
        dominators,
        def_block,
    })
}

/// What each block can see: the dominator tree and where every value is defined.
struct Scope {
    dominators: Dominators,
    def_block: HashMap<ValueId, usize>,
}

/// Map each terminator's target block IDs to block indices.
fn resolve_successors(function: &super::IrFunction) -> Result<Vec<Vec<usize>>, VerificationError> {
    let mut index_of = HashMap::with_capacity(function.blocks.len());
    for (index, block) in function.blocks.iter().enumerate() {
        index_of.insert(block.id, index);
    }
    let mut resolved = Vec::with_capacity(function.blocks.len());
    for block in &function.blocks {
        let mut edges = Vec::new();
        for successor in block.terminator.successors() {
            edges.push(
                *index_of
                    .get(&successor)
                    .ok_or(VerificationError::MissingBlock(successor))?,
            );
        }
        resolved.push(edges);
    }
    Ok(resolved)
}

/// Branch operands must agree with the target block's parameter types.
fn verify_branch_types(
    function: &super::IrFunction,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
) -> Result<(), VerificationError> {
    for block in &function.blocks {
        let edges: Vec<(super::BlockId, &[ValueId])> = match &block.terminator {
            super::Terminator::Branch { target, values } => vec![(*target, values)],
            super::Terminator::CondBranch {
                then_target,
                then_values,
                else_target,
                else_values,
                ..
            } => vec![(*then_target, then_values), (*else_target, else_values)],
            _ => Vec::new(),
        };
        for (target, args) in edges {
            let target_block = function
                .blocks
                .iter()
                .find(|candidate| candidate.id == target)
                .ok_or(VerificationError::MissingBlock(target))?;
            for (argument, parameter) in args.iter().zip(&target_block.params) {
                expect_type(
                    *argument,
                    value_type(*parameter, values)?,
                    function_index,
                    values,
                )?;
            }
        }
    }
    Ok(())
}

/// Values a terminator reads.
fn terminator_operands(terminator: &super::Terminator) -> Vec<ValueId> {
    match terminator {
        super::Terminator::Return(values) => values.clone(),
        super::Terminator::Branch { values, .. } => values.clone(),
        super::Terminator::CondBranch {
            condition,
            then_values,
            else_values,
            ..
        } => {
            let mut operands = Vec::with_capacity(1 + then_values.len() + else_values.len());
            operands.push(*condition);
            operands.extend(then_values.iter().copied());
            operands.extend(else_values.iter().copied());
            operands
        }
        super::Terminator::Trap(_) | super::Terminator::Unreachable => Vec::new(),
    }
}

/// A function's return terminator must match its declared result arity/types.
fn verify_results(
    results: &[ValueId],
    expected_types: &[ValueType],
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
    defined: &HashSet<ValueId>,
) -> Result<(), VerificationError> {
    if results.len() != expected_types.len() {
        return Err(VerificationError::ReturnArity {
            function: function_index,
            expected: expected_types.len(),
            actual: results.len(),
        });
    }
    for (id, expected) in results.iter().zip(expected_types) {
        expect_defined_type(*id, *expected, function_index, values, defined)?;
    }
    Ok(())
}

/// Within-block terminator checks that do not depend on dominance.
fn verify_terminator(
    terminator: &super::Terminator,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
    defined: &HashSet<ValueId>,
    result_types: &[ValueType],
) -> Result<(), VerificationError> {
    match terminator {
        super::Terminator::Return(results) => {
            verify_results(results, result_types, function_index, values, defined)
        }
        super::Terminator::Branch { values: args, .. } => {
            for id in args {
                require_defined(*id, values, defined)?;
            }
            Ok(())
        }
        super::Terminator::CondBranch {
            condition,
            then_values,
            else_values,
            ..
        } => {
            expect_defined_type(*condition, ValueType::I32, function_index, values, defined)?;
            for id in then_values.iter().chain(else_values) {
                require_defined(*id, values, defined)?;
            }
            Ok(())
        }
        super::Terminator::Trap(_) | super::Terminator::Unreachable => Ok(()),
    }
}

/// Values an instruction reads.
///
/// The dataflow instructions each have a distinct operand set and are listed
/// explicitly; the numeric instructions all share a `left`/`right` or single
/// `value` shape and are matched by pattern.
fn instruction_operands(instruction: &super::IrInstr) -> Vec<ValueId> {
    use super::IrInstr::*;
    match instruction {
        // Constants read nothing.
        ConstI32 { .. } | ConstI64 { .. } | ConstF32 { .. } | ConstF64 { .. } => Vec::new(),
        // A reference type and a function index are both immediates, so these
        // two read nothing either.
        RefNull { .. } | RefFunc { .. } => Vec::new(),
        // `ref.is_null` reads the reference it tests.
        RefIsNull { value, .. } => vec![*value],
        // A get names its local; a set and tee read the stored value, and the
        // local they name is a pre-allocated slot rather than an operand.
        LocalGet { local, .. } => vec![*local],
        LocalSet { value, .. } | LocalTee { value, .. } => vec![*value],
        Drop { value } => vec![*value],
        Select {
            condition,
            left,
            right,
            ..
        } => vec![*condition, *left, *right],
        // A function index is not a value; only the arguments are read.
        Call { arguments, .. } => arguments.clone(),
        // A host call reads its arguments too; the import index is an immediate.
        CallHost { arguments, .. } => arguments.clone(),
        // An indirect call reads its arguments and its table-index operand.
        CallIndirect {
            arguments, operand, ..
        } => {
            let mut read = arguments.clone();
            read.push(*operand);
            read
        }
        // A load reads its address operand; a store reads address and value.
        Load { address, .. } => vec![*address],
        Store { address, value, .. } => vec![*address, *value],
        // `memory.size` reads nothing; `memory.grow` reads the page delta.
        MemorySize { .. } => Vec::new(),
        MemoryGrow { delta, .. } => vec![*delta],
        // A global index is not a value; a set reads the value it stores.
        GlobalGet { .. } => Vec::new(),
        GlobalSet { value, .. } => vec![*value],
        // The remaining instructions are unary or binary numeric.
        I32Compare { left, right, .. }
        | I64Compare { left, right, .. }
        | F32Compare { left, right, .. }
        | F64Compare { left, right, .. }
        | I32Add { left, right, .. }
        | I32Sub { left, right, .. }
        | I32Mul { left, right, .. }
        | I32DivS { left, right, .. }
        | I32DivU { left, right, .. }
        | I32RemS { left, right, .. }
        | I32RemU { left, right, .. }
        | I32Shl { left, right, .. }
        | I32ShrS { left, right, .. }
        | I32ShrU { left, right, .. }
        | I32Rotl { left, right, .. }
        | I32Rotr { left, right, .. }
        | I32And { left, right, .. }
        | I32Or { left, right, .. }
        | I32Xor { left, right, .. }
        | I64Add { left, right, .. }
        | I64Sub { left, right, .. }
        | I64Mul { left, right, .. }
        | I64DivS { left, right, .. }
        | I64DivU { left, right, .. }
        | I64RemS { left, right, .. }
        | I64RemU { left, right, .. }
        | I64Shl { left, right, .. }
        | I64ShrS { left, right, .. }
        | I64ShrU { left, right, .. }
        | I64Rotl { left, right, .. }
        | I64Rotr { left, right, .. }
        | I64And { left, right, .. }
        | I64Or { left, right, .. }
        | I64Xor { left, right, .. }
        | F32Add { left, right, .. }
        | F32Sub { left, right, .. }
        | F32Mul { left, right, .. }
        | F32Div { left, right, .. }
        | F32Min { left, right, .. }
        | F32Max { left, right, .. }
        | F32Copysign { left, right, .. }
        | F64Add { left, right, .. }
        | F64Sub { left, right, .. }
        | F64Mul { left, right, .. }
        | F64Div { left, right, .. }
        | F64Min { left, right, .. }
        | F64Max { left, right, .. }
        | F64Copysign { left, right, .. } => vec![*left, *right],
        I32Eqz { value, .. }
        | I64Eqz { value, .. }
        | I32Unary { value, .. }
        | I64Unary { value, .. }
        | F32Unary { value, .. }
        | F64Unary { value, .. }
        | IntConvert { value, .. }
        | Reinterpret { value, .. }
        | FloatConvert { value, .. }
        | FloatTrunc { value, .. }
        | SignExtend { value, .. } => vec![*value],
    }
}

/// Values an instruction defines.
pub fn instruction_results(instruction: &super::IrInstr) -> Vec<ValueId> {
    use super::IrInstr::*;
    match instruction {
        Call { results, .. } => results.clone(),
        CallHost { results, .. } => results.clone(),
        CallIndirect { results, .. } => results.clone(),
        // Neither a drop, a store, nor a `global.set` produces a new value.
        Drop { .. } | LocalSet { .. } | Store { .. } | GlobalSet { .. } => Vec::new(),
        LocalGet { result, .. } | LocalTee { result, .. } | Select { result, .. } => {
            vec![*result]
        }
        // Every remaining instruction produces exactly one `result`.
        Load { result, .. }
        | MemorySize { result }
        | MemoryGrow { result, .. }
        | GlobalGet { result, .. }
        | RefNull { result, .. }
        | RefFunc { result, .. }
        | RefIsNull { result, .. }
        | ConstI32 { result, .. }
        | ConstI64 { result, .. }
        | ConstF32 { result, .. }
        | ConstF64 { result, .. }
        | I32Eqz { result, .. }
        | I64Eqz { result, .. }
        | I32Compare { result, .. }
        | I64Compare { result, .. }
        | F32Compare { result, .. }
        | F64Compare { result, .. }
        | I32Add { result, .. }
        | I32Sub { result, .. }
        | I32Mul { result, .. }
        | I32DivS { result, .. }
        | I32DivU { result, .. }
        | I32RemS { result, .. }
        | I32RemU { result, .. }
        | I32Shl { result, .. }
        | I32ShrS { result, .. }
        | I32ShrU { result, .. }
        | I32Rotl { result, .. }
        | I32Rotr { result, .. }
        | I32And { result, .. }
        | I32Or { result, .. }
        | I32Xor { result, .. }
        | I64Add { result, .. }
        | I64Sub { result, .. }
        | I64Mul { result, .. }
        | I64DivS { result, .. }
        | I64DivU { result, .. }
        | I64RemS { result, .. }
        | I64RemU { result, .. }
        | I64Shl { result, .. }
        | I64ShrS { result, .. }
        | I64ShrU { result, .. }
        | I64Rotl { result, .. }
        | I64Rotr { result, .. }
        | I64And { result, .. }
        | I64Or { result, .. }
        | I64Xor { result, .. }
        | F32Add { result, .. }
        | F32Sub { result, .. }
        | F32Mul { result, .. }
        | F32Div { result, .. }
        | F32Min { result, .. }
        | F32Max { result, .. }
        | F32Copysign { result, .. }
        | F64Add { result, .. }
        | F64Sub { result, .. }
        | F64Mul { result, .. }
        | F64Div { result, .. }
        | F64Min { result, .. }
        | F64Max { result, .. }
        | F64Copysign { result, .. }
        | I32Unary { result, .. }
        | I64Unary { result, .. }
        | F32Unary { result, .. }
        | F64Unary { result, .. }
        | IntConvert { result, .. }
        | Reinterpret { result, .. }
        | FloatConvert { result, .. }
        | FloatTrunc { result, .. }
        | SignExtend { result, .. } => vec![*result],
    }
}

fn verify_parameters(
    function: &super::IrFunction,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
) -> Result<(), VerificationError> {
    let expected = function.function_type.params.0.len();
    if function.params.len() != expected {
        return Err(VerificationError::ParameterArity {
            function: function_index,
            expected,
            actual: function.params.len(),
        });
    }
    for (id, expected_type) in function.params.iter().zip(&function.function_type.params.0) {
        expect_type(*id, *expected_type, function_index, values)?;
    }
    for id in &function.locals {
        value_type(*id, values)?;
    }
    Ok(())
}

fn value_type(
    id: ValueId,
    values: &HashMap<ValueId, ValueType>,
) -> Result<ValueType, VerificationError> {
    values
        .get(&id)
        .copied()
        .ok_or(VerificationError::UndefinedValue(id))
}

fn expect_type(
    id: ValueId,
    expected: ValueType,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
) -> Result<(), VerificationError> {
    let actual = value_type(id, values)?;
    if actual == expected {
        Ok(())
    } else {
        Err(VerificationError::TypeMismatch {
            function: function_index,
            value: id,
            expected,
            actual,
        })
    }
}

fn verify_instruction(
    instruction: &super::IrInstr,
    function_index: usize,
    module: &IrModule,
    values: &HashMap<ValueId, ValueType>,
    defined: &mut HashSet<ValueId>,
    constants: &HashMap<ValueId, i32>,
) -> Result<(), VerificationError> {
    match instruction {
        super::IrInstr::ConstI32 { result, .. } => {
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::ConstI64 { result, .. } => {
            define_value(*result, ValueType::I64, function_index, values, defined)
        }
        super::IrInstr::ConstF32 { result, .. } => {
            define_value(*result, ValueType::F32, function_index, values, defined)
        }
        super::IrInstr::ConstF64 { result, .. } => {
            define_value(*result, ValueType::F64, function_index, values, defined)
        }
        super::IrInstr::Drop { value } => {
            require_defined(*value, values, defined)?;
            Ok(())
        }
        super::IrInstr::Select {
            result,
            condition,
            left,
            right,
        } => {
            expect_defined_type(*condition, ValueType::I32, function_index, values, defined)?;
            let left_type = value_type(*left, values)?;
            expect_defined_type(*right, left_type, function_index, values, defined)?;
            define_value(*result, left_type, function_index, values, defined)
        }
        super::IrInstr::LocalGet { result, local } => {
            let value_type = value_type(*local, values)?;
            require_defined(*local, values, defined)?;
            define_value(*result, value_type, function_index, values, defined)
        }
        super::IrInstr::LocalSet { local, value } => {
            let value_type = value_type(*local, values)?;
            require_defined(*local, values, defined)?;
            expect_defined_type(*value, value_type, function_index, values, defined)
        }
        super::IrInstr::LocalTee {
            result,
            local,
            value,
        } => {
            let value_type = value_type(*local, values)?;
            require_defined(*local, values, defined)?;
            expect_defined_type(*value, value_type, function_index, values, defined)?;
            define_value(*result, value_type, function_index, values, defined)
        }
        super::IrInstr::I32Eqz { result, value } => {
            expect_defined_type(*value, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I32Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::I32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Eqz { result, value } => {
            expect_defined_type(*value, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::I64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I32Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I64, function_index, values, defined)
        }
        super::IrInstr::I32Add {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::I32DivS {
            result,
            left,
            right,
        }
        | super::IrInstr::I32DivU {
            result,
            left,
            right,
        }
        | super::IrInstr::I32RemS {
            result,
            left,
            right,
        }
        | super::IrInstr::I32RemU {
            result,
            left,
            right,
        }
        | super::IrInstr::I32And {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Or {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Xor {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Shl {
            result,
            left,
            right,
        }
        | super::IrInstr::I32ShrS {
            result,
            left,
            right,
        }
        | super::IrInstr::I32ShrU {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Rotl {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Rotr {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::I32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Add {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::I64DivS {
            result,
            left,
            right,
        }
        | super::IrInstr::I64DivU {
            result,
            left,
            right,
        }
        | super::IrInstr::I64RemS {
            result,
            left,
            right,
        }
        | super::IrInstr::I64RemU {
            result,
            left,
            right,
        }
        | super::IrInstr::I64And {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Or {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Xor {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Shl {
            result,
            left,
            right,
        }
        | super::IrInstr::I64ShrS {
            result,
            left,
            right,
        }
        | super::IrInstr::I64ShrU {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Rotl {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Rotr {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::I64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I64, function_index, values, defined)
        }
        super::IrInstr::F32Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::F32, function_index, values, defined)?;
            define_value(*result, ValueType::F32, function_index, values, defined)
        }
        super::IrInstr::F64Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::F64, function_index, values, defined)?;
            define_value(*result, ValueType::F64, function_index, values, defined)
        }
        super::IrInstr::IntConvert {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::Reinterpret {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::SignExtend {
            result,
            value,
            operation,
        } => {
            let value_type = operation.value_type();
            expect_defined_type(*value, value_type, function_index, values, defined)?;
            define_value(*result, value_type, function_index, values, defined)
        }
        super::IrInstr::FloatConvert {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::FloatTrunc {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::F32Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::F32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::F64Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::F64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F64, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::F32Add {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Div {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Min {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Max {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Copysign {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::F32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F32, function_index, values, defined)?;
            define_value(*result, ValueType::F32, function_index, values, defined)
        }
        super::IrInstr::F64Add {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Div {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Min {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Max {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Copysign {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::F64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F64, function_index, values, defined)?;
            define_value(*result, ValueType::F64, function_index, values, defined)
        }
        super::IrInstr::Call {
            function,
            arguments,
            results,
        } => {
            let callee_index = usize::try_from(*function)
                .map_err(|_| VerificationError::UnknownFunction(*function))?;
            let callee = module
                .functions
                .get(callee_index)
                .ok_or(VerificationError::UnknownFunction(*function))?;
            if arguments.len() != callee.function_type.params.0.len() {
                return Err(VerificationError::CallArity {
                    function: *function,
                    expected: callee.function_type.params.0.len(),
                    actual: arguments.len(),
                });
            }
            for (argument, expected) in arguments.iter().zip(&callee.function_type.params.0) {
                expect_defined_type(*argument, *expected, function_index, values, defined)?;
            }
            if results.len() != callee.function_type.results.0.len() {
                return Err(VerificationError::CallResultArity {
                    function: *function,
                    expected: callee.function_type.results.0.len(),
                    actual: results.len(),
                });
            }
            for (result, expected) in results.iter().zip(&callee.function_type.results.0) {
                define_value(*result, *expected, function_index, values, defined)?;
            }
            Ok(())
        }
        super::IrInstr::CallHost {
            import,
            arguments,
            results,
        } => {
            // The signature is checked here, against the import's declared type,
            // so a host call cannot reach the boundary with the wrong arity or
            // operand types even if the module is hand-built.
            let declaration = module
                .imports
                .get(*import as usize)
                .ok_or(VerificationError::UnknownImport(*import))?;
            let callee_type = &declaration.function_type;
            if arguments.len() != callee_type.params.0.len() {
                return Err(VerificationError::CallArity {
                    function: *import,
                    expected: callee_type.params.0.len(),
                    actual: arguments.len(),
                });
            }
            for (argument, expected) in arguments.iter().zip(&callee_type.params.0) {
                expect_defined_type(*argument, *expected, function_index, values, defined)?;
            }
            if results.len() != callee_type.results.0.len() {
                return Err(VerificationError::CallResultArity {
                    function: *import,
                    expected: callee_type.results.0.len(),
                    actual: results.len(),
                });
            }
            for (result, expected) in results.iter().zip(&callee_type.results.0) {
                define_value(*result, *expected, function_index, values, defined)?;
            }
            Ok(())
        }
        super::IrInstr::Load {
            result,
            address,
            offset,
            operation,
            bounds,
        } => {
            let memory = module.memory.as_ref().ok_or(VerificationError::NoMemory)?;
            expect_defined_type(*address, ValueType::I32, function_index, values, defined)?;
            verify_proven_bounds(
                *bounds,
                *address,
                *offset,
                u64::from(operation.width()),
                memory,
                function_index,
                constants,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::Store {
            address,
            value,
            offset,
            operation,
            bounds,
        } => {
            let memory = module.memory.as_ref().ok_or(VerificationError::NoMemory)?;
            expect_defined_type(*address, ValueType::I32, function_index, values, defined)?;
            expect_defined_type(
                *value,
                operation.operand_type(),
                function_index,
                values,
                defined,
            )?;
            verify_proven_bounds(
                *bounds,
                *address,
                *offset,
                u64::from(operation.width()),
                memory,
                function_index,
                constants,
            )
        }
        super::IrInstr::MemorySize { result } => {
            if module.memory.is_none() {
                return Err(VerificationError::NoMemory);
            }
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::MemoryGrow { result, delta } => {
            if module.memory.is_none() {
                return Err(VerificationError::NoMemory);
            }
            expect_defined_type(*delta, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::GlobalGet { result, global } => {
            let declaration = module
                .globals
                .get(*global as usize)
                .ok_or(VerificationError::UnknownGlobal(*global))?;
            define_value(
                *result,
                declaration.value_type,
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::GlobalSet { global, value } => {
            let declaration = module
                .globals
                .get(*global as usize)
                .ok_or(VerificationError::UnknownGlobal(*global))?;
            if !declaration.mutable {
                return Err(VerificationError::ImmutableGlobal(*global));
            }
            expect_defined_type(
                *value,
                declaration.value_type,
                function_index,
                values,
                defined,
            )?;
            Ok(())
        }
        super::IrInstr::CallIndirect {
            type_index,
            table,
            operand,
            arguments,
            results,
        } => {
            let table_declaration = module
                .tables
                .get(*table as usize)
                .ok_or(VerificationError::UnknownTable(*table))?;
            if table_declaration.element_type != ReferenceType::FuncRef {
                return Err(VerificationError::NotAFunctionTable(*table));
            }
            let callee_type = module
                .types
                .get(*type_index as usize)
                .ok_or(VerificationError::UnknownType(*type_index))?;
            // The table index is the topmost stack operand, above the arguments.
            expect_defined_type(*operand, ValueType::I32, function_index, values, defined)?;
            if arguments.len() != callee_type.params.0.len() {
                return Err(VerificationError::CallArity {
                    function: *type_index,
                    expected: callee_type.params.0.len(),
                    actual: arguments.len(),
                });
            }
            for (argument, expected) in arguments.iter().zip(&callee_type.params.0) {
                expect_defined_type(*argument, *expected, function_index, values, defined)?;
            }
            if results.len() != callee_type.results.0.len() {
                return Err(VerificationError::CallResultArity {
                    function: *type_index,
                    expected: callee_type.results.0.len(),
                    actual: results.len(),
                });
            }
            for (result, expected) in results.iter().zip(&callee_type.results.0) {
                define_value(*result, *expected, function_index, values, defined)?;
            }
            Ok(())
        }
        super::IrInstr::RefNull {
            result,
            reference_type,
        } => define_value(
            *result,
            ValueType::Ref(*reference_type),
            function_index,
            values,
            defined,
        ),
        super::IrInstr::RefFunc { result, function } => {
            // The index must name a function this module actually defines, the
            // same requirement a direct `call` places on its target.
            let exists = usize::try_from(*function)
                .ok()
                .and_then(|index| module.functions.get(index))
                .is_some();
            if !exists {
                return Err(VerificationError::UnknownFunction(*function));
            }
            define_value(
                *result,
                ValueType::Ref(ReferenceType::FuncRef),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::RefIsNull { result, value } => {
            // Either reference kind is accepted, matching Wasm, so this checks
            // that the operand is a reference rather than which one.
            require_defined(*value, values, defined)?;
            match value_type(*value, values)? {
                ValueType::Ref(_) => {}
                actual => {
                    return Err(VerificationError::TypeMismatch {
                        function: function_index,
                        value: *value,
                        expected: ValueType::Ref(ReferenceType::FuncRef),
                        actual,
                    })
                }
            }
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
    }
}

fn define_value(
    id: ValueId,
    expected: ValueType,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
    defined: &mut HashSet<ValueId>,
) -> Result<(), VerificationError> {
    expect_type(id, expected, function_index, values)?;
    if !defined.insert(id) {
        return Err(VerificationError::RedefinedValue(id));
    }
    Ok(())
}

fn require_defined(
    id: ValueId,
    values: &HashMap<ValueId, ValueType>,
    defined: &HashSet<ValueId>,
) -> Result<(), VerificationError> {
    value_type(id, values)?;
    if defined.contains(&id) {
        Ok(())
    } else {
        Err(VerificationError::UseBeforeDefinition(id))
    }
}

fn expect_defined_type(
    id: ValueId,
    expected: ValueType,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
    defined: &HashSet<ValueId>,
) -> Result<(), VerificationError> {
    require_defined(id, values, defined)?;
    expect_type(id, expected, function_index, values)
}

/// Re-derive a memory access's bounds claim instead of trusting it.
///
/// `Checked` is always accepted: it asserts nothing, and the backend will range
/// check. `Proven` asserts the access lies inside the memory, which is only
/// checkable when the address itself is a known constant, and that is exactly the
/// condition the bounds-check-elimination pass establishes. The comparison is in
/// `u64`, so an address near `u32::MAX` plus a large offset cannot wrap into a
/// small value and slip through: an overflowing sum is reported, not accepted.
///
/// The available size comes from the memory's *declared minimum*, which
/// instantiation guarantees and `memory.grow` can only increase, so proving an
/// access fits the minimum proves it fits forever.
fn verify_proven_bounds(
    bounds: super::BoundsCheck,
    address: ValueId,
    offset: u32,
    width: u64,
    memory: &super::IrMemory,
    function_index: usize,
    constants: &HashMap<ValueId, i32>,
) -> Result<(), VerificationError> {
    let available = memory.min_pages.saturating_mul(PAGE_SIZE);
    // A `Checked` access asserts nothing, so there is nothing to confirm: the
    // backend range-checks it at run time. Only a `Proven` claim is a claim.
    if bounds.needs_check() {
        return Ok(());
    }
    // Only a constant address can be proven. Reasoning about a runtime value is
    // the optimizer's job; the verifier's job is to confirm what the optimizer
    // claimed, and an address it cannot read is a claim it cannot confirm.
    let Some(base) = constants.get(&address).copied() else {
        return Err(VerificationError::UnprovenBounds {
            function: function_index,
            required: u64::from(offset) + width,
            available,
        });
    };
    // The address is an `i32` and the offset a `u32`, so the sum is computed in
    // `u64` and every addition is checked: a wrap here would turn an
    // out-of-bounds access into an apparently tiny one, which is exactly the bug
    // a bounds check exists to prevent.
    let end = u64::from(base as u32)
        .checked_add(u64::from(offset))
        .and_then(|value| value.checked_add(width));
    match end {
        Some(required) if required <= available => Ok(()),
        Some(required) => Err(VerificationError::UnprovenBounds {
            function: function_index,
            required,
            available,
        }),
        None => Err(VerificationError::UnprovenBounds {
            function: function_index,
            required: u64::MAX,
            available,
        }),
    }
}

/// One 64 KiB page, the unit a memory's declared minimum is counted in.
const PAGE_SIZE: u64 = 65_536;
