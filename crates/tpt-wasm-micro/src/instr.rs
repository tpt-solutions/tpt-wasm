// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly instruction representation for the Micro Interpreter.
//!
//! Clarity over performance. Do not optimize this enum.
//! The compiler gets its own representation in tpt-wasm-ir.

use std::fmt;

use tpt_wasm_types::{RefType, ValueType};

/// A WebAssembly instruction (MVP + extensions as implemented).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instr {
    // Control
    Unreachable,
    Nop,
    Block(BlockType),
    Loop(BlockType),
    If(BlockType),
    Else,
    End,
    Br(u32),
    BrIf(u32),
    BrTable(BrTable),
    Return,
    Call(u32),
    CallIndirect(u32, u32),

    // Reference
    RefNull(tpt_wasm_types::RefType),
    RefIsNull,
    RefFunc(u32),

    // Parametric
    Drop,
    Select,

    // Variable
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),

    // Memory
    I32Load(MemArg),
    I64Load(MemArg),
    F32Load(MemArg),
    F64Load(MemArg),
    I32Load8s(MemArg),
    I32Load8u(MemArg),
    I32Load16s(MemArg),
    I32Load16u(MemArg),
    I64Load8s(MemArg),
    I64Load8u(MemArg),
    I64Load16s(MemArg),
    I64Load16u(MemArg),
    I64Load32s(MemArg),
    I64Load32u(MemArg),
    I32Store(MemArg),
    I64Store(MemArg),
    F32Store(MemArg),
    F64Store(MemArg),
    I32Store8(MemArg),
    I32Store16(MemArg),
    I64Store8(MemArg),
    I64Store16(MemArg),
    I64Store32(MemArg),
    MemorySize(u32),
    MemoryGrow(u32),

    // Numeric constants
    I32Const(i32),
    I64Const(i64),
    F32Const(u32),
    F64Const(u64),

    // i32 operations
    I32Eqz,
    I32Eq,
    I32Ne,
    I32LtS,
    I32LtU,
    I32GtS,
    I32GtU,
    I32LeS,
    I32LeU,
    I32GeS,
    I32GeU,
    I32Clz,
    I32Ctz,
    I32Popcnt,
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

    // i64 operations
    I64Eqz,
    I64Eq,
    I64Ne,
    I64LtS,
    I64LtU,
    I64GtS,
    I64GtU,
    I64LeS,
    I64LeU,
    I64GeS,
    I64GeU,
    I64Clz,
    I64Ctz,
    I64Popcnt,
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

    // f32 operations
    F32Eq,
    F32Ne,
    F32Lt,
    F32Gt,
    F32Le,
    F32Ge,
    F32Abs,
    F32Neg,
    F32Ceil,
    F32Floor,
    F32Trunc,
    F32Nearest,
    F32Sqrt,
    F32Add,
    F32Sub,
    F32Mul,
    F32Div,
    F32Min,
    F32Max,
    F32Copysign,

    // f64 operations
    F64Eq,
    F64Ne,
    F64Lt,
    F64Gt,
    F64Le,
    F64Ge,
    F64Abs,
    F64Neg,
    F64Ceil,
    F64Floor,
    F64Trunc,
    F64Nearest,
    F64Sqrt,
    F64Add,
    F64Sub,
    F64Mul,
    F64Div,
    F64Min,
    F64Max,
    F64Copysign,

    // Conversions
    I32WrapI64,
    I32TruncF32S,
    I32TruncF32U,
    I32TruncF64S,
    I32TruncF64U,
    I64ExtendI32S,
    I64ExtendI32U,
    I64TruncF32S,
    I64TruncF32U,
    I64TruncF64S,
    I64TruncF64U,
    F32ConvertI32S,
    F32ConvertI32U,
    F32ConvertI64S,
    F32ConvertI64U,
    F32DemoteF64,
    F64ConvertI32S,
    F64ConvertI32U,
    F64ConvertI64S,
    F64ConvertI64U,
    F64PromoteF32,
    I32ReinterpretF32,
    I64ReinterpretF64,
    F32ReinterpretI32,
    F64ReinterpretI64,
    I32Extend8S,
    I32Extend16S,
    I64Extend8S,
    I64Extend16S,
    I64Extend32S,
}

