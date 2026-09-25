// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Validated WebAssembly-to-TPT-IR lowering.

use std::fmt;

use tpt_wasm_format::Function;
use tpt_wasm_types::{FunctionType, ValueType};
use tpt_wasm_validate::ValidatedModule;

use super::{
    BasicBlock, BlockId, FloatComparison, FloatConversion, FloatTrunc, FloatUnary, IntComparison,
    IntConversion, IntUnary, IrFunction, IrInstr, IrModule, IrValue, Reinterpret, Terminator,
    ValueId,
};

/// Errors produced while lowering a validated module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringError {
    UnsupportedFeature(&'static str),
    UnsupportedInstruction(u8),
    UnknownType(u32),
    UnknownFunction(u32),
    LocalOverflow,
    UnknownLocal(u32),
    InvalidOpcode(u8),
    UnexpectedEnd,
    TrailingBytes,
    InvalidLeb128,
    StackUnderflow,
    TypeMismatch {
        expected: ValueType,
        actual: ValueType,
    },
    UnexpectedValues,
    /// `else` appeared without a matching `if`.
    ElseWithoutIf,
    /// A branch or block type named a label or type that does not exist.
    UnknownLabel(u32),
    /// A `br_table` whose entries disagree on label arity.
    InconsistentBranchArity,
    /// A block type index or signature this lowering pass does not represent.
    UnsupportedBlockType(i64),
}

impl fmt::Display for LoweringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LoweringError {}

/// Lower a validated module into the currently supported IR subset.
///
/// Unsupported module state and instruction encodings are rejected explicitly;
/// this function never silently discards observable behavior.
pub fn lower_module(validated: &ValidatedModule) -> Result<IrModule, LoweringError> {
    let module = &validated.module;
    reject_unsupported_module_state(module)?;

    let mut functions = Vec::with_capacity(module.functions.len());
    for function in &module.functions {
        let function_type = module
            .types
            .get(function.type_index as usize)
            .ok_or(LoweringError::UnknownType(function.type_index))?;
        functions.push(lower_function(function, function_type, module)?);
    }
    Ok(IrModule { functions })
}

fn reject_unsupported_module_state(module: &tpt_wasm_format::Module) -> Result<(), LoweringError> {
    if !module.imports.is_empty() {
        return Err(LoweringError::UnsupportedFeature("imports"));
    }
    if !module.tables.is_empty() {
        return Err(LoweringError::UnsupportedFeature("tables"));
    }
    if !module.memories.is_empty() {
        return Err(LoweringError::UnsupportedFeature("memories"));
    }
    if !module.globals.is_empty() {
        return Err(LoweringError::UnsupportedFeature("globals"));
    }
    if !module.elements.is_empty() {
        return Err(LoweringError::UnsupportedFeature("element segments"));
    }
    if !module.data.is_empty() {
        return Err(LoweringError::UnsupportedFeature("data segments"));
    }
    if module.start.is_some() {
        return Err(LoweringError::UnsupportedFeature("start functions"));
    }
    if !module.exports.is_empty() {
        return Err(LoweringError::UnsupportedFeature("exports"));
    }
    Ok(())
}

/// The kind of structured construct a control frame was pushed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlKind {
    Block,
    Loop,
    If,
}

/// One entry of the structured-control stack tracked while lowering a body.
struct ControlFrame {
    kind: ControlKind,
    /// Types this label carries when a branch targets it.
    label_types: Vec<ValueType>,
    /// Value-stack height on entry, restored when the label is left normally.
    stack_height: usize,
    /// For `if`, the block that begins the `else` arm.
    else_block: Option<BlockId>,
    /// True once a `br`, `return`, or `unreachable` ended the current position.
    unreachable: bool,
    /// Block a branch to this label targets: a loop's header, or the merge block
    /// created when the label is closed.
    target: BlockId,
}

/// Accumulates the basic blocks of one function body.
struct BodyBuilder {
    blocks: Vec<BasicBlock>,
    /// Index of the block currently being filled.
    current: usize,
    /// Whether the current block already has a real terminator. This is tracked
    /// explicitly because `Unreachable` is a genuine terminator and so cannot
    /// also serve as the "not yet set" marker.
    terminated: bool,
    /// Blocks that fall through into a successor and need it patched in.
    fallthroughs: Vec<usize>,
}

impl BodyBuilder {
    fn new() -> Self {
        Self {
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instrs: Vec::new(),
                // Replaced before the function is returned.
                terminator: Terminator::Unreachable,
            }],
            current: 0,
            terminated: false,
            fallthroughs: Vec::new(),
        }
    }

    fn allocate_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            instrs: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    /// Begin filling `id`, recording that the current block falls into it.
    fn start_block(&mut self, id: BlockId) {
        if !self.terminated {
            self.fallthroughs.push(self.current);
        }
        self.current = id.0 as usize;
        self.terminated = false;
    }

    /// Whether the current block already ends in a real terminator.
    fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// Whether a specific block already ends in a real terminator.
    fn is_terminated_at(&self, id: BlockId) -> bool {
        !matches!(
            self.blocks[id.0 as usize].terminator,
            Terminator::Unreachable
        )
    }

    /// Set the terminator of a block other than the current one.
    fn terminate_at(&mut self, id: BlockId, terminator: Terminator) {
        let index = id.0 as usize;
        self.blocks[index].terminator = terminator;
        self.fallthroughs.retain(|pending| *pending != index);
    }

    fn push(&mut self, instruction: IrInstr) {
        self.blocks[self.current].instrs.push(instruction);
    }

    /// Replace the current terminator, resolving any pending fall-through.
    fn terminate(&mut self, terminator: Terminator) {
        let index = self.current;
        self.blocks[index].terminator = terminator;
        self.terminated = true;
        // A block that falls through is satisfied by the terminator just set.
        self.fallthroughs.retain(|pending| *pending != index);
    }
}

