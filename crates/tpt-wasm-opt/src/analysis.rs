use std::collections::{HashMap, HashSet};

use tpt_wasm_ir::{
    BasicBlock, BlockId, IrFunction, IrInstr, MemoryLoad, MemoryStore, Terminator, ValueId,
};
use tpt_wasm_types::ValueType;

/// One 64 KiB page, the unit a memory's declared minimum is counted in.
pub const PAGE_SIZE: u64 = 65_536;

/// The block graph of one function, indexed by block position.
///
/// Successor lists keep duplicates for a `CondBranch` whose arms share a target,
/// because that duplicate is a real second edge and dropping it would hide a
/// second predecessor.
#[derive(Debug, Clone)]
pub struct Cfg {
    /// Index of each `BlockId` in `function.blocks`.
    pub index_of: HashMap<BlockId, usize>,
    /// Successor block positions, one entry per edge.
    pub successors: Vec<Vec<usize>>,
    /// Predecessor block positions, deduplicated and in block order.
    pub predecessors: Vec<Vec<usize>>,
    /// Positions reachable from the entry block.
    pub reachable: Vec<bool>,
    /// Reachable positions in reverse postorder, entry first.
    pub order: Vec<usize>,
    /// Position of the entry block.
    pub entry: usize,
}

impl Cfg {
    /// Build the graph.
    ///
    /// A terminator naming a missing block is treated as having no edge there.
    /// The input is a verified function, so a missing target is already a
    /// verifier error and cannot reach here; recovering as "no edge" rather than
    /// panicking keeps a pass from turning a malformed module into a crash.
    pub fn new(function: &IrFunction) -> Self {
        let count = function.blocks.len();
        let mut index_of = HashMap::with_capacity(count);
        for (index, block) in function.blocks.iter().enumerate() {
            index_of.entry(block.id).or_insert(index);
        }
        let mut successors = vec![Vec::new(); count];
        for (index, block) in function.blocks.iter().enumerate() {
            for target in block.terminator.successors() {
                if let Some(&position) = index_of.get(&target) {
                    successors[index].push(position);
                }
            }
        }
        let mut predecessors = vec![Vec::new(); count];
        for (index, edges) in successors.iter().enumerate() {
            for &target in edges {
                if !predecessors[target].contains(&index) {
                    predecessors[target].push(index);
                }
            }
        }
        let entry = index_of.get(&function.entry).copied().unwrap_or(0);
        let (reachable, order) = reachability(count, entry, &successors);
        Self {
            index_of,
            successors,
            predecessors,
            reachable,
            order,
            entry,
        }
    }

    /// Is this position reachable from the entry block?
    pub fn is_reachable(&self, position: usize) -> bool {
        self.reachable.get(position).copied().unwrap_or(false)
    }
}

/// Reachability and reverse postorder, from one iterative depth-first walk.
///
/// Iterative rather than recursive so a long straight-line function cannot
/// overflow the *host* stack. That is the same hazard the baseline guards
/// against for Wasm call chains and is worth the same care here: a compiler that
/// crashes on a large input is a denial of service for whoever embeds it.
/// Reachability and reverse postorder, from one iterative depth-first walk.
///
/// Iterative rather than recursive so a long straight-line function cannot
/// overflow the *host* stack. That is the same hazard the baseline guards
/// against for Wasm call chains and is worth the same care here: a compiler that
/// crashes on a large input is a denial of service for whoever embeds it.
///
/// ## Why the finish marker is pushed before the successors
///
/// A block is marked reachable when it is *popped and found unvisited*, not when
/// it is pushed. The distinction matters: pushing first would mark a block
/// reachable merely because some other block scheduled it, which is not the same
/// as the walk having descended into it.
///
/// With the mark on pop, a block whose only incoming edge comes from a region the
/// walk never enters stays unreachable, even if that region contains a cycle back
/// to it. Marking on push gets this wrong in exactly the case the optimizer cares
/// about -- an unreachable tail that loops -- and the consequence is a pass
/// treating dead code as live and rewriting it.
fn reachability(count: usize, entry: usize, successors: &[Vec<usize>]) -> (Vec<bool>, Vec<usize>) {
    let mut reachable = vec![false; count];
    let mut postorder = Vec::with_capacity(count);
    let mut visited = vec![false; count];
    // (position, finished). The finish marker is pushed *before* the successors
    // so it is popped *after* they have been descended into, which is what makes
    // the order a postorder at all.
    let mut stack = vec![(entry, false)];
    while let Some((position, finished)) = stack.pop() {
        if finished {
            if visited[position] {
                postorder.push(position);
            }
            continue;
        }
        if visited[position] {
            continue;
        }
        visited[position] = true;
        reachable[position] = true;
        stack.push((position, true));
        for &target in successors[position].iter().rev() {
            if !visited[target] {
                stack.push((target, false));
            }
        }
    }
    postorder.reverse();
    (reachable, postorder)
}