/// Block type for control instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockType {
    Empty,
    Value(tpt_wasm_types::ValueType),
    FunctionType(u32),
}

/// Branch table for br_table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrTable {
    pub targets: Vec<u32>,
    pub default: u32,
}

/// Memory immediate (alignment + offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemArg {
    pub align: u32,
    pub offset: u64,
    pub memory_index: u32,
}

/// Errors produced while decoding a function body into [`Instr`] values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstrDecodeError {
    UnexpectedEof,
    InvalidLeb128,
    InvalidOpcode(u8),
    InvalidBlockType,
    InvalidValueType(u8),
    InvalidReferenceType(u8),
    InvalidMemoryIndex(u8),
    TrailingBytes,
}

impl fmt::Display for InstrDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for InstrDecodeError {}

/// Decode a function body into the Micro interpreter's explicit instruction list.
///
/// This performs binary-immediate parsing only. Type and index validation belongs
/// to `tpt-wasm-validate`, before a function is instantiated.
pub fn decode_body(bytes: &[u8]) -> Result<Vec<Instr>, InstrDecodeError> {
    let mut reader = BodyReader::new(bytes);
    let mut instructions = Vec::new();
    while reader.remaining() > 0 {
        instructions.push(decode_instruction(&mut reader)?);
    }
    Ok(instructions)
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

    fn byte(&mut self) -> Result<u8, InstrDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(InstrDecodeError::UnexpectedEof)?;
        self.position += 1;
        Ok(value)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], InstrDecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(InstrDecodeError::UnexpectedEof)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(InstrDecodeError::UnexpectedEof)?;
        self.position = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, InstrDecodeError> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            let payload = u32::from(byte & 0x7f);
            if shift == 28 && payload > 0x0f {
                return Err(InstrDecodeError::InvalidLeb128);
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(InstrDecodeError::InvalidLeb128)
    }

    fn i32(&mut self) -> Result<i32, InstrDecodeError> {
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
                        return Err(InstrDecodeError::InvalidLeb128);
                    }
                } else if shift < 32 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result as i32);
            }
        }
        Err(InstrDecodeError::InvalidLeb128)
    }

    fn i64(&mut self) -> Result<i64, InstrDecodeError> {
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
                        return Err(InstrDecodeError::InvalidLeb128);
                    }
                } else if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i128 << shift;
                }
                return Ok(result as i64);
            }
        }
        Err(InstrDecodeError::InvalidLeb128)
    }
}

fn decode_instruction(reader: &mut BodyReader<'_>) -> Result<Instr, InstrDecodeError> {
    let opcode = reader.byte()?;
    let instruction = match opcode {
        0x00 => Instr::Unreachable,
        0x01 => Instr::Nop,
        0x02 => Instr::Block(decode_block_type(reader)?),
        0x03 => Instr::Loop(decode_block_type(reader)?),
        0x04 => Instr::If(decode_block_type(reader)?),
        0x05 => Instr::Else,
        0x0b => Instr::End,
        0x0c => Instr::Br(reader.u32()?),
        0x0d => Instr::BrIf(reader.u32()?),
        0x0e => {
            let count = reader.u32()?;
            if count as usize > reader.remaining() {
                return Err(InstrDecodeError::UnexpectedEof);
            }
            let mut targets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                targets.push(reader.u32()?);
            }
            Instr::BrTable(BrTable {
                targets,
                default: reader.u32()?,
            })
        }
        0x0f => Instr::Return,
        0x10 => Instr::Call(reader.u32()?),
        0x11 => Instr::CallIndirect(reader.u32()?, reader.u32()?),
        0xd0 => Instr::RefNull(decode_ref_type(reader.byte()?)?),
        0xd1 => Instr::RefIsNull,
        0xd2 => Instr::RefFunc(reader.u32()?),
        0x1a => Instr::Drop,
        0x1b => Instr::Select,
        0x20 => Instr::LocalGet(reader.u32()?),
        0x21 => Instr::LocalSet(reader.u32()?),
        0x22 => Instr::LocalTee(reader.u32()?),
        0x23 => Instr::GlobalGet(reader.u32()?),
        0x24 => Instr::GlobalSet(reader.u32()?),
        0x28..=0x35 => decode_load(opcode, reader)?,
        0x36..=0x3e => decode_store(opcode, reader)?,
        0x3f => {
            let index = reader.byte()?;
            if index != 0 {
                return Err(InstrDecodeError::InvalidMemoryIndex(index));
            }
            Instr::MemorySize(0)
        }
        0x40 => {
            let index = reader.byte()?;
            if index != 0 {
                return Err(InstrDecodeError::InvalidMemoryIndex(index));
            }
            Instr::MemoryGrow(0)
        }
        0x41 => Instr::I32Const(reader.i32()?),
        0x42 => Instr::I64Const(reader.i64()?),
        0x43 => {
            let bytes = reader.take(4)?;
            let mut value = [0; 4];
            value.copy_from_slice(bytes);
            Instr::F32Const(u32::from_le_bytes(value))
        }
        0x44 => {
            let bytes = reader.take(8)?;
            let mut value = [0; 8];
            value.copy_from_slice(bytes);
            Instr::F64Const(u64::from_le_bytes(value))
        }
        _ => decode_numeric(opcode)?,
    };
    Ok(instruction)
}

