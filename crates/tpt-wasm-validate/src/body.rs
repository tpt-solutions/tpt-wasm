// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Checked MVP instruction validator internals.

use tpt_wasm_format::Function;
use tpt_wasm_types::{FunctionType, GlobalType, RefType, ValueType};

use super::{validator::ModuleContext, ValidationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlKind {
    Function,
    Block,
    Loop,
    If,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlFrame {
    pub kind: ControlKind,
    pub label_types: Vec<ValueType>,
    pub end_types: Vec<ValueType>,
    pub stack_height: usize,
    pub unreachable: bool,
    pub saw_else: bool,
}

impl ControlFrame {
    pub fn function(results: Vec<ValueType>) -> Self {
        Self {
            kind: ControlKind::Function,
            label_types: results.clone(),
            end_types: results,
            stack_height: 0,
            unreachable: false,
            saw_else: false,
        }
    }
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    pub fn byte(&mut self) -> Result<u8, ValidationError> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or(ValidationError::UnexpectedEof)?;
        self.position += 1;
        Ok(byte)
    }

    pub fn take(&mut self, length: usize) -> Result<&'a [u8], ValidationError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(ValidationError::InvalidBody(
                "immediate is too large".into(),
            ))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(ValidationError::UnexpectedEof)?;
        self.position = end;
        Ok(bytes)
    }

    pub fn u32(&mut self) -> Result<u32, ValidationError> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            let payload = u32::from(byte & 0x7f);
            if shift == 28 && payload > 0x0f {
                return Err(ValidationError::InvalidLeb128);
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(ValidationError::InvalidLeb128)
    }

    pub fn i32(&mut self) -> Result<i32, ValidationError> {
        let mut result = 0i64;
        let mut shift = 0;
        for index in 0..5 {
            let byte = self.byte()?;
            result |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if index == 4 {
                    let sign = (result >> 31) & 1;
                    let extension = if sign == 0 { 0 } else { -1 };
                    if ((result >> 32) & 0x7) != (extension & 0x7) {
                        return Err(ValidationError::InvalidLeb128);
                    }
                } else if shift < 32 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result as i32);
            }
        }
        Err(ValidationError::InvalidLeb128)
    }

    pub fn i64(&mut self) -> Result<i64, ValidationError> {
        let mut result = 0i128;
        let mut shift = 0;
        for index in 0..10 {
            let byte = self.byte()?;
            result |= i128::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if index == 9 {
                    let sign = (result >> 63) & 1;
                    let extension = if sign == 0 { 0 } else { -1 };
                    if ((result >> 64) & 0x3f) != (extension & 0x3f) {
                        return Err(ValidationError::InvalidLeb128);
                    }
                } else if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i128 << shift;
                }
                return Ok(result as i64);
            }
        }
        Err(ValidationError::InvalidLeb128)
    }

    pub fn f32(&mut self) -> Result<u32, ValidationError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ValidationError::UnexpectedEof)?,
        ))
    }

    pub fn f64(&mut self) -> Result<u64, ValidationError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ValidationError::UnexpectedEof)?,
        ))
    }
}

fn type_name(value: ValueType) -> String {
    match value {
        ValueType::I32 => "i32".into(),
        ValueType::I64 => "i64".into(),
        ValueType::F32 => "f32".into(),
        ValueType::F64 => "f64".into(),
        ValueType::V128 => "v128".into(),
        ValueType::Ref(_) => "reference".into(),
    }
}

fn mismatch(expected: ValueType, actual: Option<ValueType>) -> ValidationError {
    ValidationError::TypeMismatch {
        expected: type_name(expected),
        actual: actual.map(type_name).unwrap_or_else(|| "unknown".into()),
    }
}

/// Pop a value that must be a reference, of either kind.
///
/// `ref.is_null` accepts both reference types, so unlike `pop_value` this
/// cannot name the one expected type. A polymorphic operand in unreachable code
/// stands for whatever the surrounding frame will supply, so it is accepted
/// here and checked for real wherever it is produced.
fn pop_reference(
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
) -> Result<Option<ValueType>, ValidationError> {
    match pop_value(stack, controls, None)? {
        None | Some(ValueType::Ref(_)) => Ok(None),
        Some(other) => Err(ValidationError::TypeMismatch {
            expected: "reference".into(),
            actual: type_name(other),
        }),
    }
}

