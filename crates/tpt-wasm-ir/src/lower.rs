// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Validated WebAssembly-to-TPT-IR lowering.

use std::fmt;

use tpt_wasm_format::Function;
use tpt_wasm_types::{FunctionType, ReferenceType, Value, ValueType};
use tpt_wasm_validate::ValidatedModule;

use super::{
    BasicBlock, BlockId, FloatComparison, FloatConversion, FloatTrunc, FloatUnary, IntComparison,
    IntConversion, IntUnary, IrDataSegment, IrFunction, IrGlobal, IrInstr, IrMemory, IrModule,
    IrTable, IrValue, MemoryLoad, MemoryStore, Reinterpret, SignExtend, Terminator, ValueId,
};

/// Errors produced while lowering a validated module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringError {
    UnsupportedFeature(&'static str),
    UnsupportedInstruction(u8),
    UnknownType(u32),
    UnknownFunction(u32),
    /// A `global.get`/`global.set` naming a global that does not exist.
    UnknownGlobal(u32),
    /// `global.set` naming a global declared immutable.
    ImmutableGlobal(u32),
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
    let imports = lower_imports(module)?;
    // Wasm numbers imported functions before defined ones, so the count is what
    // a `call` or `ref.func` index is resolved against.
    let imported_functions = imports.len() as u32;

    let mut functions = Vec::with_capacity(module.functions.len());
    for function in &module.functions {
        let function_type = module
            .types
            .get(function.type_index as usize)
            .ok_or(LoweringError::UnknownType(function.type_index))?;
        functions.push(lower_function(
            function,
            function_type,
            module,
            imported_functions,
        )?);
    }
    let globals = module
        .globals
        .iter()
        .map(|global| {
            Ok(IrGlobal {
                value_type: global.global_type.value_type,
                mutable: global.global_type.mutable,
                init: read_const_expr(&global.init, global.global_type.value_type)?,
            })
        })
        .collect::<Result<Vec<_>, LoweringError>>()?;
    // MVP data segments are all active and all address memory 0, and their
    // offset must be a constant `i32`. They are kept in module order so that a
    // later segment overwrites an earlier one at the same address.
    let mut segments = Vec::with_capacity(module.data.len());
    for segment in &module.data {
        let tpt_wasm_format::DataMode::Active {
            memory_index,
            offset,
        } = &segment.mode
        else {
            return Err(LoweringError::UnsupportedFeature("passive data segments"));
        };
        if *memory_index != 0 {
            return Err(LoweringError::UnsupportedFeature("non-zero memory index"));
        }
        segments.push(IrDataSegment {
            offset: element_segment_offset(offset)?,
            bytes: segment.data.clone(),
        });
    }
    let memory = match module.memories.as_slice() {
        [] => None,
        [only] => {
            if only.memory_type.memory64 {
                return Err(LoweringError::UnsupportedFeature("64-bit memory"));
            }
            Some(IrMemory {
                min_pages: only.memory_type.limits.min,
                max_pages: only.memory_type.limits.max,
                segments,
            })
        }
        _ => return Err(LoweringError::UnsupportedFeature("multiple memories")),
    };
    // MVP allows at most one table, and its contents come from a single active
    // element segment whose offset must be a constant `i32`.
    let tables = match module.tables.as_slice() {
        [] => Vec::new(),
        [only] => {
            if only.table_type.element_type != tpt_wasm_types::ReferenceType::FuncRef {
                return Err(LoweringError::UnsupportedFeature("non-funcref table"));
            }
            let active: Vec<&tpt_wasm_format::Element> = module
                .elements
                .iter()
                .filter(|element| {
                    matches!(element.mode, tpt_wasm_format::ElementMode::Active { .. })
                })
                .collect();
            if active.len() > 1 {
                return Err(LoweringError::UnsupportedFeature(
                    "multiple element segments",
                ));
            }
            let (init, offset) = match active.first() {
                Some(element) => {
                    let tpt_wasm_format::ElementMode::Active {
                        table_index,
                        offset,
                    } = &element.mode
                    else {
                        unreachable!("filtered to active segments above");
                    };
                    if *table_index != 0 {
                        return Err(LoweringError::UnsupportedFeature("non-zero table index"));
                    }
                    (
                        lower_element_init(&element.init, imported_functions)?,
                        element_segment_offset(offset)?,
                    )
                }
                None => (Vec::new(), 0),
            };
            let elements = init;
            vec![IrTable {
                element_type: only.table_type.element_type,
                min: only.table_type.limits.min,
                max: only.table_type.limits.max,
                elements,
                offset,
            }]
        }
        _ => return Err(LoweringError::UnsupportedFeature("multiple tables")),
    };
    Ok(IrModule {
        functions,
        imports,
        globals,
        memory,
        tables,
        types: module.types.clone(),
    })
}

