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

fn lower_function(
    function: &Function,
    function_type: &FunctionType,
    module: &tpt_wasm_format::Module,
) -> Result<IrFunction, LoweringError> {
    let (mut state, params, locals) = LoweringState::new(function, function_type)?;
    let mut reader = BodyReader::new(&function.body);
    let mut instrs = Vec::new();
    let terminator = loop {
        if reader.remaining() == 0 {
            return Err(LoweringError::UnexpectedEnd);
        }
        let opcode = reader.byte()?;
        match opcode {
            0x00 => break Terminator::Unreachable,
            0x01 => {}
            0x0b => {
                if reader.remaining() != 0 {
                    return Err(LoweringError::TrailingBytes);
                }
                break Terminator::Return(state.return_values()?);
            }
            0x0f => break Terminator::Return(state.return_values()?),
            0x10 => lower_call(reader.u32()?, module, &mut state, &mut instrs)?,
            0x1a => {
                let value = state.pop_any()?;
                instrs.push(IrInstr::Drop { value });
            }
            0x1b => lower_select(&mut state, &mut instrs)?,
            0x20 => lower_local_get(reader.u32()?, &mut state, &mut instrs)?,
            0x21 => lower_local_set(reader.u32()?, &mut state, &mut instrs)?,
            0x22 => lower_local_tee(reader.u32()?, &mut state, &mut instrs)?,
            0x41 => {
                let value = reader.i32()?;
                let result = state.allocate(ValueType::I32)?;
                instrs.push(IrInstr::ConstI32 { result, value });
                state.push(result, ValueType::I32);
            }
            0x42 => {
                let value = reader.i64()?;
                let result = state.allocate(ValueType::I64)?;
                instrs.push(IrInstr::ConstI64 { result, value });
                state.push(result, ValueType::I64);
            }
            0x43 => {
                let value = reader.f32()?;
                let result = state.allocate(ValueType::F32)?;
                instrs.push(IrInstr::ConstF32 { result, value });
                state.push(result, ValueType::F32);
            }
            0x44 => {
                let value = reader.f64()?;
                let result = state.allocate(ValueType::F64)?;
                instrs.push(IrInstr::ConstF64 { result, value });
                state.push(result, ValueType::F64);
            }
            0x45 => lower_eqz(0x45, ValueType::I32, &mut state, &mut instrs)?,
            0x46..=0x4f => lower_compare(opcode, ValueType::I32, &mut state, &mut instrs)?,
            0x50 => lower_eqz(0x50, ValueType::I64, &mut state, &mut instrs)?,
            0x51..=0x5a => lower_compare(opcode, ValueType::I64, &mut state, &mut instrs)?,
            0x5b..=0x60 => lower_float_compare(opcode, ValueType::F32, &mut state, &mut instrs)?,
            0x61..=0x66 => lower_float_compare(opcode, ValueType::F64, &mut state, &mut instrs)?,
            0x67..=0x69 => lower_unary(opcode, ValueType::I32, &mut state, &mut instrs)?,
            0x79..=0x7b => lower_unary(opcode, ValueType::I64, &mut state, &mut instrs)?,
            0xa7 | 0xac | 0xad => lower_int_conversion(opcode, &mut state, &mut instrs)?,
            0xa8..=0xab | 0xae..=0xb1 => lower_float_trunc(opcode, &mut state, &mut instrs)?,
            0xbc..=0xbf => lower_reinterpret(opcode, &mut state, &mut instrs)?,
            0xb2..=0xbb => lower_float_conversion(opcode, &mut state, &mut instrs)?,
            0x8b..=0x91 => lower_float_unary(opcode, ValueType::F32, &mut state, &mut instrs)?,
            0x99..=0x9f => lower_float_unary(opcode, ValueType::F64, &mut state, &mut instrs)?,
            0x6a..=0x78 | 0x7c..=0x8a | 0x92..=0x98 | 0xa0..=0xa6 => {
                lower_binary(opcode, &mut state, &mut instrs)?
            }
            _ => return Err(LoweringError::UnsupportedInstruction(opcode)),
        }
    };

    state.values.shrink_to_fit();
    Ok(IrFunction {
        function_type: function_type.clone(),
        params,
        locals,
        values: state.values,
        entry: BlockId(0),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            instrs,
            terminator,
        }],
    })
}

fn lower_call(
    function: u32,
    module: &tpt_wasm_format::Module,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
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
    instrs.push(IrInstr::Call {
        function,
        arguments,
        results,
    });
    Ok(())
}

fn lower_select(state: &mut LoweringState, instrs: &mut Vec<IrInstr>) -> Result<(), LoweringError> {
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
    instrs.push(IrInstr::Select {
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
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let (local, value_type) = state.local(index)?;
    let result = state.allocate(value_type)?;
    instrs.push(IrInstr::LocalGet { result, local });
    state.push(result, value_type);
    Ok(())
}

fn lower_local_set(
    index: u32,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let (local, value_type) = state.local(index)?;
    let value = state.pop(value_type)?;
    instrs.push(IrInstr::LocalSet { local, value });
    Ok(())
}

fn lower_local_tee(
    index: u32,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let (local, value_type) = state.local(index)?;
    let value = state.pop(value_type)?;
    let result = state.allocate(value_type)?;
    instrs.push(IrInstr::LocalTee {
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
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let value = state.pop(input_type)?;
    let result = state.allocate(ValueType::I32)?;
    let instruction = match input_type {
        ValueType::I32 => IrInstr::I32Eqz { result, value },
        ValueType::I64 => IrInstr::I64Eqz { result, value },
        _ => return Err(LoweringError::InvalidOpcode(opcode)),
    };
    instrs.push(instruction);
    state.push(result, ValueType::I32);
    Ok(())
}

fn lower_compare(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
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
    instrs.push(instruction);
    state.push(result, ValueType::I32);
    Ok(())
}

fn lower_float_compare(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
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
    instrs.push(instruction);
    state.push(result, ValueType::I32);
    Ok(())
}

fn lower_unary(
    opcode: u8,
    input_type: ValueType,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
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
    instrs.push(instruction);
    state.push(result, input_type);
    Ok(())
}

fn lower_int_conversion(
    opcode: u8,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let operation = IntConversion::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    instrs.push(IrInstr::IntConvert {
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
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let operation = Reinterpret::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    instrs.push(IrInstr::Reinterpret {
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
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let operation = FloatTrunc::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    instrs.push(IrInstr::FloatTrunc {
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
    instrs: &mut Vec<IrInstr>,
) -> Result<(), LoweringError> {
    let operation = FloatConversion::from_opcode(opcode)?;
    let value = state.pop(operation.source_type())?;
    let result = state.allocate(operation.result_type())?;
    instrs.push(IrInstr::FloatConvert {
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
    instrs: &mut Vec<IrInstr>,
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
    instrs.push(instruction);
    state.push(result, input_type);
    Ok(())
}

fn lower_binary(
    opcode: u8,
    state: &mut LoweringState,
    instrs: &mut Vec<IrInstr>,
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
    instrs.push(instruction);
    state.push(result, value_type);
    Ok(())
}

struct BodyReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> BodyReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
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