/// Mark the innermost label as unreachable so later instructions in the same
/// position are treated as dead code.
fn mark_unreachable(controls: &mut [ControlFrame]) {
    if let Some(frame) = controls.last_mut() {
        frame.unreachable = true;
    }
}

/// Read a block type, supporting only the empty and single-value forms that the
/// MVP allows. Type indices need the multi-value representation and are
/// rejected rather than approximated.
pub(crate) fn read_block_type(
    reader: &mut BodyReader<'_>,
) -> Result<Vec<ValueType>, LoweringError> {
    let byte = reader.byte()?;
    match byte {
        0x40 => Ok(Vec::new()),
        0x7f => Ok(vec![ValueType::I32]),
        0x7e => Ok(vec![ValueType::I64]),
        0x7d => Ok(vec![ValueType::F32]),
        0x7c => Ok(vec![ValueType::F64]),
        // A non-negative s33 is a type index; anything else is malformed.
        other => Err(LoweringError::UnsupportedBlockType(i64::from(other))),
    }
}

/// Push a label for `block`, `loop`, or `if` and start its body block.
fn open_label(
    opcode: u8,
    reader: &mut BodyReader<'_>,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
    controls: &mut Vec<ControlFrame>,
) -> Result<(), LoweringError> {
    let block_type = read_block_type(reader)?;
    let kind = match opcode {
        0x02 => ControlKind::Block,
        0x03 => ControlKind::Loop,
        _ => ControlKind::If,
    };
    // A loop label carries the block's *parameters*, because a branch to a loop
    // re-enters it. A block or `if` label carries its *results*, which are
    // produced on the way out and delivered to the merge block's parameters.
    let label_types = block_type.clone();
    let stack_height = state.stack.len();

    // The condition is consumed first and becomes the two-way terminator that
    // the current block ends with.
    let condition = if kind == ControlKind::If {
        Some(state.pop(ValueType::I32)?)
    } else {
        None
    };

    // A loop re-enters its own header, so the header is also where the body
    // starts. The others get a separate merge block. Both are allocated now so
    // a branch can name them before either is filled.
    let header = builder.allocate_block();
    let merge = if kind == ControlKind::Loop {
        header
    } else {
        builder.allocate_block()
    };
    // An `if` needs a third block for the `else` arm, because the condition's
    // false edge must land in a block the arm actually fills. With no `else`,
    // that block is a bare jump to the merge.
    let else_block = (kind == ControlKind::If).then(|| builder.allocate_block());

    // The merge block receives the label's results as block parameters, so each
    // arm supplies them on its outgoing edge. A loop's header takes the label
    // values the same way.
    let receiving = if kind == ControlKind::Loop {
        header
    } else {
        merge
    };
    let mut params = Vec::with_capacity(label_types.len());
    for value_type in &label_types {
        params.push(state.allocate(*value_type)?);
    }
    let index = receiving.0 as usize;
    builder.blocks[index].params = params;

    if let Some(condition) = condition {
        // The false edge goes to the `else` block, which supplies no values
        // because an `if` itself takes none.
        builder.terminate(Terminator::CondBranch {
            condition,
            then_target: header,
            then_values: Vec::new(),
            else_target: else_block.expect("an if always allocates an else block"),
            else_values: Vec::new(),
        });
    }
    // A `block` or `loop` falls into its body, so the current block needs an
    // explicit fall-through edge rather than being left unterminated.
    if condition.is_none() && !builder.is_terminated() {
        builder.terminate(Terminator::Branch {
            target: header,
            values: Vec::new(),
        });
    }
    builder.start_block(header);

    controls.push(ControlFrame {
        kind,
        label_types,
        stack_height,
        else_block,
        unreachable: false,
        target: receiving,
    });
    Ok(())
}

/// Resolve a label depth to its frame. Depth 0 is the innermost label.
fn label_frame(controls: &[ControlFrame], depth: u32) -> Result<&ControlFrame, LoweringError> {
    let index = controls
        .len()
        .checked_sub(1 + depth as usize)
        .ok_or(LoweringError::UnknownLabel(depth))?;
    controls
        .get(index)
        .ok_or(LoweringError::UnknownLabel(depth))
}