/// Read a constant `i32` expression, as an element segment offset is.
///
/// Returns `None` for anything that is not a plain `i32.const`, so a caller
/// that only needs the common case does not have to match on the error.
#[allow(dead_code)]
pub fn const_expr_i32(expr: &tpt_wasm_format::ConstExpr) -> Option<u32> {
    match read_const_expr(expr, ValueType::I32) {
        Ok(Value::I32(value)) => u32::try_from(value).ok(),
        _ => None,
    }
}

/// Read an element segment's constant offset, which MVP requires to be an `i32`.
fn element_segment_offset(expr: &tpt_wasm_format::ConstExpr) -> Result<u32, LoweringError> {
    let value = read_const_expr(expr, ValueType::I32)?;
    match value {
        Value::I32(offset) => u32::try_from(offset)
            .map_err(|_| LoweringError::UnsupportedFeature("negative element segment offset")),
        _ => Err(LoweringError::UnsupportedFeature("element segment offset")),
    }
}

fn reject_unsupported_module_state(module: &tpt_wasm_format::Module) -> Result<(), LoweringError> {
    // Function imports are supported: they become `IrImport` entries and a
    // `CallHost` crossing the host boundary. A table, memory, or global import
    // has no initializer the IR could carry, so it is still refused.
    for import in &module.imports {
        if !matches!(import.desc, tpt_wasm_format::ImportDesc::Function(_)) {
            return Err(LoweringError::UnsupportedFeature("non-function imports"));
        }
    }
    if module.start.is_some() {
        return Err(LoweringError::UnsupportedFeature("start functions"));
    }
    // Exports are module metadata rather than code: the runtime resolves them
    // against the module's own function table, so the IR need not carry them.
    Ok(())
}