fn pop_value(
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    expected: Option<ValueType>,
) -> Result<Option<ValueType>, ValidationError> {
    let frame = controls.last().ok_or(ValidationError::InvalidBody(
        "instruction outside a control frame".into(),
    ))?;
    if stack.len() == frame.stack_height {
        if frame.unreachable {
            return Ok(None);
        }
        return Err(ValidationError::StackUnderflow);
    }
    let actual = stack.pop().ok_or(ValidationError::StackUnderflow)?;
    if let (Some(expected), Some(actual)) = (expected, actual) {
        if expected != actual {
            return Err(mismatch(expected, Some(actual)));
        }
    }
    Ok(actual)
}

fn push_value(stack: &mut Vec<Option<ValueType>>, value: ValueType) {
    stack.push(Some(value));
}

fn pop_values(
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    values: &[ValueType],
) -> Result<(), ValidationError> {
    for value in values.iter().rev() {
        pop_value(stack, controls, Some(*value))?;
    }
    Ok(())
}

fn set_unreachable(stack: &mut Vec<Option<ValueType>>, controls: &mut [ControlFrame]) {
    let height = controls.last().map(|frame| frame.stack_height).unwrap_or(0);
    stack.truncate(height);
    if let Some(frame) = controls.last_mut() {
        frame.unreachable = true;
    }
}

fn block_type(reader: &mut Reader<'_>) -> Result<Vec<ValueType>, ValidationError> {
    let byte = reader.byte()?;
    match byte {
        0x40 => Ok(Vec::new()),
        0x7f => Ok(vec![ValueType::I32]),
        0x7e => Ok(vec![ValueType::I64]),
        0x7d => Ok(vec![ValueType::F32]),
        0x7c => Ok(vec![ValueType::F64]),
        // A block may carry a reference result, which is what lets a `funcref`
        // travel through a block's label.
        0x70 => Ok(vec![ValueType::Ref(RefType::FuncRef)]),
        0x6f => Ok(vec![ValueType::Ref(RefType::ExternRef)]),
        _ => Err(ValidationError::InvalidBlockType),
    }
}

fn label_types(controls: &[ControlFrame], depth: u32) -> Result<Vec<ValueType>, ValidationError> {
    let index = controls
        .len()
        .checked_sub(depth as usize + 1)
        .ok_or(ValidationError::UnknownLabel(depth))?;
    Ok(controls[index].label_types.clone())
}

fn convert(
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    from: ValueType,
    to: ValueType,
) -> Result<(), ValidationError> {
    pop_value(stack, controls, Some(from))?;
    push_value(stack, to);
    Ok(())
}

fn end_control(
    stack: &mut Vec<Option<ValueType>>,
    controls: &mut Vec<ControlFrame>,
) -> Result<(), ValidationError> {
    let frame = controls
        .last()
        .cloned()
        .ok_or_else(|| ValidationError::InvalidBody("unexpected end".into()))?;
    pop_values(stack, controls, &frame.end_types)?;
    if stack.len() != frame.stack_height {
        return Err(ValidationError::InvalidBody(
            "operand values remain after control frame".into(),
        ));
    }
    controls.pop();
    if !controls.is_empty() {
        for value in frame.end_types {
            push_value(stack, value);
        }
    }
    Ok(())
}

fn enter_control(
    stack: &mut [Option<ValueType>],
    controls: &mut Vec<ControlFrame>,
    kind: ControlKind,
    end_types: Vec<ValueType>,
) {
    let height = stack.len();
    let label_types = if kind == ControlKind::Loop {
        Vec::new()
    } else {
        end_types.clone()
    };
    controls.push(ControlFrame {
        kind,
        label_types,
        end_types,
        stack_height: height,
        unreachable: false,
        saw_else: false,
    });
}