/// Pop the values a branch to `frame` must supply, in label order.
fn pop_label_values(
    state: &mut LoweringState,
    frame: &ControlFrame,
) -> Result<Vec<ValueId>, LoweringError> {
    let mut values = Vec::with_capacity(frame.label_types.len());
    for expected in frame.label_types.iter().rev() {
        values.push(state.pop(*expected)?);
    }
    values.reverse();
    Ok(values)
}

/// Lower `br` and `br_if`.
fn lower_branch(
    opcode: u8,
    reader: &mut BodyReader<'_>,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
    controls: &mut [ControlFrame],
) -> Result<(), LoweringError> {
    if controls.last().map(|f| f.unreachable).unwrap_or(false) {
        // A branch in dead code is still decoded but produces no terminator.
        if opcode == 0x0e {
            let count = reader.u32()?;
            for _ in 0..=count {
                reader.u32()?;
            }
        } else {
            reader.u32()?;
        }
        return Ok(());
    }
    let depth = reader.u32()?;
    let frame = label_frame(controls, depth)?;
    let target = frame.target;
    match opcode {
        0x0c => {
            let values = pop_label_values(state, frame)?;
            builder.terminate(Terminator::Branch { target, values });
            mark_unreachable(controls);
        }
        _ => {
            let condition = state.pop(ValueType::I32)?;
            let values = pop_label_values(state, frame)?;
            // The fall-through path continues in a fresh block.
            let fallthrough = builder.allocate_block();
            builder.terminate(Terminator::CondBranch {
                condition,
                then_target: target,
                then_values: values,
                else_target: fallthrough,
                else_values: Vec::new(),
            });
            builder.start_block(fallthrough);
        }
    }
    Ok(())
}

/// Lower `br_table` as a chain of comparisons against the selector.
///
/// A dedicated switch terminator is deliberately not introduced: the existing
/// two-way conditional already expresses this, and a chain keeps the IR small
/// while preserving Wasm's out-of-range default behavior.
fn lower_branch_table(
    reader: &mut BodyReader<'_>,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
    controls: &mut [ControlFrame],
) -> Result<(), LoweringError> {
    // `br_table` encodes a count, that many labels, and then a default label.
    // The count does not include the default, so one more label is read.
    let count = reader.u32()?;
    let mut labels = Vec::with_capacity(count as usize + 1);
    for _ in 0..count {
        labels.push(reader.u32()?);
    }
    let default = reader.u32()?;
    labels.push(default);
    let arity = label_frame(controls, labels[0])?.label_types.len();
    let mut label_types = Vec::with_capacity(arity);
    for label in &labels {
        let frame = label_frame(controls, *label)?;
        if frame.label_types.len() != arity {
            return Err(LoweringError::InconsistentBranchArity);
        }
        label_types = frame.label_types.clone();
    }
    let values = pop_label_values(
        state,
        &ControlFrame {
            kind: ControlKind::Block,
            label_types: label_types.clone(),
            stack_height: 0,
            else_block: None,
            unreachable: false,
            target: BlockId(0),
        },
    )?;
    let selector = state.pop(ValueType::I32)?;

    // The comparison chain is a sequence of blocks, and the selector is only
    // defined in the first of them. A later block is not dominated by that
    // definition, because a branch may have jumped straight past it, so the
    // selector is stored in a synthesized local and re-read in each block.
    let selector_local = state.allocate_temp_local(ValueType::I32)?;
    builder.push(IrInstr::LocalSet {
        local: selector_local,
        value: selector,
    });
    state.push(selector, ValueType::I32);

    // Compare the selector against each label index in turn. Every index except
    // the last gets a two-way branch: a match goes to that label, and a miss
    // continues the chain. The last index is the default, reached by falling
    // off the end of the chain, so it gets a plain branch.
    for index in 0..labels.len() {
        let default = index + 1 == labels.len();
        // The default arm never falls through, so it needs no extra block.
        let fallthrough = (!default).then(|| builder.allocate_block());
        let constant = state.allocate(ValueType::I32)?;
        builder.push(IrInstr::ConstI32 {
            result: constant,
            value: index as i32,
        });
        let current = state.allocate(ValueType::I32)?;
        builder.push(IrInstr::LocalGet {
            result: current,
            local: selector_local,
        });
        let matched = state.allocate(ValueType::I32)?;
        builder.push(IrInstr::I32Compare {
            result: matched,
            left: current,
            right: constant,
            comparison: IntComparison::Eq,
        });
        let target = label_frame(controls, labels[index])?.target;
        let terminator = match fallthrough {
            // The default arm is a plain branch: anything reaching here either
            // matched no index or the selector was out of range.
            None => Terminator::Branch {
                target,
                values: values.clone(),
            },
            Some(fallthrough) => Terminator::CondBranch {
                condition: matched,
                then_target: target,
                then_values: values.clone(),
                else_target: fallthrough,
                else_values: Vec::new(),
            },
        };
        builder.terminate(terminator);
        if let Some(fallthrough) = fallthrough {
            builder.start_block(fallthrough);
        }
    }
    mark_unreachable(controls);
    Ok(())
}