/// Collect the module's function imports, in declaration order.
///
/// This order is what the lowerer resolves a Wasm function index against: an
/// index below this length is an import, and the rest address `IrModule::functions`.
fn lower_imports(module: &tpt_wasm_format::Module) -> Result<Vec<super::IrImport>, LoweringError> {
    let mut imports = Vec::new();
    for import in &module.imports {
        let tpt_wasm_format::ImportDesc::Function(type_index) = import.desc else {
            // Rejected by `reject_unsupported_module_state`; repeated here so
            // this function stands on its own.
            return Err(LoweringError::UnsupportedFeature("non-function imports"));
        };
        let function_type = module
            .types
            .get(type_index as usize)
            .ok_or(LoweringError::UnknownType(type_index))?;
        imports.push(super::IrImport {
            module: import.module.clone(),
            name: import.name.clone(),
            function_type: function_type.clone(),
        });
    }
    Ok(imports)
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
    /// Block control continues at once the label's body finishes normally.
    ///
    /// This differs from `target` only for a loop. A branch to a loop label
    /// re-enters its header, so the body must be able to *leave* by a different
    /// edge; collapsing the two would make falling off the end of a loop body
    /// jump back to the header instead of exiting.
    exit: BlockId,
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
        // A block may carry a reference result, which is what lets a `funcref`
        // travel through a block's label.
        0x70 => Ok(vec![ValueType::Ref(ReferenceType::FuncRef)]),
        0x6f => Ok(vec![ValueType::Ref(ReferenceType::ExternRef)]),
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
    // a branch can name them before either is filled. Even a loop gets its own
    // merge block: falling off the end of a loop body must exit the loop, which
    // is a different edge from the back edge into the header.
    let header = builder.allocate_block();
    let merge = builder.allocate_block();
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
        exit: merge,
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
                then_values: values.clone(),
                else_target: fallthrough,
                // `br_if` is `[t* i32] -> [t*]`: the taken edge hands `t*` to the
                // label and the fall-through keeps the same values, so both edges
                // carry them. Passing none here would silently drop a value the
                // fall-through still has to produce.
                else_values: values.clone(),
            });
            builder.start_block(fallthrough);
            // The fall-through block receives those values on its incoming edge, so
            // it declares them as parameters. A parameter is a fresh value rather
            // than the incoming one, which is what makes the join a phi: the
            // predecessor supplies the old ids and the block body sees the new ones.
            let mut params = Vec::with_capacity(values.len());
            for expected in frame.label_types.iter() {
                params.push(state.allocate(*expected)?);
            }
            builder.blocks[fallthrough.0 as usize].params = params.clone();
            // The joined values are back on the operand stack for the fall-through,
            // so the lowerer's own stack has to be restored to match. Without this
            // the block would end one operand short of its declared result.
            for (param, expected) in params.iter().zip(frame.label_types.iter()) {
                state.push(*param, *expected);
            }
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
    // `br_table l* lN` has type `[t* i32] -> [t*]`, so the selector is the top of
    // the stack and comes off *before* the label's values. Popping them the other
    // way round silently swaps the two: the branch then carries the selector and
    // the dispatch tests the carried value, which is wrong for every selector
    // that is not equal to the value it carries.
    let selector = state.pop(ValueType::I32)?;
    let values = pop_label_values(
        state,
        &ControlFrame {
            kind: ControlKind::Block,
            label_types: label_types.clone(),
            stack_height: 0,
            else_block: None,
            unreachable: false,
            target: BlockId(0),
            exit: BlockId(0),
        },
    )?;

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
    // The values an arm hands to its label must be the ones that dominate that
    // arm. The first block can use the values popped above, but every block after
    // it is reached only through the previous comparison, so the values are
    // threaded along as each block's parameters. Handing an arm the originals
    // would name a value that does not dominate it, and dropping them on the
    // fall-through edge would leave the remaining arms with nothing to branch.
    let mut carried = values;
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
        // The fall-through block receives the values on its incoming edge, so it
        // declares them as parameters. Fresh ids are what make that join a phi.
        let mut params = Vec::with_capacity(label_types.len());
        if fallthrough.is_some() {
            for expected in label_types.iter() {
                params.push(state.allocate(*expected)?);
            }
        }
        let terminator = match fallthrough {
            // The default arm is a plain branch: anything reaching here either
            // matched no index or the selector was out of range.
            None => Terminator::Branch {
                target,
                values: carried.clone(),
            },
            Some(fallthrough) => Terminator::CondBranch {
                condition: matched,
                then_target: target,
                then_values: carried.clone(),
                else_target: fallthrough,
                else_values: carried.clone(),
            },
        };
        builder.terminate(terminator);
        if let Some(fallthrough) = fallthrough {
            builder.blocks[fallthrough.0 as usize].params = params.clone();
            builder.start_block(fallthrough);
            for (param, expected) in params.iter().zip(label_types.iter()) {
                state.push(*param, *expected);
            }
            carried = params;
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
    // Control resumes at the label's exit block, which already carries the
    // label's results as block parameters. For a `block` or `if` the exit is
    // also the branch target; for a loop it is the block after the header.
    let merge = frame.exit;
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
    imported_functions: u32,
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
        exit: BlockId(0),
    }];
    lower_body(
        &mut reader,
        module,
        imported_functions,
        &mut state,
        &mut builder,
        &mut controls,
    )?;
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
    imported_functions: u32,
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
            0x10 => lower_call(reader.u32()?, module, imported_functions, state, builder)?,
            0x11 => {
                // `call_indirect` is a type index then a table index, followed on
                // the stack by the arguments and finally the table index.
                let type_index = reader.u32()?;
                let table = reader.u32()?;
                lower_call_indirect(type_index, table, module, state, builder)?
            }
            0x1a => {
                let value = state.pop_any()?;
                builder.push(IrInstr::Drop { value });
            }
            0x1b => lower_select(state, builder)?,
            0x20 => lower_local_get(reader.u32()?, state, builder)?,
            0x21 => lower_local_set(reader.u32()?, state, builder)?,
            0x22 => lower_local_tee(reader.u32()?, state, builder)?,
            0x23 => lower_global_get(reader.u32()?, module, state, builder)?,
            0x24 => lower_global_set(reader.u32()?, module, state, builder)?,
            0x28..=0x35 => lower_load(opcode, reader, state, builder)?,
            0x36..=0x3e => lower_store(opcode, reader, state, builder)?,
            0x3f => {
                // `memory.size` carries a reserved memory index byte that MVP
                // requires to be zero.
                let index = reader.byte()?;
                if index != 0 {
                    return Err(LoweringError::UnsupportedFeature("multi-memory"));
                }
                let result = state.allocate(ValueType::I32)?;
                builder.push(IrInstr::MemorySize { result });
                state.push(result, ValueType::I32);
            }
            0x40 => {
                let index = reader.byte()?;
                if index != 0 {
                    return Err(LoweringError::UnsupportedFeature("multi-memory"));
                }
                let delta = state.pop(ValueType::I32)?;
                let result = state.allocate(ValueType::I32)?;
                builder.push(IrInstr::MemoryGrow { result, delta });
                state.push(result, ValueType::I32);
            }
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
            0xc0..=0xc4 => lower_sign_extend(opcode, state, builder)?,
            0xb2..=0xbb => lower_float_conversion(opcode, state, builder)?,
            0x8b..=0x91 => lower_float_unary(opcode, ValueType::F32, state, builder)?,
            0x99..=0x9f => lower_float_unary(opcode, ValueType::F64, state, builder)?,
            0x6a..=0x78 | 0x7c..=0x8a | 0x92..=0x98 | 0xa0..=0xa6 => {
                lower_binary(opcode, state, builder)?
            }
            0xd0 => {
                let reference_type = read_reference_type(reader.byte()?)?;
                let result = state.allocate(ValueType::Ref(reference_type))?;
                builder.push(IrInstr::RefNull {
                    result,
                    reference_type,
                });
                state.push(result, ValueType::Ref(reference_type));
            }
            0xd1 => {
                let value = state.pop_reference()?;
                let result = state.allocate(ValueType::I32)?;
                builder.push(IrInstr::RefIsNull { result, value });
                state.push(result, ValueType::I32);
            }
            0xd2 => {
                let index = reader.u32()?;
                // `ref.func` names the Wasm function index space, so an index
                // below the import count would name an imported function, which
                // the shared rebasing helper refuses for the same reason an
                // element segment entry does.
                let defined = defined_index(index, imported_functions)?;
                if usize::try_from(defined).map_or(true, |i| i >= module.functions.len()) {
                    return Err(LoweringError::UnknownFunction(index));
                }
                let result = state.allocate(ValueType::Ref(ReferenceType::FuncRef))?;
                builder.push(IrInstr::RefFunc {
                    result,
                    function: defined,
                });
                state.push(result, ValueType::Ref(ReferenceType::FuncRef));
            }
            _ => return Err(LoweringError::UnsupportedInstruction(opcode)),
        }
    }
}