pub(crate) fn validate_function(
    function: &Function,
    function_type: &FunctionType,
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    const MAX_LOCALS: u64 = 1_000_000;
    let mut locals = function_type.params.0.clone();
    let mut local_count = locals.len() as u64;
    for declaration in &function.locals {
        if !declaration.value_type.is_supported() {
            return Err(ValidationError::UnsupportedFeature(
                "unsupported local type",
            ));
        }
        local_count = local_count
            .checked_add(u64::from(declaration.count))
            .ok_or(ValidationError::InvalidLocalCount)?;
        if local_count > MAX_LOCALS {
            return Err(ValidationError::InvalidLocalCount);
        }
        locals.resize(
            locals
                .len()
                .checked_add(declaration.count as usize)
                .ok_or(ValidationError::InvalidLocalCount)?,
            declaration.value_type,
        );
    }

    let mut reader = Reader::new(&function.body);
    let mut stack = Vec::new();
    let mut controls = vec![ControlFrame::function(function_type.results.0.clone())];
    while reader.remaining() > 0 {
        if controls.is_empty() {
            return Err(ValidationError::InvalidBody(
                "instructions follow the function end".into(),
            ));
        }
        let opcode = reader.byte()?;
        validate_instruction(
            opcode,
            &mut reader,
            &mut stack,
            &mut controls,
            &locals,
            module,
        )?;
    }
    if !controls.is_empty() {
        return Err(ValidationError::InvalidBody(
            "function body ended before the function frame".into(),
        ));
    }
    Ok(())
}

fn pop_any(
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
) -> Result<Option<ValueType>, ValidationError> {
    pop_value(stack, controls, None)
}

fn select_result(
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
) -> Result<(), ValidationError> {
    pop_value(stack, controls, Some(ValueType::I32))?;
    let right = pop_any(stack, controls)?;
    let left = pop_any(stack, controls)?;
    if let (Some(left), Some(right)) = (left, right) {
        if left != right || !left.is_mvp_numeric() {
            return Err(ValidationError::TypeMismatch {
                expected: "matching numeric values".into(),
                actual: format!("{left:?}/{right:?}"),
            });
        }
        push_value(stack, left);
    } else {
        stack.push(left.or(right));
    }
    Ok(())
}

/// Bit 6 of a `memarg`'s alignment field: set when a memory index precedes the offset (multi-memory).
const MEMARG_INDEXED: u32 = 0x40;

/// Read a memory index and check that the module declares that memory.
fn memory_index(reader: &mut Reader<'_>, module: &ModuleContext) -> Result<u32, ValidationError> {
    let index = reader.u32()?;
    if index as usize >= module.memory_count {
        return Err(ValidationError::UnknownMemory(index));
    }
    Ok(index)
}

/// A `memarg`: the alignment flags, an optional memory index (bit 6 of the flags), and the offset.
/// Without the index the access names memory 0, which the module must declare.
fn memory_arg(
    reader: &mut Reader<'_>,
    module: &ModuleContext,
    natural_alignment: u32,
) -> Result<(), ValidationError> {
    let flags = reader.u32()?;
    let alignment = flags & !MEMARG_INDEXED;
    if flags & MEMARG_INDEXED != 0 {
        memory_index(reader, module)?;
    } else if module.memory_count == 0 {
        return Err(ValidationError::UnknownMemory(0));
    }
    let _offset = reader.u32()?;
    if alignment > 31 || alignment > natural_alignment {
        return Err(ValidationError::InvalidMemoryAlignment(alignment));
    }
    Ok(())
}

fn branch_types(
    reader: &mut Reader<'_>,
    controls: &[ControlFrame],
) -> Result<Vec<ValueType>, ValidationError> {
    label_types(controls, reader.u32()?)
}

fn local(index: u32, locals: &[ValueType]) -> Result<ValueType, ValidationError> {
    locals
        .get(index as usize)
        .copied()
        .ok_or(ValidationError::InvalidBody(format!(
            "unknown local {index}"
        )))
}

fn global_type(index: u32, module: &ModuleContext) -> Result<GlobalType, ValidationError> {
    module
        .global_types
        .get(index as usize)
        .copied()
        .ok_or(ValidationError::UnknownGlobal(index))
}

fn function_type(index: u32, module: &ModuleContext) -> Result<&FunctionType, ValidationError> {
    module
        .function_types
        .get(index as usize)
        .ok_or(ValidationError::UnknownFunction(index))
}

