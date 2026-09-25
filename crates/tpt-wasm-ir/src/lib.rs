// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! TPT intermediate representation.
//!
//! The IR is deliberately independent of the Micro Interpreter.
//!
//! Wasm → ValidatedWasm → TPT-Wasm IR → { baseline, optimizing, verification }
//!
//! The implemented lowering slice is deliberately conservative: it accepts
//! straight-line code with stable local slots and defined direct calls, and
//! rejects constructs that are not yet represented by this IR version.

use tpt_wasm_types::{FunctionType, ReferenceType, Trap, Value, ValueType};

mod lower;
#[cfg(test)]
mod tests;
mod verify;

pub use lower::{const_expr_i32, lower_module, LoweringError};
pub use verify::{
    lower_and_verify, verify_module, IrVerificationCertificate, LowerAndVerifyError,
    VerificationError, VerifiedIrModule,
};

/// A lowered module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IrModule {
    pub functions: Vec<IrFunction>,
    /// Module-level globals, in index order. Keeping them here lets the verifier
    /// check a `global.get`/`global.set` against the declaration without the
    /// original Wasm module.
    pub globals: Vec<IrGlobal>,
    /// The single linear memory, if the module declares one. MVP allows at most
    /// one memory, and every memory instruction addresses memory 0.
    pub memory: Option<IrMemory>,
    /// Tables, in index order. MVP allows at most one table.
    pub tables: Vec<IrTable>,
    /// The module's function types, in index order. A `call_indirect` names one
    /// of these rather than a function, so the signature is checked against the
    /// table entry's type at run time.
    pub types: Vec<FunctionType>,
}

/// One module-level table declaration and its initial contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrTable {
    /// MVP tables hold function references.
    pub element_type: ReferenceType,
    pub min: u64,
    pub max: Option<u64>,
    /// Function indices from the module's active element segment.
    pub elements: Vec<u32>,
    /// Where those function indices start in the table.
    pub offset: u32,
}

/// One module-level global declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrGlobal {
    pub value_type: ValueType,
    pub mutable: bool,
    /// The value the global starts with, from its constant initializer.
    pub init: Value,
}

/// The module's linear memory declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrMemory {
    /// Initial size in 64 KiB pages.
    pub min_pages: u64,
    /// Declared maximum, or `None` when the module sets no maximum.
    pub max_pages: Option<u64>,
    /// Initial contents, in the order the active data segments write them.
    ///
    /// Applied at instantiation, before any function runs, so a later segment
    /// overwrites an earlier one at the same address.
    pub segments: Vec<IrDataSegment>,
}

/// One active data segment: bytes written at `offset` when the module starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrDataSegment {
    pub offset: u32,
    pub bytes: Vec<u8>,
}

/// A function in the TPT IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrFunction {
    pub function_type: FunctionType,
    pub params: Vec<ValueId>,
    pub locals: Vec<ValueId>,
    pub values: Vec<IrValue>,
    pub entry: BlockId,
    pub blocks: Vec<BasicBlock>,
}

/// A typed SSA-ready value definition or entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrValue {
    pub id: ValueId,
    pub value_type: ValueType,
}

/// A basic block in the control flow graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub id: BlockId,
    /// Values entering this block from a predecessor, in the order the
    /// predecessor supplies them. This is the block-parameter form of a phi.
    pub params: Vec<ValueId>,
    pub instrs: Vec<IrInstr>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId(pub u32);