/// Read a reference type immediate: `funcref` is 0x70 and `externref` is 0x6f.
fn read_reference_type(byte: u8) -> Result<ReferenceType, LoweringError> {
    match byte {
        0x70 => Ok(ReferenceType::FuncRef),
        0x6f => Ok(ReferenceType::ExternRef),
        _ => Err(LoweringError::UnsupportedFeature("reference type")),
    }
}

/// Map a load opcode to its width, signedness, and result type.
fn load_operation(opcode: u8) -> Option<MemoryLoad> {
    Some(match opcode {
        0x28 => MemoryLoad::I32,
        0x29 => MemoryLoad::I64,
        0x2a => MemoryLoad::F32,
        0x2b => MemoryLoad::F64,
        0x2c => MemoryLoad::I32_8S,
        0x2d => MemoryLoad::I32_8U,
        0x2e => MemoryLoad::I32_16S,
        0x2f => MemoryLoad::I32_16U,
        0x30 => MemoryLoad::I64_8S,
        0x31 => MemoryLoad::I64_8U,
        0x32 => MemoryLoad::I64_16S,
        0x33 => MemoryLoad::I64_16U,
        0x34 => MemoryLoad::I64_32S,
        0x35 => MemoryLoad::I64_32U,
        _ => return None,
    })
}