fn decode_block_type(reader: &mut BodyReader<'_>) -> Result<BlockType, InstrDecodeError> {
    match reader.byte()? {
        0x40 => Ok(BlockType::Empty),
        0x7f => Ok(BlockType::Value(ValueType::I32)),
        0x7e => Ok(BlockType::Value(ValueType::I64)),
        0x7d => Ok(BlockType::Value(ValueType::F32)),
        0x7c => Ok(BlockType::Value(ValueType::F64)),
        // A block may carry a reference result, which is what lets a `funcref`
        // travel through a block's label.
        0x70 => Ok(BlockType::Value(ValueType::Ref(RefType::FuncRef))),
        0x6f => Ok(BlockType::Value(ValueType::Ref(RefType::ExternRef))),
        _ => Err(InstrDecodeError::InvalidBlockType),
    }
}

fn decode_ref_type(byte: u8) -> Result<RefType, InstrDecodeError> {
    match byte {
        0x70 => Ok(RefType::FuncRef),
        0x6f => Ok(RefType::ExternRef),
        _ => Err(InstrDecodeError::InvalidReferenceType(byte)),
    }
}

fn decode_mem_arg(reader: &mut BodyReader<'_>) -> Result<MemArg, InstrDecodeError> {
    Ok(MemArg {
        align: reader.u32()?,
        offset: u64::from(reader.u32()?),
        memory_index: 0,
    })
}

fn decode_load(opcode: u8, reader: &mut BodyReader<'_>) -> Result<Instr, InstrDecodeError> {
    let arg = decode_mem_arg(reader)?;
    Ok(match opcode {
        0x28 => Instr::I32Load(arg),
        0x29 => Instr::I64Load(arg),
        0x2a => Instr::F32Load(arg),
        0x2b => Instr::F64Load(arg),
        0x2c => Instr::I32Load8s(arg),
        0x2d => Instr::I32Load8u(arg),
        0x2e => Instr::I32Load16s(arg),
        0x2f => Instr::I32Load16u(arg),
        0x30 => Instr::I64Load8s(arg),
        0x31 => Instr::I64Load8u(arg),
        0x32 => Instr::I64Load16s(arg),
        0x33 => Instr::I64Load16u(arg),
        0x34 => Instr::I64Load32s(arg),
        0x35 => Instr::I64Load32u(arg),
        _ => return Err(InstrDecodeError::InvalidOpcode(opcode)),
    })
}

