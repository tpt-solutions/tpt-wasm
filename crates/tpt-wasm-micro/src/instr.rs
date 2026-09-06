// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly instruction representation for the Micro Interpreter.
//!
//! Clarity over performance. Do not optimize this enum.
//! The compiler gets its own representation in tpt-wasm-ir.

/// A WebAssembly instruction (MVP + extensions as implemented).
#[derive(Debug, Clone)]
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
}

/// Block type for control instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockType {
    Empty,
    Value(tpt_wasm_types::ValueType),
    FunctionType(u32),
}

/// Branch table for br_table.
#[derive(Debug, Clone)]
pub struct BrTable {
    pub targets: Vec<u32>,
    pub default: u32,
}

/// Memory immediate (alignment + offset).
#[derive(Debug, Clone, Copy)]
pub struct MemArg {
    pub align: u32,
    pub offset: u64,
    pub memory_index: u32,
}