/// Map a store opcode to the bytes it writes and the value it consumes.
fn store_operation(opcode: u8) -> Option<MemoryStore> {
    Some(match opcode {
        0x36 => MemoryStore::I32,
        0x37 => MemoryStore::I64,
        0x38 => MemoryStore::F32,
        0x39 => MemoryStore::F64,
        0x3a => MemoryStore::I32_8,
        0x3b => MemoryStore::I32_16,
        0x3c => MemoryStore::I64_8,
        0x3d => MemoryStore::I64_16,
        0x3e => MemoryStore::I64_32,
        _ => return None,
    })
}

/// Read a `memarg`. The alignment hint is advisory in Wasm and is not carried in
/// the IR, so only the static offset is kept.
fn read_memarg(reader: &mut BodyReader<'_>) -> Result<u32, LoweringError> {
    let _align = reader.u32()?;
    reader.u32()
}

fn lower_load(
    opcode: u8,
    reader: &mut BodyReader<'_>,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let operation = load_operation(opcode).ok_or(LoweringError::UnsupportedInstruction(opcode))?;
    let offset = read_memarg(reader)?;
    let address = state.pop(ValueType::I32)?;
    let result_type = operation.result_type();
    let result = state.allocate(result_type)?;
    builder.push(IrInstr::Load {
        result,
        address,
        offset,
        operation,
    });
    state.push(result, result_type);
    Ok(())
}

fn lower_store(
    opcode: u8,
    reader: &mut BodyReader<'_>,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let operation = store_operation(opcode).ok_or(LoweringError::UnsupportedInstruction(opcode))?;
    let offset = read_memarg(reader)?;
    let value = state.pop(operation.operand_type())?;
    let address = state.pop(ValueType::I32)?;
    builder.push(IrInstr::Store {
        address,
        value,
        offset,
        operation,
    });
    Ok(())
}

fn lower_global_get(
    index: u32,
    module: &tpt_wasm_format::Module,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let declaration = module
        .globals
        .get(index as usize)
        .ok_or(LoweringError::UnknownGlobal(index))?;
    let value_type = declaration.global_type.value_type;
    let result = state.allocate(value_type)?;
    builder.push(IrInstr::GlobalGet {
        result,
        global: index,
    });
    state.push(result, value_type);
    Ok(())
}

fn lower_global_set(
    index: u32,
    module: &tpt_wasm_format::Module,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let declaration = module
        .globals
        .get(index as usize)
        .ok_or(LoweringError::UnknownGlobal(index))?;
    if !declaration.global_type.mutable {
        return Err(LoweringError::ImmutableGlobal(index));
    }
    let value = state.pop(declaration.global_type.value_type)?;
    builder.push(IrInstr::GlobalSet {
        global: index,
        value,
    });
    Ok(())
}