/// Close the innermost label, merging the values it carries into a new block.
fn close_label(
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
    controls: &mut Vec<ControlFrame>,
    is_else: bool,
) -> Result<(), LoweringError> {
    let frame = controls.pop().ok_or(LoweringError::UnexpectedEnd)?;
    if is_else && frame.kind != ControlKind::If {
        return Err(LoweringError::ElseWithoutIf);
    }
    // Both arms of an `if` converge on one merge block, which is the label's
    // target and already carries the label's results as block parameters.
    let merge = frame.target;
    let else_block = frame.else_block;
    if is_else {
        // The `then` arm is complete. End it by branching to the merge and
        // resume in the reserved `else` block. The frame is pushed back so the
        // matching `end` closes the `if` and routes the arm to the merge.
        if !builder.is_terminated() {
            let mut values = Vec::with_capacity(frame.label_types.len());
            for expected in frame.label_types.iter().rev() {
                values.push(state.pop(*expected)?);
            }
            values.reverse();
            builder.terminate(Terminator::Branch {
                target: merge,
                values,
            });
        }
        let else_block = else_block.ok_or(LoweringError::ElseWithoutIf)?;
        state.stack.truncate(frame.stack_height);
        let mut reopened = frame;
        reopened.kind = ControlKind::Block;
        reopened.unreachable = false;
        controls.push(reopened);
        builder.start_block(else_block);
        return Ok(());
    }
    // A reserved but unfilled `else` block jumps straight to the merge. An `if`
    // with no `else` has no results, so it supplies no values.
    if let Some(else_block) = else_block {
        if !builder.is_terminated_at(else_block) {
            builder.terminate_at(
                else_block,
                Terminator::Branch {
                    target: merge,
                    values: Vec::new(),
                },
            );
        }
    }
    // A completed arm carries the label's results to the merge block, which
    // receives them as block parameters.
    if !builder.is_terminated() {
        let mut values = Vec::with_capacity(frame.label_types.len());
        for expected in frame.label_types.iter().rev() {
            values.push(state.pop(*expected)?);
        }
        values.reverse();
        builder.terminate(Terminator::Branch {
            target: merge,
            values,
        });
    }
    // Control continues at the merge block, whose parameters now hold the
    // label's results, so they go back on the value stack.
    builder.start_block(merge);
    // Restore the value stack to its height on entry to the label.
    state.stack.truncate(frame.stack_height);
    let index = merge.0 as usize;
    for parameter in builder.blocks[index].params.clone() {
        let value_type = state
            .values
            .iter()
            .find(|value| value.id == parameter)
            .map(|value| value.value_type)
            .ok_or(LoweringError::UnexpectedValues)?;
        state.push(parameter, value_type);
    }
    Ok(())
}

fn lower_function(
    function: &Function,
    function_type: &FunctionType,
    module: &tpt_wasm_format::Module,
) -> Result<IrFunction, LoweringError> {
    let (mut state, params, mut locals) = LoweringState::new(function, function_type)?;
    let mut reader = BodyReader::new(&function.body);
    let mut builder = BodyBuilder::new();
    // The function body is itself a label whose arity is the result arity.
    let mut controls: Vec<ControlFrame> = vec![ControlFrame {
        kind: ControlKind::Block,
        label_types: function_type.results.0.clone(),
        stack_height: 0,
        else_block: None,
        unreachable: false,
        target: BlockId(0),
    }];
    lower_body(&mut reader, module, &mut state, &mut builder, &mut controls)?;
    // Lowering may have appended synthetic local slots for values that must
    // survive across basic blocks, such as a `br_table` selector. Those slots
    // are real locals, so they join the function's local list and are
    // initialized like any other. `local_slots` holds the parameters first, so
    // the original locals start after them.
    let synthesized = state.local_slots[params.len() + locals.len()..].to_vec();
    locals.extend(synthesized);
    state.values.shrink_to_fit();
    Ok(IrFunction {
        function_type: function_type.clone(),
        params,
        locals,
        values: state.values,
        entry: BlockId(0),
        blocks: builder.blocks,
    })
}