fn control_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &mut Vec<ControlFrame>,
) -> Result<(), ValidationError> {
    match opcode {
        0x00 => set_unreachable(stack, controls),
        0x01 => {}
        0x02 => {
            let end_types = block_type(reader)?;
            enter_control(stack, controls, ControlKind::Block, end_types);
        }
        0x03 => {
            let end_types = block_type(reader)?;
            enter_control(stack, controls, ControlKind::Loop, end_types);
        }
        0x04 => {
            pop_value(stack, controls, Some(ValueType::I32))?;
            let end_types = block_type(reader)?;
            enter_control(stack, controls, ControlKind::If, end_types);
        }
        0x05 => {
            let frame = controls
                .last_mut()
                .ok_or(ValidationError::InvalidBody("else outside an if".into()))?;
            if frame.kind != ControlKind::If || frame.saw_else {
                return Err(ValidationError::InvalidBody("invalid else".into()));
            }
            let end_types = frame.end_types.clone();
            let height = frame.stack_height;
            pop_values(stack, controls, &end_types)?;
            if stack.len() != height {
                return Err(ValidationError::InvalidBody(
                    "then branch leaves values for else".into(),
                ));
            }
            stack.truncate(height);
            let frame = controls.last_mut().expect("if frame exists");
            frame.saw_else = true;
            frame.unreachable = false;
        }
        0x0b => {
            let frame = controls.last().ok_or(ValidationError::InvalidBody(
                "end outside a control frame".into(),
            ))?;
            if frame.kind == ControlKind::If && !frame.saw_else && !frame.end_types.is_empty() {
                return Err(ValidationError::InvalidBody(
                    "if without else must not produce values".into(),
                ));
            }
            end_control(stack, controls)?;
        }
        0x0c => {
            let types = branch_types(reader, controls)?;
            pop_values(stack, controls, &types)?;
            set_unreachable(stack, controls);
        }
        0x0d => {
            pop_value(stack, controls, Some(ValueType::I32))?;
            let types = branch_types(reader, controls)?;
            // `br_if l` has type `[t* i32] -> [t*]`. The taken path hands `t*` to
            // the label, but the fall-through keeps them, so they are checked and
            // then put back. Only an empty label type makes the two paths agree on
            // the resulting stack, which is why this is invisible for a `br_if` to
            // a void block and wrong for one that carries a value.
            pop_values(stack, controls, &types)?;
            for value in types {
                push_value(stack, value);
            }
        }
        0x0e => {
            pop_value(stack, controls, Some(ValueType::I32))?;
            let count = reader.u32()?;
            if count as usize > reader.remaining() {
                return Err(ValidationError::InvalidBody(
                    "branch table is larger than the remaining body".into(),
                ));
            }
            let mut labels = Vec::new();
            for _ in 0..count {
                labels.push(label_types(controls, reader.u32()?)?);
            }
            let default = label_types(controls, reader.u32()?)?;
            if labels.iter().any(|types| types != &default) {
                return Err(ValidationError::InvalidBranchTypes);
            }
            pop_values(stack, controls, &default)?;
            set_unreachable(stack, controls);
        }
        0x0f => {
            let types = controls
                .first()
                .map(|frame| frame.end_types.clone())
                .ok_or(ValidationError::InvalidBody(
                    "missing function frame".into(),
                ))?;
            pop_values(stack, controls, &types)?;
            set_unreachable(stack, controls);
        }
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    }
    Ok(())
}

fn variable_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    locals: &[ValueType],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    match opcode {
        0x20 => {
            let value = local(reader.u32()?, locals)?;
            push_value(stack, value);
        }
        0x21 => {
            let value = local(reader.u32()?, locals)?;
            pop_value(stack, controls, Some(value))?;
        }
        0x22 => {
            let value = local(reader.u32()?, locals)?;
            pop_value(stack, controls, Some(value))?;
            push_value(stack, value);
        }
        0x23 => {
            let value = global_type(reader.u32()?, module)?.value_type;
            push_value(stack, value);
        }
        0x24 => {
            let index = reader.u32()?;
            let global = global_type(index, module)?;
            if !global.mutable {
                return Err(ValidationError::ImmutableGlobal(index));
            }
            pop_value(stack, controls, Some(global.value_type))?;
        }
        0x1a => {
            pop_value(stack, controls, None)?;
        }
        0x1b => select_result(stack, controls)?,
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    }
    Ok(())
}