/// Decode a global's constant initializer.
///
/// MVP initializers are a single numeric constant followed by `end`. Anything
/// else needs the import and reference machinery, and is rejected rather than
/// approximated.
pub(crate) fn read_const_expr(
    expr: &tpt_wasm_format::ConstExpr,
    value_type: ValueType,
) -> Result<Value, LoweringError> {
    let mut reader = BodyReader::new(&expr.0);
    // The initializer is `t.const <immediate> end`, so the opcode comes first and
    // must agree with the declared type.
    let value = match (reader.byte()?, value_type) {
        (0x41, ValueType::I32) => Value::I32(reader.i32()?),
        (0x42, ValueType::I64) => Value::I64(reader.i64()?),
        (0x43, ValueType::F32) => Value::F32(reader.f32()?),
        (0x44, ValueType::F64) => Value::F64(reader.f64()?),
        _ => return Err(LoweringError::UnsupportedFeature("global initializer")),
    };
    if reader.byte()? != 0x0b || reader.remaining() != 0 {
        return Err(LoweringError::UnsupportedFeature("global initializer"));
    }
    Ok(value)
}

/// Lower an element segment's entries into the IR's table representation.
///
/// The two encoding families converge here. Plain indices are already function
/// numbers in the module's index space, so they only need rebasing past the
/// imports. An expression is either `ref.null`, which leaves the slot empty, or
/// `ref.func`, which names a function and is rebased the same way.
fn lower_element_init(
    init: &tpt_wasm_format::ElementInit,
    imported_functions: u32,
) -> Result<Vec<Option<u32>>, LoweringError> {
    match init {
        tpt_wasm_format::ElementInit::FuncIndices(indices) => indices
            .iter()
            .map(|index| Ok(Some(defined_index(*index, imported_functions)?)))
            .collect(),
        tpt_wasm_format::ElementInit::Expressions(expressions) => expressions
            .iter()
            .map(|expression| read_table_init_expr(expression, imported_functions))
            .collect(),
    }
}

/// Read one `ref.null` or `ref.func` element initializer.
fn read_table_init_expr(
    expr: &tpt_wasm_format::ConstExpr,
    imported_functions: u32,
) -> Result<Option<u32>, LoweringError> {
    let mut reader = BodyReader::new(&expr.0);
    let entry = match reader.byte()? {
        // `ref.null t`, which initializes the slot to null.
        0xd0 => match reader.byte()? {
            0x70 | 0x6f => None,
            _ => return Err(LoweringError::UnsupportedFeature("element initializer")),
        },
        0xd2 => Some(defined_index(reader.u32()?, imported_functions)?),
        _ => return Err(LoweringError::UnsupportedFeature("element initializer")),
    };
    if reader.byte()? != 0x0b || reader.remaining() != 0 {
        return Err(LoweringError::UnsupportedFeature("element initializer"));
    }
    Ok(entry)
}

/// Rebase a module-level function index onto the IR's index space.
///
/// The IR numbers only the module's own functions and turns everything below the
/// import count into a host call. A reference or a table entry has to name a
/// concrete function of this module, so an index below that count is refused
/// rather than redirected at a different function.
fn defined_index(index: u32, imported_functions: u32) -> Result<u32, LoweringError> {
    if index < imported_functions {
        return Err(LoweringError::UnsupportedFeature(
            "reference to an imported function",
        ));
    }
    Ok(index - imported_functions)
}