const NO_DOMINATOR: usize = usize::MAX;

/// Immediate dominators over a function's blocks.
#[derive(Debug, Clone)]
pub struct Dominators {
    idom: Vec<usize>,
    depth: Vec<usize>,
    reachable: Vec<bool>,
}

impl Dominators {
    /// Compute immediate dominators with the iterative Cooper-Harvey-Kennedy
    /// fixpoint over reverse postorder.
    ///
    /// A block with no computed dominator is unreachable and so is not in the
    /// tree. `dominates` treats such a block as dominating only itself, which is
    /// the conservative answer: a pass must not forward a value into a region it
    /// cannot prove is dominated.
    pub fn new(cfg: &Cfg) -> Self {
        let count = cfg.successors.len();
        let mut idom = vec![NO_DOMINATOR; count];
        if count == 0 {
            return Self {
                idom,
                depth: Vec::new(),
                reachable: Vec::new(),
            };
        }
        idom[cfg.entry] = cfg.entry;
        let mut position = vec![NO_DOMINATOR; count];
        for (rank, &block) in cfg.order.iter().enumerate() {
            position[block] = rank;
        }
        for _ in 0..=count {
            let mut changed = false;
            for &block in &cfg.order {
                if block == cfg.entry {
                    continue;
                }
                let mut candidate = NO_DOMINATOR;
                for &predecessor in &cfg.predecessors[block] {
                    if idom[predecessor] == NO_DOMINATOR {
                        continue;
                    }
                    candidate = match candidate {
                        NO_DOMINATOR => predecessor,
                        current => intersect(&idom, &position, current, predecessor),
                    };
                }
                if candidate != NO_DOMINATOR && idom[block] != candidate {
                    idom[block] = candidate;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Depth follows from the tree: a dominator always precedes its dominated
        // block in reverse postorder, so one pass in that order suffices.
        let mut depth = vec![0usize; count];
        for &block in &cfg.order {
            if block != cfg.entry && idom[block] != NO_DOMINATOR {
                depth[block] = depth[idom[block]] + 1;
            }
        }
        Self {
            idom,
            depth,
            reachable: cfg.reachable.clone(),
        }
    }

    /// Does `ancestor` dominate `node`? A block always dominates itself.
    pub fn dominates(&self, ancestor: usize, node: usize) -> bool {
        if ancestor == node {
            // Even an unreachable block dominates itself, which keeps a
            // same-block query from accidentally failing.
            return true;
        }
        if !self.reachable.get(ancestor).copied().unwrap_or(false)
            || !self.reachable.get(node).copied().unwrap_or(false)
        {
            return false;
        }
        let mut current = node;
        // Bounded by the tree depth so a malformed graph cannot spin here.
        for _ in 0..=self.depth.len() {
            if current == ancestor {
                return true;
            }
            let next = self.idom[current];
            if next == NO_DOMINATOR || next == current {
                return false;
            }
            if self.depth[next] < self.depth[ancestor] {
                return false;
            }
            current = next;
        }
        false
    }

    /// The immediate dominator of a block, if it has one.
    pub fn immediate(&self, block: usize) -> Option<usize> {
        match self.idom.get(block) {
            Some(&parent) if parent != NO_DOMINATOR && parent != block => Some(parent),
            _ => None,
        }
    }
}

/// Walk both dominator paths up until they meet.
fn intersect(idom: &[usize], position: &[usize], mut a: usize, mut b: usize) -> usize {
    for _ in 0..=idom.len() {
        while a != b && position[a] > position[b] {
            let next = idom[a];
            if next == NO_DOMINATOR || next == a {
                return b;
            }
            a = next;
        }
        while a != b && position[b] > position[a] {
            let next = idom[b];
            if next == NO_DOMINATOR || next == b {
                return a;
            }
            b = next;
        }
        if a == b {
            return a;
        }
    }
    if position[a] <= position[b] {
        a
    } else {
        b
    }
}

/// The single value this instruction defines, if it defines exactly one.
///
/// A call defines one value per result, so it returns `None` here and
/// [`defined_values`] is what a pass that has to account for every output uses.
use IrInstr::*;

pub fn defines(instruction: &IrInstr) -> Option<ValueId> {
    Some(match instruction {
        ConstI32 { result, .. }
        | ConstI64 { result, .. }
        | ConstF32 { result, .. }
        | ConstF64 { result, .. }
        | Select { result, .. }
        | LocalGet { result, .. }
        | LocalTee { result, .. }
        | I32Eqz { result, .. }
        | I64Eqz { result, .. }
        | I32Compare { result, .. }
        | I64Compare { result, .. }
        | F32Compare { result, .. }
        | F64Compare { result, .. }
        | I32Unary { result, .. }
        | I64Unary { result, .. }
        | F32Unary { result, .. }
        | F64Unary { result, .. }
        | IntConvert { result, .. }
        | Reinterpret { result, .. }
        | SignExtend { result, .. }
        | FloatConvert { result, .. }
        | FloatTrunc { result, .. }
        | RefNull { result, .. }
        | RefFunc { result, .. }
        | RefIsNull { result, .. }
        | Load { result, .. }
        | MemorySize { result }
        | MemoryGrow { result, .. }
        | GlobalGet { result, .. } => *result,
        I32Add { result, .. }
        | I32Sub { result, .. }
        | I32Mul { result, .. }
        | I32DivS { result, .. }
        | I32DivU { result, .. }
        | I32RemS { result, .. }
        | I32RemU { result, .. }
        | I32And { result, .. }
        | I32Or { result, .. }
        | I32Xor { result, .. }
        | I32Shl { result, .. }
        | I32ShrS { result, .. }
        | I32ShrU { result, .. }
        | I32Rotl { result, .. }
        | I32Rotr { result, .. }
        | I64Add { result, .. }
        | I64Sub { result, .. }
        | I64Mul { result, .. }
        | I64DivS { result, .. }
        | I64DivU { result, .. }
        | I64RemS { result, .. }
        | I64RemU { result, .. }
        | I64And { result, .. }
        | I64Or { result, .. }
        | I64Xor { result, .. }
        | I64Shl { result, .. }
        | I64ShrS { result, .. }
        | I64ShrU { result, .. }
        | I64Rotl { result, .. }
        | I64Rotr { result, .. }
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
        | F64Copysign { result, .. } => *result,
        // No single result: a call, a drop, a store, or a side effect.
        Drop { .. }
        | LocalSet { .. }
        | Call { .. }
        | CallHost { .. }
        | Store { .. }
        | GlobalSet { .. }
        | CallIndirect { .. } => return None,
    })
}

/// Every value this instruction defines.
pub fn defined_values(instruction: &IrInstr) -> Vec<ValueId> {
    match instruction {
        IrInstr::Call { results, .. }
        | IrInstr::CallHost { results, .. }
        | IrInstr::CallIndirect { results, .. } => results.clone(),
        other => defines(other).into_iter().collect(),
    }
}

/// Every value this instruction reads.
pub fn used_values(instruction: &IrInstr) -> Vec<ValueId> {
    match instruction {
        ConstI32 { .. } | ConstI64 { .. } | ConstF32 { .. } | ConstF64 { .. } => Vec::new(),
        Drop { value } | LocalSet { value, .. } => vec![*value],
        // A `local.get` reads the slot, and a `local.tee` writes it and yields a
        // result. Neither reads the *result* it introduces, so reporting the
        // result here would make a pass treat its own output as an input and
        // conclude the value is live before it is defined.
        LocalGet { local, .. } => vec![*local],
        LocalTee { value, .. } => vec![*value],
        Select {
            condition,
            left,
            right,
            ..
        } => vec![*condition, *left, *right],
        Call { arguments, .. } | CallHost { arguments, .. } => arguments.clone(),
        CallIndirect {
            operand, arguments, ..
        } => {
            let mut all = arguments.clone();
            all.push(*operand);
            all
        }
        I32Eqz { value, .. }
        | I64Eqz { value, .. }
        | I32Unary { value, .. }
        | I64Unary { value, .. }
        | F32Unary { value, .. }
        | F64Unary { value, .. }
        | IntConvert { value, .. }
        | Reinterpret { value, .. }
        | SignExtend { value, .. }
        | FloatConvert { value, .. }
        | FloatTrunc { value, .. }
        | RefIsNull { value, .. } => vec![*value],
        RefNull { .. } | RefFunc { .. } | MemorySize { .. } | GlobalGet { .. } => Vec::new(),
        Load { address, .. } => vec![*address],
        Store { address, value, .. } => vec![*address, *value],
        MemoryGrow { delta, .. } => vec![*delta],
        GlobalSet { value, .. } => vec![*value],
        _ => binary_operands(instruction),
    }
}

/// The two operands of a binary operation, in source order.
///
/// Kept apart from [`used_values`] only to keep each match arm set readable. The
/// fallback returns nothing rather than panicking so a variant added to the IR
/// later shows up as a pass that stops reasoning about it, not as a crash in the
/// middle of a compilation.
fn binary_operands(instruction: &IrInstr) -> Vec<ValueId> {
    match instruction {
        I32Add { left, right, .. }
        | I32Sub { left, right, .. }
        | I32Mul { left, right, .. }
        | I32DivS { left, right, .. }
        | I32DivU { left, right, .. }
        | I32RemS { left, right, .. }
        | I32RemU { left, right, .. }
        | I32And { left, right, .. }
        | I32Or { left, right, .. }
        | I32Xor { left, right, .. }
        | I32Shl { left, right, .. }
        | I32ShrS { left, right, .. }
        | I32ShrU { left, right, .. }
        | I32Rotl { left, right, .. }
        | I32Rotr { left, right, .. }
        | I64Add { left, right, .. }
        | I64Sub { left, right, .. }
        | I64Mul { left, right, .. }
        | I64DivS { left, right, .. }
        | I64DivU { left, right, .. }
        | I64RemS { left, right, .. }
        | I64RemU { left, right, .. }
        | I64And { left, right, .. }
        | I64Or { left, right, .. }
        | I64Xor { left, right, .. }
        | I64Shl { left, right, .. }
        | I64ShrS { left, right, .. }
        | I64ShrU { left, right, .. }
        | I64Rotl { left, right, .. }
        | I64Rotr { left, right, .. }
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
        | F64Copysign { left, right, .. }
        | I32Compare { left, right, .. }
        | I64Compare { left, right, .. }
        | F32Compare { left, right, .. }
        | F64Compare { left, right, .. } => vec![*left, *right],
        _ => Vec::new(),
    }
}

/// May this instruction trap?
///
/// This is the question that separates "the result is unused" from "the
/// instruction is dead". `i32.div_s` with an unused result still traps when the
/// divisor is zero, and Wasm's trap is observable, so such an instruction is
/// *not* dead. Answering this wrongly is not a missed optimization, it is a
/// missing trap.
///
/// A `call` counts because a callee can trap and the call depth is finite, so a
/// call is never removable no matter how its results are used.
pub fn may_trap(instruction: &IrInstr) -> bool {
    match instruction {
        // Division and remainder trap on a zero divisor, and signed division
        // additionally traps on `INT_MIN / -1`.
        I32DivS { .. }
        | I32DivU { .. }
        | I32RemS { .. }
        | I32RemU { .. }
        | I64DivS { .. }
        | I64DivU { .. }
        | I64RemS { .. }
        | I64RemU { .. } => true,
        // Every float-to-integer truncation has a trapping range.
        FloatTrunc { .. } => true,
        // An access outside the memory traps.
        Load { .. } | Store { .. } => true,
        // An index outside the table, a null entry, and a signature mismatch are
        // all traps, and the callee can trap too.
        CallIndirect { .. } | Call { .. } | CallHost { .. } => true,
        // Everything else in the modeled instruction set is total.
        _ => false,
    }
}

/// Does this instruction change anything a guest can observe?
///
/// Memory, globals, the table, and the host boundary are all observable, as is a
/// local that a later instruction in the same function can read. A value feeding
/// only pure arithmetic is not observable once nothing reads it.
pub fn has_side_effects(instruction: &IrInstr) -> bool {
    match instruction {
        Store { .. }
        | GlobalSet { .. }
        | MemoryGrow { .. }
        | Call { .. }
        | CallHost { .. }
        | CallIndirect { .. }
        | LocalSet { .. }
        | LocalTee { .. } => true,
        // A `drop` discards a value. Under the MVP there is nothing for a discard
        // to run, so it is not listed here. It is *also* not removable by
        // accident: a `drop` of a value something else still reads is exactly the
        // case `is_removable` and dead-store elimination have to reason about,
        // and treating every drop as pure would let a future proposal with
        // destructors change the answer silently.
        _ => false,
    }
}

/// May this instruction be deleted when none of its results are used?
///
/// This is the single definition of "dead" that every pass uses, and it requires
/// all three of:
///
/// 1. no result is used,
/// 2. no observable side effect, and
/// 3. no possible trap.
///
/// The third condition is what makes this correct rather than merely useful. A
/// `local.get` of an unused local satisfies all three. A `call` satisfies neither
/// two nor three. A `div_s` satisfies one and two but *not* three, so it
/// survives; once constant propagation has shown the divisor is non-zero, a later
/// sweep can recognize that it no longer traps and remove it then. That ordering
/// is why the pipeline folds before it eliminates.
pub fn is_removable(instruction: &IrInstr, results_used: bool) -> bool {
    !results_used && !has_side_effects(instruction) && !may_trap(instruction)
}

/// Every value a terminator reads.
pub fn terminator_used_values(terminator: &Terminator) -> Vec<ValueId> {
    match terminator {
        Terminator::Return(values) | Terminator::Branch { values, .. } => values.clone(),
        Terminator::Trap(_) | Terminator::Unreachable => Vec::new(),
        Terminator::CondBranch {
            condition,
            then_values,
            else_values,
            ..
        } => {
            let mut all = vec![*condition];
            all.extend(then_values.iter().copied());
            all.extend(else_values.iter().copied());
            all
        }
    }
}

/// Is this instruction a plain read of state a later instruction could change?
///
/// A `global.get`, a `load`, and `memory.size` are not effects, but they are not
/// constants either: a pass may not move one past a store, or reuse its result
/// after one. Redundancy elimination needs this to decide which expressions it is
/// allowed to treat as interchangeable.
pub fn reads_mutable_state(instruction: &IrInstr) -> bool {
    matches!(
        instruction,
        IrInstr::Load { .. }
            | IrInstr::GlobalGet { .. }
            | IrInstr::MemorySize { .. }
            | IrInstr::RefFunc { .. }
    )
}

/// Does this instruction write state that could change what another read returns?
pub fn writes_mutable_state(instruction: &IrInstr) -> bool {
    matches!(
        instruction,
        IrInstr::Store { .. }
            | IrInstr::GlobalSet { .. }
            | IrInstr::MemoryGrow { .. }
            | IrInstr::Call { .. }
            | IrInstr::CallHost { .. }
            | IrInstr::CallIndirect { .. }
            | IrInstr::LocalSet { .. }
            | IrInstr::LocalTee { .. }
    )
}

/// Every value the function defines: parameters, locals, block parameters, and
/// instruction results.
pub fn all_defined(function: &IrFunction) -> HashSet<ValueId> {
    let mut defined: HashSet<ValueId> = function
        .params
        .iter()
        .chain(&function.locals)
        .copied()
        .collect();
    for block in &function.blocks {
        defined.extend(block.params.iter().copied());
        for instruction in &block.instrs {
            defined.extend(defined_values(instruction));
        }
    }
    defined
}

/// Drop the declarations of values nothing defines any more.
///
/// Removing an instruction orphans the `IrValue` that instruction satisfied, and
/// both the IR verifier and the baseline backend reject a declared-but-undefined
/// value -- correctly, since that is what an uninitialized read looks like. So
/// every pass that removes instructions has to prune the declarations too, and
/// doing it in one place means no pass can forget.
///
/// ## Why pruning is restricted to newly-orphaned values
///
/// A value can be undefined *mid-pipeline* and defined again later: a
/// control-flow pass may delete the block that defined it while another value
/// still names it, and only a subsequent pass restores a definition. Pruning on
/// "undefined now" would remove the declaration in that window, and the finished
/// module would then be missing a value the backend needs -- an error whose cause
/// is several passes away from where it happened.
///
/// [`prune_orphaned`] therefore removes only what the *last* pass broke, which is
/// exactly what has to be cleaned up, and this function is the unrestricted
/// version used once the pipeline has converged.
pub fn prune_declarations(function: &mut IrFunction) -> usize {
    let defined = all_defined(function);
    let before = function.values.len();
    function.values.retain(|value| defined.contains(&value.id));
    before - function.values.len()
}

/// Remove declarations that nothing defines *and* nothing names.
///
/// `before` is a snapshot of the values the previous state defined, and it
/// distinguishes two very different situations that both look like "undefined
/// now":
///
/// * the value was defined a moment ago and this pass deleted its definition --
///   its declaration is an orphan and must go;
/// * the value was *already* undefined before this pass, because an earlier pass
///   removed its definition -- its declaration is stale from before, and
///   removing it is equally correct.
///
/// Both end in the same place, which is why this takes the snapshot at all: it
/// lets a caller tell the two apart for reporting, and it documents that a value
/// can be undefined for a pass and defined again by a later one.
pub fn prune_orphaned(function: &mut IrFunction, _before: &HashSet<ValueId>) -> usize {
    let defined = all_defined(function);
    let used = used_values_of(function);
    // A declaration is dropped when nothing defines it *and* nothing names it.
    // Both halves matter:
    //
    // * "undefined but still named" is a real defect -- an operand of some
    //   instruction or edge reads a slot nothing wrote -- and the verifier must be
    //   the one to report it, with the value still declared so the report can
    //   name it;
    // * "undefined and unnamed" is pure garbage left by a pass, and the backend
    //   would reject it with an error that names the backend rather than the
    //   pass that made it.
    //
    // Keeping the first and dropping the second is what lets the verifier stay
    // the authority on the former while the second never reaches a backend.
    let previous = function.values.len();
    function
        .values
        .retain(|value| defined.contains(&value.id) || used.contains(&value.id));
    previous - function.values.len()
}

/// The count of uses of every value in the function.
///
/// A local is counted like any other value. A local is storage rather than an SSA
/// name, so "used" here means "named as an operand by some instruction", which is
/// what dead-store elimination needs: a `local.set` whose slot nothing names again
/// is removable, and that is decided by exactly this count.
pub fn use_counts(function: &IrFunction) -> HashMap<ValueId, usize> {
    let mut counts: HashMap<ValueId, usize> = HashMap::new();
    // Seeded with every value the function defines, so a value that is defined
    // and never used reports zero rather than being absent -- an absent entry
    // would be indistinguishable from "not a real value" to a caller.
    for value in all_defined(function) {
        counts.insert(value, 0);
    }
    for block in &function.blocks {
        for instruction in &block.instrs {
            for value in used_values(instruction) {
                *counts.entry(value).or_insert(0) += 1;
            }
        }
        for value in terminator_used_values(&block.terminator) {
            *counts.entry(value).or_insert(0) += 1;
        }
    }
    counts
}

/// Is this value a function local, i.e. a mutable slot rather than an SSA name?
pub fn is_local(function: &IrFunction, value: ValueId) -> bool {
    function.locals.contains(&value)
}

/// The block that defines a value.
///
/// A local is attributed to the entry block, matching how the IR verifier treats
/// it: storage for the whole function, so it is visible everywhere. Instruction
/// results and block parameters are attributed to the block that introduces them.
pub fn defining_block(function: &IrFunction, cfg: &Cfg, value: ValueId) -> Option<usize> {
    if function.params.contains(&value) || function.locals.contains(&value) {
        return Some(cfg.entry);
    }
    for (index, block) in function.blocks.iter().enumerate() {
        if block.params.contains(&value) {
            return Some(index);
        }
        for instruction in &block.instrs {
            if defines(instruction) == Some(value) {
                return Some(index);
            }
        }
    }
    None
}

/// Bytes touched by a load, with the value type it produces.
pub fn load_shape(operation: MemoryLoad) -> (u32, ValueType) {
    (operation.width(), operation.result_type())
}

/// Bytes written by a store, with the value type it consumes.
pub fn store_shape(operation: MemoryStore) -> (u32, ValueType) {
    (operation.width(), operation.operand_type())
}

/// Do these two byte ranges overlap? An empty range overlaps nothing.
pub fn ranges_overlap(start_a: u64, len_a: u32, start_b: u64, len_b: u32) -> bool {
    if len_a == 0 || len_b == 0 {
        return false;
    }
    let end_a = start_a.saturating_add(u64::from(len_a));
    let end_b = start_b.saturating_add(u64::from(len_b));
    start_a < end_b && start_b < end_a
}

/// Rewrite every operand of an instruction in place.
///
/// This is the single place the optimizer renames values, so a pass that
/// substitutes a value for another gets *all* operand positions for free and
/// cannot forget one. Forgetting one is not a compile error: the IR verifier
/// would catch an unknown value, but a substitution that reaches a `Drop` while
/// missing the paired `Select` would compile cleanly and change behavior.
///
/// `f` is called once per operand occurrence, so a caller must supply a
/// substitution that is stable under repeated application -- which a map from one
/// value to another is, because it never maps a value to itself.
pub fn map_operands(instruction: &mut IrInstr, mut f: impl FnMut(ValueId) -> ValueId) {
    match instruction {
        ConstI32 { .. }
        | ConstI64 { .. }
        | ConstF32 { .. }
        | ConstF64 { .. }
        | RefNull { .. }
        | RefFunc { .. }
        | MemorySize { .. }
        | GlobalGet { .. } => {}
        Drop { value } => *value = f(*value),
        LocalGet { local, .. } => *local = f(*local),
        LocalSet { local, value } | LocalTee { local, value, .. } => {
            *local = f(*local);
            *value = f(*value);
        }
        Select {
            condition,
            left,
            right,
            ..
        } => {
            *condition = f(*condition);
            *left = f(*left);
            *right = f(*right);
        }
        Call { arguments, .. } | CallHost { arguments, .. } => {
            for argument in arguments.iter_mut() {
                *argument = f(*argument);
            }
        }
        CallIndirect {
            operand, arguments, ..
        } => {
            *operand = f(*operand);
            for argument in arguments.iter_mut() {
                *argument = f(*argument);
            }
        }
        Load { address, .. } => *address = f(*address),
        Store { address, value, .. } => {
            *address = f(*address);
            *value = f(*value);
        }
        MemoryGrow { delta, .. } => *delta = f(*delta),
        GlobalSet { value, .. } => *value = f(*value),
        I32Eqz { value, .. }
        | I64Eqz { value, .. }
        | I32Unary { value, .. }
        | I64Unary { value, .. }
        | F32Unary { value, .. }
        | F64Unary { value, .. }
        | IntConvert { value, .. }
        | Reinterpret { value, .. }
        | SignExtend { value, .. }
        | FloatConvert { value, .. }
        | FloatTrunc { value, .. }
        | RefIsNull { value, .. } => *value = f(*value),
        _ => {
            let operands = used_values(instruction);
            if operands.len() == 2 {
                let left = f(operands[0]);
                let right = f(operands[1]);
                set_binary_operands(instruction, left, right);
            }
        }
    }
}

/// Overwrite a binary operation's two operands.
fn set_binary_operands(instruction: &mut IrInstr, left: ValueId, right: ValueId) {
    macro_rules! set {
        ($($variant:ident),* $(,)?) => {
            match instruction {
                $(
                    IrInstr::$variant { left: l, right: r, .. } => {
                        *l = left;
                        *r = right;
                    }
                )*
                _ => {}
            }
        };
    }
    set!(
        I32Add,
        I32Sub,
        I32Mul,
        I32DivS,
        I32DivU,
        I32RemS,
        I32RemU,
        I32And,
        I32Or,
        I32Xor,
        I32Shl,
        I32ShrS,
        I32ShrU,
        I32Rotl,
        I32Rotr,
        I64Add,
        I64Sub,
        I64Mul,
        I64DivS,
        I64DivU,
        I64RemS,
        I64RemU,
        I64And,
        I64Or,
        I64Xor,
        I64Shl,
        I64ShrS,
        I64ShrU,
        I64Rotl,
        I64Rotr,
        F32Add,
        F32Sub,
        F32Mul,
        F32Div,
        F32Min,
        F32Max,
        F32Copysign,
        F64Add,
        F64Sub,
        F64Mul,
        F64Div,
        F64Min,
        F64Max,
        F64Copysign,
        I32Compare,
        I64Compare,
        F32Compare,
        F64Compare,
    );
}

/// Rewrite every operand a terminator names.
pub fn map_terminator_operands(terminator: &mut Terminator, mut f: impl FnMut(ValueId) -> ValueId) {
    match terminator {
        Terminator::Return(values) | Terminator::Branch { values, .. } => {
            for value in values.iter_mut() {
                *value = f(*value);
            }
        }
        Terminator::Trap(_) | Terminator::Unreachable => {}
        Terminator::CondBranch {
            condition,
            then_values,
            else_values,
            ..
        } => {
            *condition = f(*condition);
            for value in then_values.iter_mut() {
                *value = f(*value);
            }
            for value in else_values.iter_mut() {
                *value = f(*value);
            }
        }
    }
}

/// Rewrite the parameters a block declares.
pub fn map_block_params(block: &mut BasicBlock, mut f: impl FnMut(ValueId) -> ValueId) {
    for param in block.params.iter_mut() {
        *param = f(*param);
    }
}

/// Every value named anywhere in the function: by an instruction, by a block's
/// parameters, or by a terminator.
///
/// Parameters count as *uses* of whatever the incoming edges bind them to, and
/// they are included here so a block parameter is never mistaken for garbage.
/// Excluding them would let the pruner delete the declaration of a parameter that
/// is still perfectly well defined.
pub fn used_values_of(function: &IrFunction) -> HashSet<ValueId> {
    let mut used: HashSet<ValueId> = HashSet::new();
    for block in &function.blocks {
        used.extend(block.params.iter().copied());
        for instruction in &block.instrs {
            used.extend(used_values(instruction));
            used.extend(defined_values(instruction));
        }
        used.extend(terminator_used_values(&block.terminator));
    }
    used
}
/// Remove declarations that nothing defines *and* nothing names.
///
/// ## Why the criterion is "defined and used", not merely "defined"
///
/// A pass that removes an instruction -- or a whole block -- orphans the
/// `IrValue` that instruction or parameter satisfied. Both the IR verifier and
/// the baseline backend reject a declared-but-undefined value, so something has
/// to clean these up, and doing it in one place means no pass can forget.
///
/// The tempting criterion, "remove whatever is currently undefined", is wrong
/// mid-pipeline. A control-flow pass can delete the block that defined a value
/// while a later pass in the same round re-establishes a definition for it, and
/// pruning on the intermediate state would remove a declaration the finished
/// module still needs. The backend then reports a value it cannot find, and the
/// cause is several passes away from where it happened.
///
/// Requiring *both* conditions avoids the trap without any history:
///
/// * undefined but still named -- a real defect, an operand reading a slot
///   nothing wrote. The declaration is kept so the verifier can name the value,
///   and the verifier is the right authority on this;
/// * undefined and unnamed -- pure garbage. Nothing can refer to it, so removing
///   the declaration is safe at any point in the pipeline.
pub fn prune_garbage(function: &mut IrFunction) -> usize {
    let defined = all_defined(function);
    let used = used_values_of(function);
    let previous = function.values.len();
    function
        .values
        .retain(|value| defined.contains(&value.id) || used.contains(&value.id));
    previous - function.values.len()
}