/// The `0xFC`-prefixed table instructions, 12 to 17. Each names a table, and the ones that read an element
/// segment name it too. A reference operand, and the two tables of a copy, must agree on the element type.
fn table_bulk_instruction(
    sub: u32,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    match sub {
        12 => {
            let elem = reader.u32()?;
            let table = reader.u32()?;
            require_same_kind(segment_type(module, elem)?, table_type(module, table)?)?;
            for _ in 0..3 {
                pop_value(stack, controls, Some(ValueType::I32))?;
            }
        }
        13 => {
            segment_type(module, reader.u32()?)?;
        }
        14 => {
            let dst = reader.u32()?;
            let src = reader.u32()?;
            require_same_kind(table_type(module, src)?, table_type(module, dst)?)?;
            for _ in 0..3 {
                pop_value(stack, controls, Some(ValueType::I32))?;
            }
        }
        15 => {
            let element = ValueType::Ref(table_type(module, reader.u32()?)?);
            pop_value(stack, controls, Some(ValueType::I32))?;
            pop_value(stack, controls, Some(element))?;
            push_value(stack, ValueType::I32);
        }
        16 => {
            table_type(module, reader.u32()?)?;
            push_value(stack, ValueType::I32);
        }
        17 => {
            let element = ValueType::Ref(table_type(module, reader.u32()?)?);
            pop_value(stack, controls, Some(ValueType::I32))?;
            pop_value(stack, controls, Some(element))?;
            pop_value(stack, controls, Some(ValueType::I32))?;
        }
        _ => return Err(ValidationError::InvalidInstruction(0xfc)),
    }
    Ok(())
}

fn segment_type(module: &ModuleContext, index: u32) -> Result<RefType, ValidationError> {
    module
        .elem_types
        .get(index as usize)
        .copied()
        .ok_or(ValidationError::UnknownElementSegment(index))
}

fn table_type(module: &ModuleContext, index: u32) -> Result<RefType, ValidationError> {
    module
        .table_types
        .get(index as usize)
        .map(|table| table.element_type)
        .ok_or(ValidationError::UnknownTable(index))
}

fn require_same_kind(expected: RefType, actual: RefType) -> Result<(), ValidationError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ValidationError::TypeMismatch {
            expected: format!("{expected:?}"),
            actual: format!("{actual:?}"),
        })
    }
}

/// The `0xFC`-prefixed bulk memory instructions. Each names memory 0, and `memory.init` and `data.drop`
/// name a data segment, which must be below the declared count.
fn bulk_memory_instruction(
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    let sub = reader.u32()?;
    if (12..=17).contains(&sub) {
        return table_bulk_instruction(sub, reader, stack, controls, module);
    }
    if sub == 8 || sub == 9 {
        let data = reader.u32()?;
        match module.data_count {
            Some(count) if data < count => {}
            _ => return Err(ValidationError::UnknownDataSegment(data)),
        }
    }
    // Each memory operand is an index the module declares. `memory.copy` names its destination and
    // then its source, and `memory.init` names its data segment before its memory.
    match sub {
        8 | 11 => {
            memory_index(reader, module)?;
        }
        10 => {
            memory_index(reader, module)?;
            memory_index(reader, module)?;
        }
        9 => return Ok(()),
        _ => return Err(ValidationError::InvalidInstruction(0xfc)),
    }
    for _ in 0..3 {
        pop_value(stack, controls, Some(ValueType::I32))?;
    }
    Ok(())
}

fn memory_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    let (natural_alignment, value_type, stores) = match opcode {
        0x28 => (2, ValueType::I32, false),
        0x29 => (3, ValueType::I64, false),
        0x2a => (2, ValueType::F32, false),
        0x2b => (3, ValueType::F64, false),
        0x2c | 0x2d => (0, ValueType::I32, false),
        0x2e | 0x2f => (1, ValueType::I32, false),
        0x30 | 0x31 => (0, ValueType::I64, false),
        0x32 | 0x33 => (1, ValueType::I64, false),
        0x34 | 0x35 => (2, ValueType::I64, false),
        0x36 => (2, ValueType::I32, true),
        0x37 => (3, ValueType::I64, true),
        0x38 => (2, ValueType::F32, true),
        0x39 => (3, ValueType::F64, true),
        0x3a => (0, ValueType::I32, true),
        0x3b => (1, ValueType::I32, true),
        0x3c => (0, ValueType::I64, true),
        0x3d => (1, ValueType::I64, true),
        0x3e => (2, ValueType::I64, true),
        0x3f => {
            memory_index(reader, module)?;
            push_value(stack, ValueType::I32);
            return Ok(());
        }
        0x40 => {
            memory_index(reader, module)?;
            pop_value(stack, controls, Some(ValueType::I32))?;
            push_value(stack, ValueType::I32);
            return Ok(());
        }
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    };
    memory_arg(reader, module, natural_alignment)?;
    if stores {
        pop_value(stack, controls, Some(value_type))?;
        pop_value(stack, controls, Some(ValueType::I32))?;
    } else {
        pop_value(stack, controls, Some(ValueType::I32))?;
        push_value(stack, value_type);
    }
    Ok(())
}