/// Lower one function body into a control flow graph.
///
/// The body is scanned once. Structured constructs push a [`ControlFrame`];
/// a branch ends the current block, and a merge block carries the label's
/// values into the continuation. Values produced after a terminator are dead
/// and are not lowered, matching Wasm's unreachable-code rules.
fn lower_body(
    reader: &mut BodyReader<'_>,
    module: &tpt_wasm_format::Module,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
    controls: &mut Vec<ControlFrame>,
) -> Result<(), LoweringError> {
    loop {
        if reader.remaining() == 0 {
            return Err(LoweringError::UnexpectedEnd);
        }
        let opcode = reader.byte()?;
        let reachable = !controls
            .last()
            .map(|frame| frame.unreachable)
            .unwrap_or(false);

        match opcode {
            0x0b => {
                if controls.len() == 1 {
                    // The final `end` closes the function body.
                    if reader.remaining() != 0 {
                        return Err(LoweringError::TrailingBytes);
                    }
                    // A body already ended by `return`, `br`, or `unreachable`
                    // keeps that terminator; the `end` must not overwrite it.
                    if !builder.is_terminated() {
                        let results = state.return_values()?;
                        builder.terminate(Terminator::Return(results));
                    }
                    return Ok(());
                }
                close_label(state, builder, controls, false)?;
                continue;
            }
            0x05 => {
                // `else` begins the second arm of an `if`.
                if !reachable {
                    continue;
                }
                close_label(state, builder, controls, true)?;
                continue;
            }
            0x00 => {
                builder.terminate(Terminator::Unreachable);
                mark_unreachable(controls);
                continue;
            }
            0x0f => {
                if !reachable {
                    continue;
                }
                let results = state.return_values()?;
                builder.terminate(Terminator::Return(results));
                mark_unreachable(controls);
                continue;
            }
            0x0c | 0x0d => {
                lower_branch(opcode, reader, state, builder, controls)?;
                continue;
            }
            0x0e => {
                if reachable {
                    lower_branch_table(reader, state, builder, controls)?;
                } else {
                    // A `br_table` in dead code is still decoded: a count, that
                    // many labels, and a default.
                    let count = reader.u32()?;
                    for _ in 0..=count {
                        reader.u32()?;
                    }
                }
                continue;
            }
            0x02..=0x04 => {
                open_label(opcode, reader, state, builder, controls)?;
                continue;
            }
            _ if !reachable => {
                // Dead code: no value is produced, so nothing is lowered. The
                // stack is left as-is because the frame's height is restored
                // when the enclosing label closes.
                continue;
            }
            0x01 => {}
            0x10 => lower_call(reader.u32()?, module, state, builder)?,
            0x1a => {
                let value = state.pop_any()?;
                builder.push(IrInstr::Drop { value });
            }
            0x1b => lower_select(state, builder)?,
            0x20 => lower_local_get(reader.u32()?, state, builder)?,
            0x21 => lower_local_set(reader.u32()?, state, builder)?,
            0x22 => lower_local_tee(reader.u32()?, state, builder)?,
            0x41 => {
                let value = reader.i32()?;
                let result = state.allocate(ValueType::I32)?;
                builder.push(IrInstr::ConstI32 { result, value });
                state.push(result, ValueType::I32);
            }
            0x42 => {
                let value = reader.i64()?;
                let result = state.allocate(ValueType::I64)?;
                builder.push(IrInstr::ConstI64 { result, value });
                state.push(result, ValueType::I64);
            }
            0x43 => {
                let value = reader.f32()?;
                let result = state.allocate(ValueType::F32)?;
                builder.push(IrInstr::ConstF32 { result, value });
                state.push(result, ValueType::F32);
            }
            0x44 => {
                let value = reader.f64()?;
                let result = state.allocate(ValueType::F64)?;
                builder.push(IrInstr::ConstF64 { result, value });
                state.push(result, ValueType::F64);
            }
            0x45 => lower_eqz(0x45, ValueType::I32, state, builder)?,
            0x46..=0x4f => lower_compare(opcode, ValueType::I32, state, builder)?,
            0x50 => lower_eqz(0x50, ValueType::I64, state, builder)?,
            0x51..=0x5a => lower_compare(opcode, ValueType::I64, state, builder)?,
            0x5b..=0x60 => lower_float_compare(opcode, ValueType::F32, state, builder)?,
            0x61..=0x66 => lower_float_compare(opcode, ValueType::F64, state, builder)?,
            0x67..=0x69 => lower_unary(opcode, ValueType::I32, state, builder)?,
            0x79..=0x7b => lower_unary(opcode, ValueType::I64, state, builder)?,
            0xa7 | 0xac | 0xad => lower_int_conversion(opcode, state, builder)?,
            0xa8..=0xab | 0xae..=0xb1 => lower_float_trunc(opcode, state, builder)?,
            0xbc..=0xbf => lower_reinterpret(opcode, state, builder)?,
            0xb2..=0xbb => lower_float_conversion(opcode, state, builder)?,
            0x8b..=0x91 => lower_float_unary(opcode, ValueType::F32, state, builder)?,
            0x99..=0x9f => lower_float_unary(opcode, ValueType::F64, state, builder)?,
            0x6a..=0x78 | 0x7c..=0x8a | 0x92..=0x98 | 0xa0..=0xa6 => {
                lower_binary(opcode, state, builder)?
            }
            _ => return Err(LoweringError::UnsupportedInstruction(opcode)),
        }
    }
}

fn lower_call(
    function: u32,
    module: &tpt_wasm_format::Module,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let callee = module
        .functions
        .get(function as usize)
        .ok_or(LoweringError::UnknownFunction(function))?;
    let callee_type = module
        .types
        .get(callee.type_index as usize)
        .ok_or(LoweringError::UnknownType(callee.type_index))?;
    let mut arguments = Vec::with_capacity(callee_type.params.0.len());
    for expected in callee_type.params.0.iter().rev() {
        arguments.push(state.pop(*expected)?);
    }
    arguments.reverse();
    let results = callee_type
        .results
        .0
        .iter()
        .map(|value_type| {
            let result = state.allocate(*value_type)?;
            state.push(result, *value_type);
            Ok(result)
        })
        .collect::<Result<Vec<_>, LoweringError>>()?;
    builder.push(IrInstr::Call {
        function,
        arguments,
        results,
    });
    Ok(())
}