fn decode_store(opcode: u8, reader: &mut BodyReader<'_>) -> Result<Instr, InstrDecodeError> {
    let arg = decode_mem_arg(reader)?;
    Ok(match opcode {
        0x36 => Instr::I32Store(arg),
        0x37 => Instr::I64Store(arg),
        0x38 => Instr::F32Store(arg),
        0x39 => Instr::F64Store(arg),
        0x3a => Instr::I32Store8(arg),
        0x3b => Instr::I32Store16(arg),
        0x3c => Instr::I64Store8(arg),
        0x3d => Instr::I64Store16(arg),
        0x3e => Instr::I64Store32(arg),
        _ => return Err(InstrDecodeError::InvalidOpcode(opcode)),
    })
}

fn decode_numeric(opcode: u8) -> Result<Instr, InstrDecodeError> {
    let instruction = match opcode {
        0x45 => Instr::I32Eqz,
        0x46 => Instr::I32Eq,
        0x47 => Instr::I32Ne,
        0x48 => Instr::I32LtS,
        0x49 => Instr::I32LtU,
        0x4a => Instr::I32GtS,
        0x4b => Instr::I32GtU,
        0x4c => Instr::I32LeS,
        0x4d => Instr::I32LeU,
        0x4e => Instr::I32GeS,
        0x4f => Instr::I32GeU,
        0x50 => Instr::I64Eqz,
        0x51 => Instr::I64Eq,
        0x52 => Instr::I64Ne,
        0x53 => Instr::I64LtS,
        0x54 => Instr::I64LtU,
        0x55 => Instr::I64GtS,
        0x56 => Instr::I64GtU,
        0x57 => Instr::I64LeS,
        0x58 => Instr::I64LeU,
        0x59 => Instr::I64GeS,
        0x5a => Instr::I64GeU,
        0x5b => Instr::F32Eq,
        0x5c => Instr::F32Ne,
        0x5d => Instr::F32Lt,
        0x5e => Instr::F32Gt,
        0x5f => Instr::F32Le,
        0x60 => Instr::F32Ge,
        0x61 => Instr::F64Eq,
        0x62 => Instr::F64Ne,
        0x63 => Instr::F64Lt,
        0x64 => Instr::F64Gt,
        0x65 => Instr::F64Le,
        0x66 => Instr::F64Ge,
        0x67 => Instr::I32Clz,
        0x68 => Instr::I32Ctz,
        0x69 => Instr::I32Popcnt,
        0x6a => Instr::I32Add,
        0x6b => Instr::I32Sub,
        0x6c => Instr::I32Mul,
        0x6d => Instr::I32DivS,
        0x6e => Instr::I32DivU,
        0x6f => Instr::I32RemS,
        0x70 => Instr::I32RemU,
        0x71 => Instr::I32And,
        0x72 => Instr::I32Or,
        0x73 => Instr::I32Xor,
        0x74 => Instr::I32Shl,
        0x75 => Instr::I32ShrS,
        0x76 => Instr::I32ShrU,
        0x77 => Instr::I32Rotl,
        0x78 => Instr::I32Rotr,
        0x79 => Instr::I64Clz,
        0x7a => Instr::I64Ctz,
        0x7b => Instr::I64Popcnt,
        0x7c => Instr::I64Add,
        0x7d => Instr::I64Sub,
        0x7e => Instr::I64Mul,
        0x7f => Instr::I64DivS,
        0x80 => Instr::I64DivU,
        0x81 => Instr::I64RemS,
        0x82 => Instr::I64RemU,
        0x83 => Instr::I64And,
        0x84 => Instr::I64Or,
        0x85 => Instr::I64Xor,
        0x86 => Instr::I64Shl,
        0x87 => Instr::I64ShrS,
        0x88 => Instr::I64ShrU,
        0x89 => Instr::I64Rotl,
        0x8a => Instr::I64Rotr,
        0x8b => Instr::F32Abs,
        0x8c => Instr::F32Neg,
        0x8d => Instr::F32Ceil,
        0x8e => Instr::F32Floor,
        0x8f => Instr::F32Trunc,
        0x90 => Instr::F32Nearest,
        0x91 => Instr::F32Sqrt,
        0x92 => Instr::F32Add,
        0x93 => Instr::F32Sub,
        0x94 => Instr::F32Mul,
        0x95 => Instr::F32Div,
        0x96 => Instr::F32Min,
        0x97 => Instr::F32Max,
        0x98 => Instr::F32Copysign,
        0x99 => Instr::F64Abs,
        0x9a => Instr::F64Neg,
        0x9b => Instr::F64Ceil,
        0x9c => Instr::F64Floor,
        0x9d => Instr::F64Trunc,
        0x9e => Instr::F64Nearest,
        0x9f => Instr::F64Sqrt,
        0xa0 => Instr::F64Add,
        0xa1 => Instr::F64Sub,
        0xa2 => Instr::F64Mul,
        0xa3 => Instr::F64Div,
        0xa4 => Instr::F64Min,
        0xa5 => Instr::F64Max,
        0xa6 => Instr::F64Copysign,
        0xa7 => Instr::I32WrapI64,
        0xa8 => Instr::I32TruncF32S,
        0xa9 => Instr::I32TruncF32U,
        0xaa => Instr::I32TruncF64S,
        0xab => Instr::I32TruncF64U,
        0xac => Instr::I64ExtendI32S,
        0xad => Instr::I64ExtendI32U,
        0xae => Instr::I64TruncF32S,
        0xaf => Instr::I64TruncF32U,
        0xb0 => Instr::I64TruncF64S,
        0xb1 => Instr::I64TruncF64U,
        0xb2 => Instr::F32ConvertI32S,
        0xb3 => Instr::F32ConvertI32U,
        0xb4 => Instr::F32ConvertI64S,
        0xb5 => Instr::F32ConvertI64U,
        0xb6 => Instr::F32DemoteF64,
        0xb7 => Instr::F64ConvertI32S,
        0xb8 => Instr::F64ConvertI32U,
        0xb9 => Instr::F64ConvertI64S,
        0xba => Instr::F64ConvertI64U,
        0xbb => Instr::F64PromoteF32,
        0xbc => Instr::I32ReinterpretF32,
        0xbd => Instr::I64ReinterpretF64,
        0xbe => Instr::F32ReinterpretI32,
        0xbf => Instr::F64ReinterpretI64,
        0xc0 => Instr::I32Extend8S,
        0xc1 => Instr::I32Extend16S,
        0xc2 => Instr::I64Extend8S,
        0xc3 => Instr::I64Extend16S,
        0xc4 => Instr::I64Extend32S,
        _ => return Err(InstrDecodeError::InvalidOpcode(opcode)),
    };
    Ok(instruction)
}