fn call_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    match opcode {
        0x10 => {
            let index = reader.u32()?;
            let signature = function_type(index, module)?;
            pop_values(stack, controls, &signature.params.0)?;
            for value in &signature.results.0 {
                push_value(stack, *value);
            }
        }
        0x11 => {
            let type_index = reader.u32()?;
            let table_index = reader.u32()?;
            if table_index as usize >= module.table_types.len() {
                return Err(ValidationError::UnknownTable(table_index));
            }
            let signature = module
                .types
                .get(type_index as usize)
                .ok_or(ValidationError::UnknownType(type_index))?;
            pop_value(stack, controls, Some(ValueType::I32))?;
            pop_values(stack, controls, &signature.params.0)?;
            for value in &signature.results.0 {
                push_value(stack, *value);
            }
        }
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    }
    Ok(())
}

/// Read a reference type immediate: `funcref` is 0x70 and `externref` is 0x6f.
fn reference_type(reader: &mut Reader<'_>) -> Result<RefType, ValidationError> {
    match reader.byte()? {
        0x70 => Ok(RefType::FuncRef),
        0x6f => Ok(RefType::ExternRef),
        other => Err(ValidationError::InvalidBody(format!(
            "invalid reference type {other:#04x}"
        ))),
    }
}

/// Validate `ref.null`, `ref.is_null`, and `ref.func`.
fn reference_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    match opcode {
        0xd0 => {
            push_value(stack, ValueType::Ref(reference_type(reader)?));
        }
        0xd1 => {
            // Either reference kind is accepted, so only the reference-ness of
            // the operand is checked here.
            pop_reference(stack, controls)?;
            push_value(stack, ValueType::I32);
        }
        0xd2 => {
            // The index space counts imported and defined functions alike, so
            // the module context is the authority on whether it exists.
            let index = reader.u32()?;
            function_type(index, module)?;
            // The spec lets a body take a reference only to a function the module declares
            // (an export, an element segment, or a global initializer), not any function.
            if !module.declared_functions.contains(&index) {
                return Err(ValidationError::UndeclaredFunctionReference(index));
            }
            push_value(stack, ValueType::Ref(RefType::FuncRef));
        }
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    }
    Ok(())
}

/// `table.get` and `table.set`. The element type comes from the named table's type, so
/// a reference of the wrong kind is refused here rather than at run time.
fn table_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    let index = reader.u32()?;
    let table = module
        .table_types
        .get(index as usize)
        .ok_or(ValidationError::UnknownTable(index))?;
    let element = ValueType::Ref(table.element_type);
    match opcode {
        0x25 => {
            pop_value(stack, controls, Some(ValueType::I32))?;
            push_value(stack, element);
        }
        0x26 => {
            pop_value(stack, controls, Some(element))?;
            pop_value(stack, controls, Some(ValueType::I32))?;
        }
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    }
    Ok(())
}

fn constant_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
) -> Result<(), ValidationError> {
    let value = match opcode {
        0x41 => {
            let _ = reader.i32()?;
            ValueType::I32
        }
        0x42 => {
            let _ = reader.i64()?;
            ValueType::I64
        }
        0x43 => {
            let _ = reader.f32()?;
            ValueType::F32
        }
        0x44 => {
            let _ = reader.f64()?;
            ValueType::F64
        }
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    };
    push_value(stack, value);
    Ok(())
}