fn lower_select(state: &mut LoweringState, builder: &mut BodyBuilder) -> Result<(), LoweringError> {
    let condition = state.pop(ValueType::I32)?;
    let right = state.pop_any()?;
    let left = state.pop_any()?;
    let left_type = state
        .values
        .iter()
        .find(|value| value.id == left)
        .map(|value| value.value_type)
        .ok_or(LoweringError::UnexpectedValues)?;
    let right_type = state
        .values
        .iter()
        .find(|value| value.id == right)
        .map(|value| value.value_type)
        .ok_or(LoweringError::UnexpectedValues)?;
    if left_type != right_type {
        return Err(LoweringError::TypeMismatch {
            expected: left_type,
            actual: right_type,
        });
    }
    let result = state.allocate(left_type)?;
    builder.push(IrInstr::Select {
        result,
        condition,
        left,
        right,
    });
    state.push(result, left_type);
    Ok(())
}

fn lower_local_get(
    index: u32,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let (local, value_type) = state.local(index)?;
    let result = state.allocate(value_type)?;
    builder.push(IrInstr::LocalGet { result, local });
    state.push(result, value_type);
    Ok(())
}

fn lower_local_set(
    index: u32,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let (local, value_type) = state.local(index)?;
    let value = state.pop(value_type)?;
    builder.push(IrInstr::LocalSet { local, value });
    Ok(())
}

fn lower_local_tee(
    index: u32,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let (local, value_type) = state.local(index)?;
    let value = state.pop(value_type)?;
    let result = state.allocate(value_type)?;
    builder.push(IrInstr::LocalTee {
        result,
        local,
        value,
    });
    state.push(result, value_type);
    Ok(())
}