/// The typed instruction subset implemented by the first lowering pass.
///
/// Every instruction that produces a value names that value explicitly. The
/// lowering pass supplies an SSA definition; local variables are represented by
/// their stable value IDs until a later control-flow lowering pass introduces
/// block parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrInstr {
    ConstI32 {
        result: ValueId,
        value: i32,
    },
    ConstI64 {
        result: ValueId,
        value: i64,
    },
    ConstF32 {
        result: ValueId,
        value: u32,
    },
    ConstF64 {
        result: ValueId,
        value: u64,
    },
    Drop {
        value: ValueId,
    },
    Select {
        result: ValueId,
        condition: ValueId,
        left: ValueId,
        right: ValueId,
    },
    LocalGet {
        result: ValueId,
        local: ValueId,
    },
    LocalSet {
        local: ValueId,
        value: ValueId,
    },
    LocalTee {
        result: ValueId,
        local: ValueId,
        value: ValueId,
    },
    I32Add {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32Sub {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32Mul {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32DivS {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32DivU {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32RemS {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32RemU {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32And {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32Or {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32Xor {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32Shl {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32ShrS {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32ShrU {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32Rotl {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I32Rotr {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Add {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Sub {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Mul {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64DivS {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64DivU {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64RemS {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64RemU {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64And {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Or {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Xor {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Shl {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64ShrS {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64ShrU {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Rotl {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    I64Rotr {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Add {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Sub {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Mul {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Div {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F64Add {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F64Sub {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F64Mul {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F64Div {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Min {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Max {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Copysign {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F64Min {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F64Max {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F64Copysign {
        result: ValueId,
        left: ValueId,
        right: ValueId,
    },
    F32Compare {
        result: ValueId,
        left: ValueId,
        right: ValueId,
        comparison: FloatComparison,
    },
    F64Compare {
        result: ValueId,
        left: ValueId,
        right: ValueId,
        comparison: FloatComparison,
    },
    I32Eqz {
        result: ValueId,
        value: ValueId,
    },
    I32Compare {
        result: ValueId,
        left: ValueId,
        right: ValueId,
        comparison: IntComparison,
    },
    I64Eqz {
        result: ValueId,
        value: ValueId,
    },
    I64Compare {
        result: ValueId,
        left: ValueId,
        right: ValueId,
        comparison: IntComparison,
    },
    I32Unary {
        result: ValueId,
        value: ValueId,
        operation: IntUnary,
    },
    I64Unary {
        result: ValueId,
        value: ValueId,
        operation: IntUnary,
    },
    F32Unary {
        result: ValueId,
        value: ValueId,
        operation: FloatUnary,
    },
    F64Unary {
        result: ValueId,
        value: ValueId,
        operation: FloatUnary,
    },
    IntConvert {
        result: ValueId,
        value: ValueId,
        operation: IntConversion,
    },
    Reinterpret {
        result: ValueId,
        value: ValueId,
        operation: Reinterpret,
    },
    FloatConvert {
        result: ValueId,
        value: ValueId,
        operation: FloatConversion,
    },
    FloatTrunc {
        result: ValueId,
        value: ValueId,
        operation: FloatTrunc,
    },
    Call {
        function: u32,
        arguments: Vec<ValueId>,
        results: Vec<ValueId>,
    },
    /// Read `width` bytes little-endian from `address + offset`.
    Load {
        result: ValueId,
        address: ValueId,
        offset: u32,
        operation: MemoryLoad,
    },
    /// Write the low `width` bytes of `value` at `address + offset`.
    Store {
        address: ValueId,
        value: ValueId,
        offset: u32,
        operation: MemoryStore,
    },
    /// Current size of memory 0, in pages.
    MemorySize {
        result: ValueId,
    },
    /// Grow memory 0 by `delta` pages, yielding the previous size or -1.
    MemoryGrow {
        result: ValueId,
        delta: ValueId,
    },
    GlobalGet {
        result: ValueId,
        global: u32,
    },
    GlobalSet {
        global: u32,
        value: ValueId,
    },
    /// Indirect call through table 0, dispatching on the function's type.
    CallIndirect {
        /// The expected signature, as a module type index.
        type_index: u32,
        /// The table the index operand reads.
        table: u32,
        /// The `i32` table index, the last operand popped.
        operand: ValueId,
        arguments: Vec<ValueId>,
        results: Vec<ValueId>,
    },
}

/// The width, signedness, and result type of one Wasm memory load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLoad {
    /// `i32.load`: 4 bytes, sign-extended into the low 32 bits of an `i32`.
    I32,
    /// `i64.load`: 8 bytes, a full `i64`.
    I64,
    /// `f32.load`: 4 bytes, raw bits reinterpreted as `f32`.
    F32,
    /// `f64.load`: 8 bytes, raw bits reinterpreted as `f64`.
    F64,
    I32_8S,
    I32_8U,
    I32_16S,
    I32_16U,
    I64_8S,
    I64_8U,
    I64_16S,
    I64_16U,
    I64_32S,
    I64_32U,
}

impl MemoryLoad {
    /// Bytes touched by this load.
    pub fn width(self) -> u32 {
        match self {
            MemoryLoad::I32 | MemoryLoad::F32 | MemoryLoad::I64_32S | MemoryLoad::I64_32U => 4,
            MemoryLoad::I64 | MemoryLoad::F64 => 8,
            MemoryLoad::I32_8S | MemoryLoad::I32_8U | MemoryLoad::I64_8S | MemoryLoad::I64_8U => 1,
            MemoryLoad::I32_16S
            | MemoryLoad::I32_16U
            | MemoryLoad::I64_16S
            | MemoryLoad::I64_16U => 2,
        }
    }

    /// The value type this load produces.
    pub fn result_type(self) -> ValueType {
        match self {
            MemoryLoad::I32
            | MemoryLoad::I32_8S
            | MemoryLoad::I32_8U
            | MemoryLoad::I32_16S
            | MemoryLoad::I32_16U => ValueType::I32,
            MemoryLoad::F32 => ValueType::F32,
            MemoryLoad::F64 => ValueType::F64,
            MemoryLoad::I64
            | MemoryLoad::I64_8S
            | MemoryLoad::I64_8U
            | MemoryLoad::I64_16S
            | MemoryLoad::I64_16U
            | MemoryLoad::I64_32S
            | MemoryLoad::I64_32U => ValueType::I64,
        }
    }
}

/// Which bytes of a value one Wasm memory store writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryStore {
    I32,
    I64,
    F32,
    F64,
    I32_8,
    I32_16,
    I64_8,
    I64_16,
    I64_32,
}

impl MemoryStore {
    /// Bytes written by this store.
    pub fn width(self) -> u32 {
        match self {
            MemoryStore::I32 | MemoryStore::F32 | MemoryStore::I64_32 => 4,
            MemoryStore::I64 | MemoryStore::F64 => 8,
            MemoryStore::I32_8 | MemoryStore::I64_8 => 1,
            MemoryStore::I32_16 | MemoryStore::I64_16 => 2,
        }
    }

    /// The value type this store consumes.
    pub fn operand_type(self) -> ValueType {
        match self {
            MemoryStore::I32 | MemoryStore::I32_8 | MemoryStore::I32_16 => ValueType::I32,
            MemoryStore::F32 => ValueType::F32,
            MemoryStore::F64 => ValueType::F64,
            MemoryStore::I64 | MemoryStore::I64_8 | MemoryStore::I64_16 | MemoryStore::I64_32 => {
                ValueType::I64
            }
        }
    }
}

/// A signed or unsigned integer comparison operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntComparison {
    Eq,
    Ne,
    LtS,
    LtU,
    GtS,
    GtU,
    LeS,
    LeU,
    GeS,
    GeU,
}

impl IntComparison {
    fn from_i32_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0x46 => Ok(Self::Eq),
            0x47 => Ok(Self::Ne),
            0x48 => Ok(Self::LtS),
            0x49 => Ok(Self::LtU),
            0x4a => Ok(Self::GtS),
            0x4b => Ok(Self::GtU),
            0x4c => Ok(Self::LeS),
            0x4d => Ok(Self::LeU),
            0x4e => Ok(Self::GeS),
            0x4f => Ok(Self::GeU),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }

    fn from_i64_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0x51 => Ok(Self::Eq),
            0x52 => Ok(Self::Ne),
            0x53 => Ok(Self::LtS),
            0x54 => Ok(Self::LtU),
            0x55 => Ok(Self::GtS),
            0x56 => Ok(Self::GtU),
            0x57 => Ok(Self::LeS),
            0x58 => Ok(Self::LeU),
            0x59 => Ok(Self::GeS),
            0x5a => Ok(Self::GeU),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatComparison {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl FloatComparison {
    fn from_f32_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0x5b => Ok(Self::Eq),
            0x5c => Ok(Self::Ne),
            0x5d => Ok(Self::Lt),
            0x5e => Ok(Self::Gt),
            0x5f => Ok(Self::Le),
            0x60 => Ok(Self::Ge),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }

    fn from_f64_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0x61 => Ok(Self::Eq),
            0x62 => Ok(Self::Ne),
            0x63 => Ok(Self::Lt),
            0x64 => Ok(Self::Gt),
            0x65 => Ok(Self::Le),
            0x66 => Ok(Self::Ge),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }
}

/// A unary integer bit-count operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntUnary {
    Clz,
    Ctz,
    Popcnt,
}

impl IntUnary {
    fn from_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0x67 | 0x79 => Ok(Self::Clz),
            0x68 | 0x7a => Ok(Self::Ctz),
            0x69 | 0x7b => Ok(Self::Popcnt),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }
}

/// A non-trapping integer width or signedness conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntConversion {
    I32WrapI64,
    I64ExtendI32S,
    I64ExtendI32U,
}

impl IntConversion {
    fn from_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0xa7 => Ok(Self::I32WrapI64),
            0xac => Ok(Self::I64ExtendI32S),
            0xad => Ok(Self::I64ExtendI32U),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }

    fn source_type(self) -> ValueType {
        match self {
            Self::I32WrapI64 => ValueType::I64,
            Self::I64ExtendI32S | Self::I64ExtendI32U => ValueType::I32,
        }
    }

    fn result_type(self) -> ValueType {
        match self {
            Self::I32WrapI64 => ValueType::I32,
            Self::I64ExtendI32S | Self::I64ExtendI32U => ValueType::I64,
        }
    }
}

/// A non-trapping raw-bit reinterpretation between same-width numeric types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reinterpret {
    I32FromF32,
    I64FromF64,
    F32FromI32,
    F64FromI64,
}

impl Reinterpret {
    fn from_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0xbc => Ok(Self::I32FromF32),
            0xbd => Ok(Self::I64FromF64),
            0xbe => Ok(Self::F32FromI32),
            0xbf => Ok(Self::F64FromI64),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }

    fn source_type(self) -> ValueType {
        match self {
            Self::I32FromF32 => ValueType::F32,
            Self::I64FromF64 => ValueType::F64,
            Self::F32FromI32 => ValueType::I32,
            Self::F64FromI64 => ValueType::I64,
        }
    }

    fn result_type(self) -> ValueType {
        match self {
            Self::I32FromF32 => ValueType::I32,
            Self::I64FromF64 => ValueType::I64,
            Self::F32FromI32 => ValueType::F32,
            Self::F64FromI64 => ValueType::F64,
        }
    }
}

/// A non-trapping IEEE 754 conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatConversion {
    F32FromI32S,
    F32FromI32U,
    F32FromI64S,
    F32FromI64U,
    F32FromF64,
    F64FromI32S,
    F64FromI32U,
    F64FromI64S,
    F64FromI64U,
    F64FromF32,
}

impl FloatConversion {
    fn from_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0xb2 => Ok(Self::F32FromI32S),
            0xb3 => Ok(Self::F32FromI32U),
            0xb4 => Ok(Self::F32FromI64S),
            0xb5 => Ok(Self::F32FromI64U),
            0xb6 => Ok(Self::F32FromF64),
            0xb7 => Ok(Self::F64FromI32S),
            0xb8 => Ok(Self::F64FromI32U),
            0xb9 => Ok(Self::F64FromI64S),
            0xba => Ok(Self::F64FromI64U),
            0xbb => Ok(Self::F64FromF32),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }

    fn source_type(self) -> ValueType {
        match self {
            Self::F32FromI32S | Self::F32FromI32U => ValueType::I32,
            Self::F32FromI64S | Self::F32FromI64U => ValueType::I64,
            Self::F32FromF64 => ValueType::F64,
            Self::F64FromI32S | Self::F64FromI32U => ValueType::I32,
            Self::F64FromI64S | Self::F64FromI64U => ValueType::I64,
            Self::F64FromF32 => ValueType::F32,
        }
    }

    fn result_type(self) -> ValueType {
        match self {
            Self::F32FromI32S
            | Self::F32FromI32U
            | Self::F32FromI64S
            | Self::F32FromI64U
            | Self::F32FromF64 => ValueType::F32,
            Self::F64FromI32S
            | Self::F64FromI32U
            | Self::F64FromI64S
            | Self::F64FromI64U
            | Self::F64FromF32 => ValueType::F64,
        }
    }
}

/// A trapping conversion from floating point to an integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatTrunc {
    I32FromF32S,
    I32FromF32U,
    I32FromF64S,
    I32FromF64U,
    I64FromF32S,
    I64FromF32U,
    I64FromF64S,
    I64FromF64U,
}

impl FloatTrunc {
    fn from_opcode(opcode: u8) -> Result<Self, LoweringError> {
        match opcode {
            0xa8 => Ok(Self::I32FromF32S),
            0xa9 => Ok(Self::I32FromF32U),
            0xaa => Ok(Self::I32FromF64S),
            0xab => Ok(Self::I32FromF64U),
            0xae => Ok(Self::I64FromF32S),
            0xaf => Ok(Self::I64FromF32U),
            0xb0 => Ok(Self::I64FromF64S),
            0xb1 => Ok(Self::I64FromF64U),
            _ => Err(LoweringError::InvalidOpcode(opcode)),
        }
    }

    fn source_type(self) -> ValueType {
        match self {
            Self::I32FromF32S | Self::I32FromF32U | Self::I64FromF32S | Self::I64FromF32U => {
                ValueType::F32
            }
            Self::I32FromF64S | Self::I32FromF64U | Self::I64FromF64S | Self::I64FromF64U => {
                ValueType::F64
            }
        }
    }

    fn result_type(self) -> ValueType {
        match self {
            Self::I32FromF32S | Self::I32FromF32U | Self::I32FromF64S | Self::I32FromF64U => {
                ValueType::I32
            }
            Self::I64FromF32S | Self::I64FromF32U | Self::I64FromF64S | Self::I64FromF64U => {
                ValueType::I64
            }
        }
    }
}

/// A unary IEEE 754 operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatUnary {
    Abs,
    Neg,
    Ceil,
    Floor,
    Trunc,
    Nearest,
    Sqrt,
}

impl FloatUnary {
    fn from_opcode(opcode: u8) -> Result<Self, LoweringError> {
        let operation = match opcode {
            0x8b | 0x99 => Self::Abs,
            0x8c | 0x9a => Self::Neg,
            0x8d | 0x9b => Self::Ceil,
            0x8e | 0x9c => Self::Floor,
            0x8f | 0x9d => Self::Trunc,
            0x90 | 0x9e => Self::Nearest,
            0x91 | 0x9f => Self::Sqrt,
            _ => return Err(LoweringError::InvalidOpcode(opcode)),
        };
        Ok(operation)
    }
}

/// A basic block terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Return(Vec<ValueId>),
    Trap(Trap),
    Unreachable,
    /// Unconditional branch, passing `values` to the target block's parameters.
    Branch {
        target: BlockId,
        values: Vec<ValueId>,
    },
    /// Two-way branch. `then_values` feed the `then_target` block parameters and
    /// `else_values` feed the `else_target` block parameters.
    CondBranch {
        condition: ValueId,
        then_target: BlockId,
        then_values: Vec<ValueId>,
        else_target: BlockId,
        else_values: Vec<ValueId>,
    },
}

impl Terminator {
    /// The blocks this terminator transfers control to.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Self::Return(_) | Self::Trap(_) | Self::Unreachable => Vec::new(),
            Self::Branch { target, .. } => vec![*target],
            Self::CondBranch {
                then_target,
                else_target,
                ..
            } => vec![*then_target, *else_target],
        }
    }
}