#[cfg(test)]
mod tests {
    use super::{decode_body, BlockType, BrTable, Instr, InstrDecodeError, MemArg};
    use tpt_wasm_types::{RefType, ValueType};

    #[test]
    fn decodes_constants_control_and_memory_immediates() {
        let body = [0x02, 0x7f, 0x41, 0x2a, 0x0b, 0x28, 0x02, 0x08, 0x0b];
        assert_eq!(
            decode_body(&body).unwrap(),
            vec![
                Instr::Block(BlockType::Value(ValueType::I32)),
                Instr::I32Const(42),
                Instr::End,
                Instr::I32Load(MemArg {
                    align: 2,
                    offset: 8,
                    memory_index: 0
                }),
                Instr::End,
            ]
        );
    }

    #[test]
    fn decodes_branch_table_and_reference_immediates() {
        let body = [0x0e, 0x01, 0x00, 0x01, 0xd0, 0x70, 0xd2, 0x03];
        assert_eq!(
            decode_body(&body).unwrap(),
            vec![
                Instr::BrTable(BrTable {
                    targets: vec![0],
                    default: 1
                }),
                Instr::RefNull(RefType::FuncRef),
                Instr::RefFunc(3),
            ]
        );
    }

    #[test]
    fn rejects_truncated_and_unknown_instructions() {
        assert_eq!(decode_body(&[0x41]), Err(InstrDecodeError::UnexpectedEof));
        assert_eq!(
            decode_body(&[0xff]),
            Err(InstrDecodeError::InvalidOpcode(0xff))
        );
        assert_eq!(
            decode_body(&[0x02, 0x7e]),
            Ok(vec![Instr::Block(BlockType::Value(ValueType::I64))])
        );
    }
}