fn lower_eqz(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let value = state.pop(input_type)?;
    let result = state.allocate(ValueType::I32)?;
    let instruction = match input_type {
        ValueType::I32 => IrInstr::I32Eqz { result, value },
        ValueType::I64 => IrInstr::I64Eqz { result, value },
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    builder.push(instruction);
    state.push(result, ValueType::I32);
    Ok(())
}

fn lower_compare(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let right = state.pop(input_type)?;
    let left = state.pop(input_type)?;
    let result = state.allocate(ValueType::I32)?;
    let comparison = match input_type {
        ValueType::I32 => IntComparison::from_i32_opcode(opcode)?,
        ValueType::I64 => IntComparison::from_i64_opcode(opcode)?,
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    let instruction = match input_type {
        ValueType::I32 => IrInstr::I32Compare {
            result,
            left,
            right,
            comparison,
        },
        ValueType::I64 => IrInstr::I64Compare {
            result,
            left,
            right,
            comparison,
        },
        _ => unreachable!(),
    };
    builder.push(instruction);
    state.push(result, ValueType::I32);
    Ok(())
}

fn lower_float_compare(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let right = state.pop(input_type)?;
    let left = state.pop(input_type)?;
    let result = state.allocate(ValueType::I32)?;
    let instruction = match input_type {
        ValueType::F32 => IrInstr::F32Compare {
            result,
            left,
            right,
            comparison: FloatComparison::from_f32_opcode(opcode)?,
        },
        ValueType::F64 => IrInstr::F64Compare {
            result,
            left,
            right,
            comparison: FloatComparison::from_f64_opcode(opcode)?,
        },
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    builder.push(instruction);
    state.push(result, ValueType::I32);
    Ok(())
}

fn lower_unary(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let value = state.pop(input_type)?;
    let result = state.allocate(input_type)?;
    let operation = IntUnary::from_opcode(opcode)?;
    let instruction = match input_type {
        ValueType::I32 => IrInstr::I32Unary {
            result,
            value,
            operation,
        },
        ValueType::I64 => IrInstr::I64Unary {
            result,
            value,
            operation,
        },
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    builder.push(instruction);
    state.push(result, input_type);
    Ok(())
}

fn lower_int_conversion(
    opcode: u8,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let operation = IntConversion::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    builder.push(IrInstr::IntConvert {
        result,
        value,
        operation,
    });
    state.push(result, operation.result_type());
    Ok(())
}

fn lower_reinterpret(
    opcode: u8,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let operation = Reinterpret::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    builder.push(IrInstr::Reinterpret {
        result,
        value,
        operation,
    });
    state.push(result, operation.result_type());
    Ok(())
}

fn lower_float_trunc(
    opcode: u8,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let operation = FloatTrunc::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    builder.push(IrInstr::FloatTrunc {
        result,
        value,
        operation,
    });
    state.push(result, operation.result_type());
    Ok(())
}

fn lower_float_conversion(
    opcode: u8,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let operation = FloatConversion::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    builder.push(IrInstr::FloatConvert {
        result,
        value,
        operation,
    });
    state.push(result, operation.result_type());
    Ok(())
}

fn lower_float_unary(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let value = state.pop(input_type)?;
    let result = state.allocate(input_type)?;
    let operation = FloatUnary::from_opcode(opcode)?;
    let instruction = match input_type {
        ValueType::F32 => IrInstr::F32Unary {
            result,
            value,
            operation,
        },
        ValueType::F64 => IrInstr::F64Unary {
            result,
            value,
            operation,
        },
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    builder.push(instruction);
    state.push(result, input_type);
    Ok(())
}

fn lower_binary(
    opcode: u8,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let value_type = match opcode {
        0x6a..=0x78 => ValueType::I32,
        0x7c..=0x8a => ValueType::I64,
        0x92..=0x98 => ValueType::F32,
        0xa0..=0xa6 => ValueType::F64,
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    let right = state.pop(value_type)?;
    let left = state.pop(value_type)?;
    let result = state.allocate(value_type)?;
    let instruction = match opcode {
        0x6a => IrInstr::I32Add {
            result,
            left,
            right,
        },
        0x6b => IrInstr::I32Sub {
            result,
            left,
            right,
        },
        0x6c => IrInstr::I32Mul {
            result,
            left,
            right,
        },
        0x6d => IrInstr::I32DivS {
            result,
            left,
            right,
        },
        0x6e => IrInstr::I32DivU {
            result,
            left,
            right,
        },
        0x6f => IrInstr::I32RemS {
            result,
            left,
            right,
        },
        0x70 => IrInstr::I32RemU {
            result,
            left,
            right,
        },
        0x71 => IrInstr::I32And {
            result,
            left,
            right,
        },
        0x72 => IrInstr::I32Or {
            result,
            left,
            right,
        },
        0x73 => IrInstr::I32Xor {
            result,
            left,
            right,
        },
        0x74 => IrInstr::I32Shl {
            result,
            left,
            right,
        },
        0x75 => IrInstr::I32ShrS {
            result,
            left,
            right,
        },
        0x76 => IrInstr::I32ShrU {
            result,
            left,
            right,
        },
        0x77 => IrInstr::I32Rotl {
            result,
            left,
            right,
        },
        0x78 => IrInstr::I32Rotr {
            result,
            left,
            right,
        },
        0x7c => IrInstr::I64Add {
            result,
            left,
            right,
        },
        0x7d => IrInstr::I64Sub {
            result,
            left,
            right,
        },
        0x7e => IrInstr::I64Mul {
            result,
            left,
            right,
        },
        0x7f => IrInstr::I64DivS {
            result,
            left,
            right,
        },
        0x80 => IrInstr::I64DivU {
            result,
            left,
            right,
        },
        0x81 => IrInstr::I64RemS {
            result,
            left,
            right,
        },
        0x82 => IrInstr::I64RemU {
            result,
            left,
            right,
        },
        0x83 => IrInstr::I64And {
            result,
            left,
            right,
        },
        0x84 => IrInstr::I64Or {
            result,
            left,
            right,
        },
        0x85 => IrInstr::I64Xor {
            result,
            left,
            right,
        },
        0x86 => IrInstr::I64Shl {
            result,
            left,
            right,
        },
        0x87 => IrInstr::I64ShrS {
            result,
            left,
            right,
        },
        0x88 => IrInstr::I64ShrU {
            result,
            left,
            right,
        },
        0x89 => IrInstr::I64Rotl {
            result,
            left,
            right,
        },
        0x8a => IrInstr::I64Rotr {
            result,
            left,
            right,
        },
        0x92 => IrInstr::F32Add {
            result,
            left,
            right,
        },
        0x93 => IrInstr::F32Sub {
            result,
            left,
            right,
        },
        0x94 => IrInstr::F32Mul {
            result,
            left,
            right,
        },
        0x95 => IrInstr::F32Div {
            result,
            left,
            right,
        },
        0x96 => IrInstr::F32Min {
            result,
            left,
            right,
        },
        0x97 => IrInstr::F32Max {
            result,
            left,
            right,
        },
        0x98 => IrInstr::F32Copysign {
            result,
            left,
            right,
        },
        0xa0 => IrInstr::F64Add {
            result,
            left,
            right,
        },
        0xa1 => IrInstr::F64Sub {
            result,
            left,
            right,
        },
        0xa2 => IrInstr::F64Mul {
            result,
            left,
            right,
        },
        0xa3 => IrInstr::F64Div {
            result,
            left,
            right,
        },
        0xa4 => IrInstr::F64Min {
            result,
            left,
            right,
        },
        0xa5 => IrInstr::F64Max {
            result,
            left,
            right,
        },
        0xa6 => IrInstr::F64Copysign {
            result,
            left,
            right,
        },
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    builder.push(instruction);
    state.push(result, value_type);
    Ok(())
}

pub(crate) struct BodyReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> BodyReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn byte(&mut self) -> Result<u8, LoweringError> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or(LoweringError::UnexpectedEnd)?;
        self.position += 1;
        Ok(byte)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], LoweringError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(LoweringError::UnexpectedEnd)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(LoweringError::UnexpectedEnd)?;
        self.position = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, LoweringError> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            let payload = u32::from(byte & 0x7f);
            if shift == 28 && payload > 0x0f {
                return Err(LoweringError::InvalidLeb128);
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(LoweringError::InvalidLeb128)
    }

    fn i32(&mut self) -> Result<i32, LoweringError> {
        let value = self.signed(32)?;
        Ok(value as i32)
    }

    fn i64(&mut self) -> Result<i64, LoweringError> {
        self.signed(64)
    }

    fn f32(&mut self) -> Result<u32, LoweringError> {
        let bytes = self.take(4)?;
        let mut value = [0; 4];
        value.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(value))
    }

    fn f64(&mut self) -> Result<u64, LoweringError> {
        let bytes = self.take(8)?;
        let mut value = [0; 8];
        value.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(value))
    }

    fn signed(&mut self, bits: u32) -> Result<i64, LoweringError> {
        let max_bytes = bits.div_ceil(7) as usize;
        let mut result = 0i128;
        let mut shift = 0u32;
        for index in 0..max_bytes {
            let byte = self.byte()?;
            let payload = byte & 0x7f;
            if index + 1 == max_bytes {
                let available = bits - shift;
                if available < 7 {
                    let max_payload = (0x7fu8 >> (7 - available)) & 0x7f;
                    let sign_payload = (0x7fu8 << available) & 0x7f;
                    if payload > max_payload && payload < sign_payload {
                        return Err(LoweringError::InvalidLeb128);
                    }
                }
            }
            result |= i128::from(payload) << shift;
            if byte & 0x80 == 0 {
                let width = shift + 7;
                if width < bits && byte & 0x40 != 0 {
                    result |= -1i128 << width;
                }
                return Ok(result as i64);
            }
            shift += 7;
        }
        Err(LoweringError::InvalidLeb128)
    }
}

struct LoweringState {
    values: Vec<IrValue>,
    stack: Vec<(ValueId, ValueType)>,
    result_types: Vec<ValueType>,
    local_slots: Vec<ValueId>,
}

impl LoweringState {
    fn new(
        function: &Function,
        function_type: &FunctionType,
    ) -> Result<(Self, Vec<ValueId>, Vec<ValueId>), LoweringError> {
        let mut state = Self {
            values: Vec::new(),
            stack: Vec::new(),
            result_types: function_type.results.0.clone(),
            local_slots: Vec::new(),
        };
        let mut params = Vec::new();
        for value_type in &function_type.params.0 {
            params.push(state.allocate(*value_type)?);
        }
        let mut locals = Vec::new();
        for declaration in &function.locals {
            let count =
                usize::try_from(declaration.count).map_err(|_| LoweringError::LocalOverflow)?;
            for _ in 0..count {
                locals.push(state.allocate(declaration.value_type)?);
            }
        }
        state.local_slots = params.iter().chain(&locals).copied().collect();
        Ok((state, params, locals))
    }

    fn local(&self, index: u32) -> Result<(ValueId, ValueType), LoweringError> {
        let slot = *self
            .local_slots
            .get(index as usize)
            .ok_or(LoweringError::UnknownLocal(index))?;
        let value_type = self
            .values
            .get(slot.0 as usize)
            .map(|value| value.value_type)
            .ok_or(LoweringError::UnknownLocal(index))?;
        Ok((slot, value_type))
    }

    fn allocate(&mut self, value_type: ValueType) -> Result<ValueId, LoweringError> {
        let id = u32::try_from(self.values.len()).map_err(|_| LoweringError::LocalOverflow)?;
        self.values.push(IrValue {
            id: ValueId(id),
            value_type,
        });
        Ok(ValueId(id))
    }

    /// Allocate a synthetic local slot for a compiler-generated temporary.
    ///
    /// Lowering a `br_table` needs a value that survives across basic blocks,
    /// which SSA alone cannot express. The slot is appended to `local_slots` so
    /// it is initialized like any other local and can be named by
    /// `local.get`/`local.set` in the IR.
    fn allocate_temp_local(&mut self, value_type: ValueType) -> Result<ValueId, LoweringError> {
        let id = self.allocate(value_type)?;
        self.local_slots.push(id);
        Ok(id)
    }

    fn push(&mut self, id: ValueId, value_type: ValueType) {
        self.stack.push((id, value_type));
    }

    fn pop_any(&mut self) -> Result<ValueId, LoweringError> {
        self.stack
            .pop()
            .map(|(id, _)| id)
            .ok_or(LoweringError::StackUnderflow)
    }

    fn pop(&mut self, expected: ValueType) -> Result<ValueId, LoweringError> {
        let (id, actual) = self.stack.pop().ok_or(LoweringError::StackUnderflow)?;
        if actual != expected {
            return Err(LoweringError::TypeMismatch { expected, actual });
        }
        Ok(id)
    }

    fn return_values(&mut self) -> Result<Vec<ValueId>, LoweringError> {
        if self.stack.len() < self.result_types.len() {
            return Err(LoweringError::StackUnderflow);
        }
        if self.stack.len() > self.result_types.len() {
            return Err(LoweringError::UnexpectedValues);
        }
        let start = self.stack.len() - self.result_types.len();
        for (expected, (_, actual)) in self.result_types.iter().zip(&self.stack[start..]) {
            if expected != actual {
                return Err(LoweringError::TypeMismatch {
                    expected: *expected,
                    actual: *actual,
                });
            }
        }
        Ok(self.stack[start..].iter().map(|(id, _)| *id).collect())
    }
}