fn lower_call(
    function: u32,
    module: &tpt_wasm_format::Module,
    imported_functions: u32,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    // Wasm numbers imported functions before defined ones. An index below the
    // import count crosses the host boundary; the rest address the module's own
    // functions, rebased so the IR needs only the defined ones.
    if function < imported_functions {
        let callee_type = import_function_type(function, module)?;
        let arguments = pop_arguments(callee_type, state)?;
        let results = allocate_results(callee_type, state)?;
        builder.push(IrInstr::CallHost {
            import: function,
            arguments,
            results,
        });
        return Ok(());
    }
    let defined = function - imported_functions;
    let callee = module
        .functions
        .get(defined as usize)
        .ok_or(LoweringError::UnknownFunction(function))?;
    let callee_type = module
        .types
        .get(callee.type_index as usize)
        .ok_or(LoweringError::UnknownType(callee.type_index))?;
    let arguments = pop_arguments(callee_type, state)?;
    let results = allocate_results(callee_type, state)?;
    builder.push(IrInstr::Call {
        function: defined,
        arguments,
        results,
    });
    Ok(())
}

/// The declared type of the `index`th function import, in declaration order.
fn import_function_type(
    index: u32,
    module: &tpt_wasm_format::Module,
) -> Result<&FunctionType, LoweringError> {
    let mut seen = 0u32;
    for import in &module.imports {
        if let tpt_wasm_format::ImportDesc::Function(type_index) = import.desc {
            if seen == index {
                return module
                    .types
                    .get(type_index as usize)
                    .ok_or(LoweringError::UnknownType(type_index));
            }
            seen += 1;
        }
    }
    Err(LoweringError::UnknownFunction(index))
}
/// Pop the declared arguments, topmost first.
fn pop_arguments(
    callee_type: &tpt_wasm_types::FunctionType,
    state: &mut LoweringState,
) -> Result<Vec<ValueId>, LoweringError> {
    let mut arguments = Vec::with_capacity(callee_type.params.0.len());
    for expected in callee_type.params.0.iter().rev() {
        arguments.push(state.pop(*expected)?);
    }
    arguments.reverse();
    Ok(arguments)
}

/// Allocate the declared results and push them onto the operand stack.
fn allocate_results(
    callee_type: &tpt_wasm_types::FunctionType,
    state: &mut LoweringState,
) -> Result<Vec<ValueId>, LoweringError> {
    callee_type
        .results
        .0
        .iter()
        .map(|value_type| {
            let result = state.allocate(*value_type)?;
            state.push(result, *value_type);
            Ok(result)
        })
        .collect()
}

fn lower_call_indirect(
    type_index: u32,
    table: u32,
    module: &tpt_wasm_format::Module,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let callee_type = module
        .types
        .get(type_index as usize)
        .ok_or(LoweringError::UnknownType(type_index))?;
    // The table index sits above the arguments on the operand stack, so it is
    // popped first. The arguments come off underneath it, topmost first, and
    // the results are pushed only once the index has been taken, since a result
    // of the same type would otherwise be mistaken for it.
    let operand = state.pop(ValueType::I32)?;
    let arguments = pop_arguments(callee_type, state)?;
    let results = allocate_results(callee_type, state)?;
    builder.push(IrInstr::CallIndirect {
        type_index,
        table,
        operand,
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

fn lower_sign_extend(
    opcode: u8,
    state: &mut LoweringState,
    builder: &mut BodyBuilder,
) -> Result<(), LoweringError> {
    let operation = SignExtend::from_opcode(opcode)?;
    let value_type = operation.value_type();
    let value = state.pop(value_type)?;
    let result = state.allocate(value_type)?;
    builder.push(IrInstr::SignExtend {
        result,
        value,
        operation,
    });
    state.push(result, value_type);
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

    /// Pop a value that must be a reference, of either kind.
    ///
    /// `ref.is_null` accepts both reference types, so the kind cannot be named
    /// as the expected type the way [`LoweringState::pop`] does.
    fn pop_reference(&mut self) -> Result<ValueId, LoweringError> {
        let (id, actual) = self.stack.pop().ok_or(LoweringError::StackUnderflow)?;
        if !matches!(actual, ValueType::Ref(_)) {
            return Err(LoweringError::TypeMismatch {
                expected: ValueType::Ref(ReferenceType::FuncRef),
                actual,
            });
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