fn numeric_instruction(
    opcode: u8,
    stack: &mut Vec<Option<ValueType>>,
    controls: &[ControlFrame],
) -> Result<(), ValidationError> {
    let operation = match opcode {
        0x45 => ("unary", ValueType::I32, ValueType::I32),
        0x46..=0x4f => ("compare", ValueType::I32, ValueType::I32),
        0x50 => ("unary", ValueType::I64, ValueType::I32),
        0x51..=0x5a => ("compare", ValueType::I64, ValueType::I32),
        0x5b..=0x60 => ("compare", ValueType::F32, ValueType::I32),
        0x61..=0x66 => ("compare", ValueType::F64, ValueType::I32),
        0x67..=0x69 => ("unary", ValueType::I32, ValueType::I32),
        0x6a..=0x78 => ("binary", ValueType::I32, ValueType::I32),
        0x79..=0x7b => ("unary", ValueType::I64, ValueType::I64),
        0x7c..=0x8a => ("binary", ValueType::I64, ValueType::I64),
        0x8b..=0x91 => ("unary", ValueType::F32, ValueType::F32),
        0x92..=0x98 => ("binary", ValueType::F32, ValueType::F32),
        0x99..=0x9f => ("unary", ValueType::F64, ValueType::F64),
        0xa0..=0xa6 => ("binary", ValueType::F64, ValueType::F64),
        0xa7 => ("convert", ValueType::I64, ValueType::I32),
        0xa8 | 0xa9 => ("convert", ValueType::F32, ValueType::I32),
        0xaa | 0xab => ("convert", ValueType::F64, ValueType::I32),
        0xac | 0xad => ("convert", ValueType::I32, ValueType::I64),
        0xae | 0xaf => ("convert", ValueType::F32, ValueType::I64),
        0xb0 | 0xb1 => ("convert", ValueType::F64, ValueType::I64),
        0xb2 | 0xb3 => ("convert", ValueType::I32, ValueType::F32),
        0xb4 | 0xb5 => ("convert", ValueType::I64, ValueType::F32),
        0xb6 => ("convert", ValueType::F64, ValueType::F32),
        0xb7 | 0xb8 => ("convert", ValueType::I32, ValueType::F64),
        0xb9 | 0xba => ("convert", ValueType::I64, ValueType::F64),
        0xbb => ("convert", ValueType::F32, ValueType::F64),
        0xbc => ("convert", ValueType::F32, ValueType::I32),
        0xbd => ("convert", ValueType::F64, ValueType::I64),
        0xbe => ("convert", ValueType::I32, ValueType::F32),
        0xbf => ("convert", ValueType::I64, ValueType::F64),
        0xc0 | 0xc1 => ("unary", ValueType::I32, ValueType::I32),
        0xc2..=0xc4 => ("unary", ValueType::I64, ValueType::I64),
        _ => return Err(ValidationError::InvalidInstruction(opcode)),
    };
    match operation.0 {
        "unary" => {
            pop_value(stack, controls, Some(operation.1))?;
            push_value(stack, operation.2);
        }
        "binary" => {
            pop_value(stack, controls, Some(operation.1))?;
            pop_value(stack, controls, Some(operation.1))?;
            push_value(stack, operation.2);
        }
        "compare" => {
            pop_value(stack, controls, Some(operation.1))?;
            pop_value(stack, controls, Some(operation.1))?;
            push_value(stack, operation.2);
        }
        "convert" => convert(stack, controls, operation.1, operation.2)?,
        _ => unreachable!(),
    }
    Ok(())
}

pub(crate) fn validate_instruction(
    opcode: u8,
    reader: &mut Reader<'_>,
    stack: &mut Vec<Option<ValueType>>,
    controls: &mut Vec<ControlFrame>,
    locals: &[ValueType],
    module: &ModuleContext,
) -> Result<(), ValidationError> {
    match opcode {
        0x00..=0x0f => control_instruction(opcode, reader, stack, controls),
        0x10 | 0x11 => call_instruction(opcode, reader, stack, controls, module),
        0x1a | 0x1b | 0x20..=0x24 => {
            variable_instruction(opcode, reader, stack, controls, locals, module)
        }
        0x28..=0x40 => memory_instruction(opcode, reader, stack, controls, module),
        0xfc => bulk_memory_instruction(reader, stack, controls, module),
        0x41..=0x44 => constant_instruction(opcode, reader, stack),
        0xd0..=0xd2 => reference_instruction(opcode, reader, stack, controls, module),
        0x25 | 0x26 => table_instruction(opcode, reader, stack, controls, module),
        _ => numeric_instruction(opcode, stack, controls),
    }
}
