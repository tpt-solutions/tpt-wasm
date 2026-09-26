// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Baseline code generation.
//!
//! Priorities: correctness, startup time, predictable output, easy debugging.
//! NOT peak performance — that is the Optimizing compiler's job.
//!
//! Pipeline: Wasm → TPT IR → simple lowering → native code
//!
//! Status: M7 — portable baseline slice for the straight-line MVP instruction
//! set and structured control flow represented by the IR; native backends
//! pending.

use std::collections::HashMap;
use std::fmt;

use tpt_wasm_ir::{BlockId, IrFunction, IrInstr, Terminator, ValueId};
use tpt_wasm_types::{FunctionType, Trap, Value, ValueType};

/// Target architecture for code generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArch {
    X86_64,
    Aarch64,
    Riscv64,
}

/// A compiled function (native code bytes + metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledFunction {
    pub arch: TargetArch,
    pub code: Vec<u8>,
}

/// Errors produced while lowering certified IR to portable baseline code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodegenError {
    UnsupportedBlockCount {
        actual: usize,
    },
    MissingEntryBlock,
    DuplicateBlock(BlockId),
    UnknownBlock(BlockId),
    /// A branch edge supplied the wrong number of values for its target.
    BranchArity {
        target: BlockId,
        expected: usize,
        actual: usize,
    },
    DuplicateValue(ValueId),
    UnknownValue(ValueId),
    TypeMismatch {
        value: ValueId,
        expected: ValueType,
        actual: ValueType,
    },
    UnsupportedInstruction(&'static str),
    UnsupportedTerminator(&'static str),
    /// A memory declaration this backend cannot represent.
    UnsupportedMemory(&'static str),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CodegenError {}

/// A portable baseline `i32` binary operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I32BinaryOp {
    Add,
    Sub,
    Mul,
    DivS,
    DivU,
    RemS,
    RemU,
    Shl,
    ShrS,
    ShrU,
    Rotl,
    Rotr,
    And,
    Or,
    Xor,
}

/// A portable baseline `i64` binary operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I64BinaryOp {
    Add,
    Sub,
    Mul,
    DivS,
    DivU,
    RemS,
    RemU,
    Shl,
    ShrS,
    ShrU,
    Rotl,
    Rotr,
    And,
    Or,
    Xor,
}

/// A portable baseline `f32` binary operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F32BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Copysign,
}

/// A portable baseline `f64` binary operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F64BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Copysign,
}

/// A deterministic portable baseline operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineOp {
    ConstI32 {
        slot: u32,
        value: i32,
    },
    ConstI64 {
        slot: u32,
        value: i64,
    },
    I32Binary {
        result: u32,
        left: u32,
        right: u32,
        operation: I32BinaryOp,
    },
    I32Eqz {
        result: u32,
        value: u32,
    },
    I32Compare {
        result: u32,
        left: u32,
        right: u32,
        comparison: tpt_wasm_ir::IntComparison,
    },
    I64Binary {
        result: u32,
        left: u32,
        right: u32,
        operation: I64BinaryOp,
    },
    I64Eqz {
        result: u32,
        value: u32,
    },
    I64Compare {
        result: u32,
        left: u32,
        right: u32,
        comparison: tpt_wasm_ir::IntComparison,
    },
    ConstF32 {
        slot: u32,
        value: u32,
    },
    ConstF64 {
        slot: u32,
        value: u64,
    },
    F32Binary {
        result: u32,
        left: u32,
        right: u32,
        operation: F32BinaryOp,
    },
    F32Unary {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::FloatUnary,
    },
    F32Compare {
        result: u32,
        left: u32,
        right: u32,
        comparison: tpt_wasm_ir::FloatComparison,
    },
    F64Binary {
        result: u32,
        left: u32,
        right: u32,
        operation: F64BinaryOp,
    },
    F64Unary {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::FloatUnary,
    },
    F64Compare {
        result: u32,
        left: u32,
        right: u32,
        comparison: tpt_wasm_ir::FloatComparison,
    },
    Drop {
        value: u32,
    },
    Select {
        result: u32,
        condition: u32,
        left: u32,
        right: u32,
    },
    LocalGet {
        result: u32,
        local: u32,
    },
    LocalSet {
        local: u32,
        value: u32,
    },
    LocalTee {
        result: u32,
        local: u32,
        value: u32,
    },
    I32Unary {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::IntUnary,
    },
    I64Unary {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::IntUnary,
    },
    IntConvert {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::IntConversion,
    },
    Reinterpret {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::Reinterpret,
    },
    FloatConvert {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::FloatConversion,
    },
    FloatTrunc {
        result: u32,
        value: u32,
        operation: tpt_wasm_ir::FloatTrunc,
    },
    Return(Vec<u32>),
    Trap(Trap),
    Unreachable,
    /// Unconditional jump, binding `values` to the target block's parameters.
    Branch {
        target: u32,
        values: Vec<u32>,
    },
    /// Two-way jump, binding the matching operand list to each target.
    CondBranch {
        condition: u32,
        then_target: u32,
        then_values: Vec<u32>,
        else_target: u32,
        else_values: Vec<u32>,
    },
    /// Direct call to the function at this index in the module's function table.
    ///
    /// Execution needs that table, so a function containing a call can only be
    /// run by [`BaselineFunction::execute_with`].
    Call {
        function: u32,
        arguments: Vec<u32>,
        results: Vec<u32>,
    },
    /// Call an imported function through the host boundary.
    ///
    /// Execution needs a [`HostBoundary`], so a function containing a host call
    /// can only be run by a module that has one installed.
    CallHost {
        /// Index into the module's imports.
        import: u32,
        arguments: Vec<u32>,
        results: Vec<u32>,
    },
    /// Indirect call: read `operand` from the table, then dispatch.
    CallIndirect {
        /// The expected signature, as an index into the module's types.
        type_index: u32,
        /// Slot of the `i32` table index.
        operand: u32,
        arguments: Vec<u32>,
        results: Vec<u32>,
    },
    /// Read `width` bytes little-endian from memory at `address + offset`.
    Load {
        result: u32,
        address: u32,
        offset: u32,
        operation: tpt_wasm_ir::MemoryLoad,
    },
    /// Write the low `width` bytes of `value` at `address + offset`.
    Store {
        address: u32,
        value: u32,
        offset: u32,
        operation: tpt_wasm_ir::MemoryStore,
    },
    MemorySize {
        result: u32,
    },
    MemoryGrow {
        result: u32,
        delta: u32,
    },
    GlobalGet {
        result: u32,
        global: u32,
    },
    GlobalSet {
        global: u32,
        value: u32,
    },
    /// Produce a null reference of the given kind.
    RefNull {
        slot: u32,
        reference_type: tpt_wasm_types::ReferenceType,
    },
    /// Produce a reference to the function at this index in the function table.
    RefFunc {
        slot: u32,
        function: u32,
    },
    /// 1 when the operand is a null reference, 0 otherwise.
    RefIsNull {
        result: u32,
        value: u32,
    },
}

/// One basic block of a lowered baseline function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineBlock {
    /// Slots that receive the values supplied by whichever edge arrived here.
    pub params: Vec<u32>,
    /// Instructions followed by exactly one terminator operation.
    pub ops: Vec<BaselineOp>,
}

/// A certified IR function lowered to portable baseline code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineFunction {
    pub function_type: FunctionType,
    pub params: Vec<u32>,
    pub locals: Vec<u32>,
    pub value_types: Vec<ValueType>,
    /// Blocks in the same order as the IR function's blocks.
    pub blocks: Vec<BaselineBlock>,
    /// Index of the entry block within `blocks`.
    pub entry: u32,
}

struct SlotState {
    declared: HashMap<ValueId, ValueType>,
    slots: HashMap<ValueId, u32>,
    types: Vec<ValueType>,
}

impl SlotState {
    fn new(function: &IrFunction) -> Result<Self, CodegenError> {
        let mut declared = HashMap::with_capacity(function.values.len());
        for value in &function.values {
            if declared.insert(value.id, value.value_type).is_some() {
                return Err(CodegenError::DuplicateValue(value.id));
            }
        }
        Ok(Self {
            declared,
            slots: HashMap::new(),
            types: Vec::with_capacity(function.values.len()),
        })
    }

    fn define(&mut self, id: ValueId) -> Result<u32, CodegenError> {
        let value_type = self
            .declared
            .get(&id)
            .copied()
            .ok_or(CodegenError::UnknownValue(id))?;
        if self.slots.contains_key(&id) {
            return Err(CodegenError::DuplicateValue(id));
        }
        let slot = u32::try_from(self.types.len()).map_err(|_| CodegenError::UnknownValue(id))?;
        self.slots.insert(id, slot);
        self.types.push(value_type);
        Ok(slot)
    }

    fn slot(&self, id: ValueId) -> Result<u32, CodegenError> {
        self.slots
            .get(&id)
            .copied()
            .ok_or(CodegenError::UnknownValue(id))
    }

    fn expect(&self, id: ValueId, expected: ValueType) -> Result<u32, CodegenError> {
        let slot = self.slot(id)?;
        let actual = self.types[slot as usize];
        if actual == expected {
            Ok(slot)
        } else {
            Err(CodegenError::TypeMismatch {
                value: id,
                expected,
                actual,
            })
        }
    }

    /// The slot and declared type of a value, used where a value only needs to
    /// agree with itself (locals, `select` arms) instead of a fixed type.
    fn typed(&self, id: ValueId) -> Result<(u32, ValueType), CodegenError> {
        let slot = self.slot(id)?;
        Ok((slot, self.types[slot as usize]))
    }
}

/// Lower one certified IR function to portable baseline code.
///
/// Every basic block is lowered; branch edges carry the slots that bind the
/// target block's parameters, mirroring the IR's block-argument phis.
pub fn lower_function(function: &IrFunction) -> Result<BaselineFunction, CodegenError> {
    if function.blocks.is_empty() {
        return Err(CodegenError::MissingEntryBlock);
    }
    let mut state = SlotState::new(function)?;
    // Branch targets are IR block ids; the baseline addresses blocks by index.
    let mut index_of = HashMap::with_capacity(function.blocks.len());
    for (index, block) in function.blocks.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| CodegenError::UnsupportedBlockCount {
            actual: function.blocks.len(),
        })?;
        if index_of.insert(block.id, index).is_some() {
            return Err(CodegenError::DuplicateBlock(block.id));
        }
    }
    if !index_of.contains_key(&function.entry) {
        return Err(CodegenError::MissingEntryBlock);
    }
    let entry = index_of[&function.entry];
    let params = function
        .params
        .iter()
        .map(|id| state.define(*id))
        .collect::<Result<Vec<_>, _>>()?;
    let locals = function
        .locals
        .iter()
        .map(|id| state.define(*id))
        .collect::<Result<Vec<_>, _>>()?;
    // Block parameters (phi nodes) get slots up front so that an edge from any
    // block can bind its operands no matter which order the blocks appear in.
    let mut block_params = Vec::with_capacity(function.blocks.len());
    for block in &function.blocks {
        let mut slots = Vec::with_capacity(block.params.len());
        for id in &block.params {
            slots.push(state.define(*id)?);
        }
        block_params.push(slots);
    }

    let mut blocks = Vec::with_capacity(function.blocks.len());
    for (index, block) in function.blocks.iter().enumerate() {
        let mut ops = Vec::with_capacity(block.instrs.len() + 1);
        for instruction in &block.instrs {
            match instruction {
                IrInstr::ConstI32 { result, value } => {
                    let slot = state.define(*result)?;
                    ops.push(BaselineOp::ConstI32 {
                        slot,
                        value: *value,
                    });
                }
                IrInstr::I32Add { .. }
                | IrInstr::I32Sub { .. }
                | IrInstr::I32Mul { .. }
                | IrInstr::I32DivS { .. }
                | IrInstr::I32DivU { .. }
                | IrInstr::I32RemS { .. }
                | IrInstr::I32RemU { .. }
                | IrInstr::I32Shl { .. }
                | IrInstr::I32ShrS { .. }
                | IrInstr::I32ShrU { .. }
                | IrInstr::I32Rotl { .. }
                | IrInstr::I32Rotr { .. }
                | IrInstr::I32And { .. }
                | IrInstr::I32Or { .. }
                | IrInstr::I32Xor { .. } => {
                    let operation = match instruction {
                        IrInstr::I32Add { .. } => I32BinaryOp::Add,
                        IrInstr::I32Sub { .. } => I32BinaryOp::Sub,
                        IrInstr::I32Mul { .. } => I32BinaryOp::Mul,
                        IrInstr::I32DivS { .. } => I32BinaryOp::DivS,
                        IrInstr::I32DivU { .. } => I32BinaryOp::DivU,
                        IrInstr::I32RemS { .. } => I32BinaryOp::RemS,
                        IrInstr::I32RemU { .. } => I32BinaryOp::RemU,
                        IrInstr::I32Shl { .. } => I32BinaryOp::Shl,
                        IrInstr::I32ShrS { .. } => I32BinaryOp::ShrS,
                        IrInstr::I32ShrU { .. } => I32BinaryOp::ShrU,
                        IrInstr::I32Rotl { .. } => I32BinaryOp::Rotl,
                        IrInstr::I32Rotr { .. } => I32BinaryOp::Rotr,
                        IrInstr::I32And { .. } => I32BinaryOp::And,
                        IrInstr::I32Or { .. } => I32BinaryOp::Or,
                        IrInstr::I32Xor { .. } => I32BinaryOp::Xor,
                        _ => return Err(CodegenError::UnsupportedInstruction("i32 binary")),
                    };
                    let (result, left, right) = match instruction {
                        IrInstr::I32Add {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32Sub {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32Mul {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32DivS {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32DivU {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32RemS {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32RemU {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32Shl {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32ShrS {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32ShrU {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32Rotl {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32Rotr {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32And {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32Or {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I32Xor {
                            result,
                            left,
                            right,
                        } => (*result, *left, *right),
                        _ => unreachable!(),
                    };
                    let left = state.expect(left, ValueType::I32)?;
                    let right = state.expect(right, ValueType::I32)?;
                    let result = state.define(result)?;
                    ops.push(BaselineOp::I32Binary {
                        result,
                        left,
                        right,
                        operation,
                    });
                }
                IrInstr::I32Eqz { result, value } => {
                    let value = state.expect(*value, ValueType::I32)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::I32Eqz { result, value });
                }
                IrInstr::I32Compare {
                    result,
                    left,
                    right,
                    comparison,
                } => {
                    let left = state.expect(*left, ValueType::I32)?;
                    let right = state.expect(*right, ValueType::I32)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::I32Compare {
                        result,
                        left,
                        right,
                        comparison: *comparison,
                    });
                }
                IrInstr::ConstI64 { result, value } => {
                    let slot = state.define(*result)?;
                    ops.push(BaselineOp::ConstI64 {
                        slot,
                        value: *value,
                    });
                }
                IrInstr::I64Add { .. }
                | IrInstr::I64Sub { .. }
                | IrInstr::I64Mul { .. }
                | IrInstr::I64DivS { .. }
                | IrInstr::I64DivU { .. }
                | IrInstr::I64RemS { .. }
                | IrInstr::I64RemU { .. }
                | IrInstr::I64Shl { .. }
                | IrInstr::I64ShrS { .. }
                | IrInstr::I64ShrU { .. }
                | IrInstr::I64Rotl { .. }
                | IrInstr::I64Rotr { .. }
                | IrInstr::I64And { .. }
                | IrInstr::I64Or { .. }
                | IrInstr::I64Xor { .. } => {
                    let operation = match instruction {
                        IrInstr::I64Add { .. } => I64BinaryOp::Add,
                        IrInstr::I64Sub { .. } => I64BinaryOp::Sub,
                        IrInstr::I64Mul { .. } => I64BinaryOp::Mul,
                        IrInstr::I64DivS { .. } => I64BinaryOp::DivS,
                        IrInstr::I64DivU { .. } => I64BinaryOp::DivU,
                        IrInstr::I64RemS { .. } => I64BinaryOp::RemS,
                        IrInstr::I64RemU { .. } => I64BinaryOp::RemU,
                        IrInstr::I64Shl { .. } => I64BinaryOp::Shl,
                        IrInstr::I64ShrS { .. } => I64BinaryOp::ShrS,
                        IrInstr::I64ShrU { .. } => I64BinaryOp::ShrU,
                        IrInstr::I64Rotl { .. } => I64BinaryOp::Rotl,
                        IrInstr::I64Rotr { .. } => I64BinaryOp::Rotr,
                        IrInstr::I64And { .. } => I64BinaryOp::And,
                        IrInstr::I64Or { .. } => I64BinaryOp::Or,
                        IrInstr::I64Xor { .. } => I64BinaryOp::Xor,
                        _ => return Err(CodegenError::UnsupportedInstruction("i64 binary")),
                    };
                    let (result, left, right) = match instruction {
                        IrInstr::I64Add {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64Sub {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64Mul {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64DivS {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64DivU {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64RemS {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64RemU {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64Shl {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64ShrS {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64ShrU {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64Rotl {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64Rotr {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64And {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64Or {
                            result,
                            left,
                            right,
                        }
                        | IrInstr::I64Xor {
                            result,
                            left,
                            right,
                        } => (*result, *left, *right),
                        _ => unreachable!(),
                    };
                    let left = state.expect(left, ValueType::I64)?;
                    let right = state.expect(right, ValueType::I64)?;
                    let result = state.define(result)?;
                    ops.push(BaselineOp::I64Binary {
                        result,
                        left,
                        right,
                        operation,
                    });
                }
                IrInstr::I64Eqz { result, value } => {
                    let value = state.expect(*value, ValueType::I64)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::I64Eqz { result, value });
                }
                IrInstr::I64Compare {
                    result,
                    left,
                    right,
                    comparison,
                } => {
                    let left = state.expect(*left, ValueType::I64)?;
                    let right = state.expect(*right, ValueType::I64)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::I64Compare {
                        result,
                        left,
                        right,
                        comparison: *comparison,
                    });
                }
                IrInstr::ConstF32 { result, value } => {
                    let slot = state.define(*result)?;
                    ops.push(BaselineOp::ConstF32 {
                        slot,
                        value: *value,
                    });
                }
                IrInstr::F32Add { .. }
                | IrInstr::F32Sub { .. }
                | IrInstr::F32Mul { .. }
                | IrInstr::F32Div { .. }
                | IrInstr::F32Min { .. }
                | IrInstr::F32Max { .. }
                | IrInstr::F32Copysign { .. } => {
                    let (operation, result, left, right) = match instruction {
                        IrInstr::F32Add {
                            result,
                            left,
                            right,
                        } => (F32BinaryOp::Add, *result, *left, *right),
                        IrInstr::F32Sub {
                            result,
                            left,
                            right,
                        } => (F32BinaryOp::Sub, *result, *left, *right),
                        IrInstr::F32Mul {
                            result,
                            left,
                            right,
                        } => (F32BinaryOp::Mul, *result, *left, *right),
                        IrInstr::F32Div {
                            result,
                            left,
                            right,
                        } => (F32BinaryOp::Div, *result, *left, *right),
                        IrInstr::F32Min {
                            result,
                            left,
                            right,
                        } => (F32BinaryOp::Min, *result, *left, *right),
                        IrInstr::F32Max {
                            result,
                            left,
                            right,
                        } => (F32BinaryOp::Max, *result, *left, *right),
                        IrInstr::F32Copysign {
                            result,
                            left,
                            right,
                        } => (F32BinaryOp::Copysign, *result, *left, *right),
                        _ => return Err(CodegenError::UnsupportedInstruction("f32 binary")),
                    };
                    let left = state.expect(left, ValueType::F32)?;
                    let right = state.expect(right, ValueType::F32)?;
                    let result = state.define(result)?;
                    ops.push(BaselineOp::F32Binary {
                        result,
                        left,
                        right,
                        operation,
                    });
                }
                IrInstr::F32Unary {
                    result,
                    value,
                    operation,
                } => {
                    let value = state.expect(*value, ValueType::F32)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::F32Unary {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::F32Compare {
                    result,
                    left,
                    right,
                    comparison,
                } => {
                    let left = state.expect(*left, ValueType::F32)?;
                    let right = state.expect(*right, ValueType::F32)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::F32Compare {
                        result,
                        left,
                        right,
                        comparison: *comparison,
                    });
                }
                IrInstr::ConstF64 { result, value } => {
                    let slot = state.define(*result)?;
                    ops.push(BaselineOp::ConstF64 {
                        slot,
                        value: *value,
                    });
                }
                IrInstr::F64Add { .. }
                | IrInstr::F64Sub { .. }
                | IrInstr::F64Mul { .. }
                | IrInstr::F64Div { .. }
                | IrInstr::F64Min { .. }
                | IrInstr::F64Max { .. }
                | IrInstr::F64Copysign { .. } => {
                    let (operation, result, left, right) = match instruction {
                        IrInstr::F64Add {
                            result,
                            left,
                            right,
                        } => (F64BinaryOp::Add, *result, *left, *right),
                        IrInstr::F64Sub {
                            result,
                            left,
                            right,
                        } => (F64BinaryOp::Sub, *result, *left, *right),
                        IrInstr::F64Mul {
                            result,
                            left,
                            right,
                        } => (F64BinaryOp::Mul, *result, *left, *right),
                        IrInstr::F64Div {
                            result,
                            left,
                            right,
                        } => (F64BinaryOp::Div, *result, *left, *right),
                        IrInstr::F64Min {
                            result,
                            left,
                            right,
                        } => (F64BinaryOp::Min, *result, *left, *right),
                        IrInstr::F64Max {
                            result,
                            left,
                            right,
                        } => (F64BinaryOp::Max, *result, *left, *right),
                        IrInstr::F64Copysign {
                            result,
                            left,
                            right,
                        } => (F64BinaryOp::Copysign, *result, *left, *right),
                        _ => return Err(CodegenError::UnsupportedInstruction("f64 binary")),
                    };
                    let left = state.expect(left, ValueType::F64)?;
                    let right = state.expect(right, ValueType::F64)?;
                    let result = state.define(result)?;
                    ops.push(BaselineOp::F64Binary {
                        result,
                        left,
                        right,
                        operation,
                    });
                }
                IrInstr::F64Unary {
                    result,
                    value,
                    operation,
                } => {
                    let value = state.expect(*value, ValueType::F64)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::F64Unary {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::F64Compare {
                    result,
                    left,
                    right,
                    comparison,
                } => {
                    let left = state.expect(*left, ValueType::F64)?;
                    let right = state.expect(*right, ValueType::F64)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::F64Compare {
                        result,
                        left,
                        right,
                        comparison: *comparison,
                    });
                }
                IrInstr::Drop { value } => {
                    // `drop` discards a value that has already been computed, so it
                    // only needs to type-check the operand; the slot stays live for
                    // the remainder of the straight-line sequence.
                    let value = state.slot(*value)?;
                    ops.push(BaselineOp::Drop { value });
                }
                IrInstr::Select {
                    result,
                    condition,
                    left,
                    right,
                } => {
                    let condition = state.expect(*condition, ValueType::I32)?;
                    let (left_slot, left_type) = state.typed(*left)?;
                    let (right_slot, right_type) = state.typed(*right)?;
                    if left_type != right_type {
                        return Err(CodegenError::TypeMismatch {
                            value: *right,
                            expected: left_type,
                            actual: right_type,
                        });
                    }
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::Select {
                        result,
                        condition,
                        left: left_slot,
                        right: right_slot,
                    });
                }
                IrInstr::LocalGet { result, local } => {
                    // Locals are pre-allocated slots, so a `get` is a typed copy.
                    let (local, _) = state.typed(*local)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::LocalGet { result, local });
                }
                IrInstr::LocalSet { local, value } => {
                    let (local, local_type) = state.typed(*local)?;
                    let value = state.expect(*value, local_type)?;
                    ops.push(BaselineOp::LocalSet { local, value });
                }
                IrInstr::LocalTee {
                    result,
                    local,
                    value,
                } => {
                    let (local, local_type) = state.typed(*local)?;
                    let value = state.expect(*value, local_type)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::LocalTee {
                        result,
                        local,
                        value,
                    });
                }
                IrInstr::I32Unary {
                    result,
                    value,
                    operation,
                } => {
                    let value = state.expect(*value, ValueType::I32)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::I32Unary {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::I64Unary {
                    result,
                    value,
                    operation,
                } => {
                    let value = state.expect(*value, ValueType::I64)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::I64Unary {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::IntConvert {
                    result,
                    value,
                    operation,
                } => {
                    let source = match operation {
                        tpt_wasm_ir::IntConversion::I32WrapI64 => ValueType::I64,
                        tpt_wasm_ir::IntConversion::I64ExtendI32S
                        | tpt_wasm_ir::IntConversion::I64ExtendI32U => ValueType::I32,
                    };
                    let value = state.expect(*value, source)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::IntConvert {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::Reinterpret {
                    result,
                    value,
                    operation,
                } => {
                    let source = match operation {
                        tpt_wasm_ir::Reinterpret::I32FromF32 => ValueType::F32,
                        tpt_wasm_ir::Reinterpret::I64FromF64 => ValueType::F64,
                        tpt_wasm_ir::Reinterpret::F32FromI32 => ValueType::I32,
                        tpt_wasm_ir::Reinterpret::F64FromI64 => ValueType::I64,
                    };
                    let value = state.expect(*value, source)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::Reinterpret {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::FloatConvert {
                    result,
                    value,
                    operation,
                } => {
                    let source = match operation {
                        tpt_wasm_ir::FloatConversion::F32FromI32S
                        | tpt_wasm_ir::FloatConversion::F32FromI32U => ValueType::I32,
                        tpt_wasm_ir::FloatConversion::F32FromI64S
                        | tpt_wasm_ir::FloatConversion::F32FromI64U => ValueType::I64,
                        tpt_wasm_ir::FloatConversion::F32FromF64 => ValueType::F64,
                        tpt_wasm_ir::FloatConversion::F64FromI32S
                        | tpt_wasm_ir::FloatConversion::F64FromI32U => ValueType::I32,
                        tpt_wasm_ir::FloatConversion::F64FromI64S
                        | tpt_wasm_ir::FloatConversion::F64FromI64U => ValueType::I64,
                        tpt_wasm_ir::FloatConversion::F64FromF32 => ValueType::F32,
                    };
                    let value = state.expect(*value, source)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::FloatConvert {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::FloatTrunc {
                    result,
                    value,
                    operation,
                } => {
                    let source = match operation {
                        tpt_wasm_ir::FloatTrunc::I32FromF32S
                        | tpt_wasm_ir::FloatTrunc::I32FromF32U
                        | tpt_wasm_ir::FloatTrunc::I64FromF32S
                        | tpt_wasm_ir::FloatTrunc::I64FromF32U => ValueType::F32,
                        tpt_wasm_ir::FloatTrunc::I32FromF64S
                        | tpt_wasm_ir::FloatTrunc::I32FromF64U
                        | tpt_wasm_ir::FloatTrunc::I64FromF64S
                        | tpt_wasm_ir::FloatTrunc::I64FromF64U => ValueType::F64,
                    };
                    let value = state.expect(*value, source)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::FloatTrunc {
                        result,
                        value,
                        operation: *operation,
                    });
                }
                IrInstr::Call {
                    function,
                    arguments,
                    results,
                } => {
                    // The callee's signature was checked against the module by the
                    // verifier; the baseline only needs the argument and result
                    // slots, and re-checks that the arity is self-consistent.
                    let mut argument_slots = Vec::with_capacity(arguments.len());
                    for id in arguments {
                        argument_slots.push(state.slot(*id)?);
                    }
                    let mut result_slots = Vec::with_capacity(results.len());
                    for id in results {
                        result_slots.push(state.define(*id)?);
                    }
                    ops.push(BaselineOp::Call {
                        function: *function,
                        arguments: argument_slots,
                        results: result_slots,
                    });
                }
                IrInstr::CallHost {
                    import,
                    arguments,
                    results,
                } => {
                    // The import's signature was checked by the verifier; the
                    // baseline only needs the argument and result slots, and
                    // re-checks that the arity is self-consistent.
                    let mut argument_slots = Vec::with_capacity(arguments.len());
                    for id in arguments {
                        argument_slots.push(state.slot(*id)?);
                    }
                    let mut result_slots = Vec::with_capacity(results.len());
                    for id in results {
                        result_slots.push(state.define(*id)?);
                    }
                    ops.push(BaselineOp::CallHost {
                        import: *import,
                        arguments: argument_slots,
                        results: result_slots,
                    });
                }
                IrInstr::CallIndirect {
                    type_index,
                    operand,
                    arguments,
                    results,
                    ..
                } => {
                    let operand = state.expect(*operand, ValueType::I32)?;
                    let mut argument_slots = Vec::with_capacity(arguments.len());
                    for id in arguments {
                        argument_slots.push(state.slot(*id)?);
                    }
                    let mut result_slots = Vec::with_capacity(results.len());
                    for id in results {
                        result_slots.push(state.define(*id)?);
                    }
                    ops.push(BaselineOp::CallIndirect {
                        type_index: *type_index,
                        operand,
                        arguments: argument_slots,
                        results: result_slots,
                    });
                }
                IrInstr::Load {
                    result,
                    address,
                    offset,
                    operation,
                } => {
                    let address = state.expect(*address, ValueType::I32)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::Load {
                        result,
                        address,
                        offset: *offset,
                        operation: *operation,
                    });
                }
                IrInstr::Store {
                    address,
                    value,
                    offset,
                    operation,
                } => {
                    let address = state.expect(*address, ValueType::I32)?;
                    let value = state.expect(*value, operation.operand_type())?;
                    ops.push(BaselineOp::Store {
                        address,
                        value,
                        offset: *offset,
                        operation: *operation,
                    });
                }
                IrInstr::MemorySize { result } => {
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::MemorySize { result });
                }
                IrInstr::MemoryGrow { result, delta } => {
                    let delta = state.expect(*delta, ValueType::I32)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::MemoryGrow { result, delta });
                }
                IrInstr::GlobalGet { result, global } => {
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::GlobalGet {
                        result,
                        global: *global,
                    });
                }
                IrInstr::GlobalSet { global, value } => {
                    // The global's declared type was checked by the verifier; the
                    // baseline only needs the value's slot.
                    let value = state.slot(*value)?;
                    ops.push(BaselineOp::GlobalSet {
                        global: *global,
                        value,
                    });
                }
                IrInstr::RefNull {
                    result,
                    reference_type,
                } => {
                    // The result's declared type already pins the kind, so the
                    // verifier has established the two agree.
                    let slot = state.define(*result)?;
                    ops.push(BaselineOp::RefNull {
                        slot,
                        reference_type: *reference_type,
                    });
                }
                IrInstr::RefFunc { result, function } => {
                    // The index was checked against the module's function table.
                    let slot = state.define(*result)?;
                    ops.push(BaselineOp::RefFunc {
                        slot,
                        function: *function,
                    });
                }
                IrInstr::RefIsNull { result, value } => {
                    // Either reference kind is accepted, so the operand is read
                    // by slot rather than through a typed accessor.
                    let value = state.slot(*value)?;
                    let result = state.define(*result)?;
                    ops.push(BaselineOp::RefIsNull { result, value });
                } // Every IR instruction is lowered above. When the IR grows, this
                  // arm becomes reachable again and rejects the new instruction
                  // rather than silently dropping it; until then it is dead, so the
                  // `IrInstr` match above is the exhaustive one.
            }
        }
        let terminator = match &block.terminator {
            Terminator::Return(results) => {
                let mut slots = Vec::with_capacity(results.len());
                for (id, expected) in results.iter().zip(&function.function_type.results.0) {
                    slots.push(state.expect(*id, *expected)?);
                }
                if slots.len() != function.function_type.results.0.len() {
                    return Err(CodegenError::UnsupportedTerminator("return arity"));
                }
                BaselineOp::Return(slots)
            }
            Terminator::Trap(trap) => BaselineOp::Trap(trap.clone()),
            Terminator::Unreachable => BaselineOp::Unreachable,
            Terminator::Branch { target, values } => {
                let (index, slots) = lower_edge(*target, values, &index_of, &block_params, &state)?;
                BaselineOp::Branch {
                    target: index,
                    values: slots,
                }
            }
            Terminator::CondBranch {
                condition,
                then_target,
                then_values,
                else_target,
                else_values,
            } => {
                let condition = state.expect(*condition, ValueType::I32)?;
                let (then_index, then_slots) =
                    lower_edge(*then_target, then_values, &index_of, &block_params, &state)?;
                let (else_index, else_slots) =
                    lower_edge(*else_target, else_values, &index_of, &block_params, &state)?;
                BaselineOp::CondBranch {
                    condition,
                    then_target: then_index,
                    then_values: then_slots,
                    else_target: else_index,
                    else_values: else_slots,
                }
            }
        };
        ops.push(terminator);
        blocks.push(BaselineBlock {
            params: block_params[index].clone(),
            ops,
        });
    }
    Ok(BaselineFunction {
        function_type: function.function_type.clone(),
        params,
        locals,
        value_types: state.types,
        blocks,
        entry,
    })
}

/// Resolve one outgoing edge: the target's block index plus the slots of the
/// operands bound to that block's parameters.
///
/// The verifier has already proved edges well-typed, but the baseline re-checks
/// arity and types so it stays safe when handed an unverified IR function.
fn lower_edge(
    target: BlockId,
    values: &[ValueId],
    index_of: &HashMap<BlockId, u32>,
    block_params: &[Vec<u32>],
    state: &SlotState,
) -> Result<(u32, Vec<u32>), CodegenError> {
    let index = *index_of
        .get(&target)
        .ok_or(CodegenError::UnknownBlock(target))?;
    let expected = &block_params[index as usize];
    if values.len() != expected.len() {
        return Err(CodegenError::BranchArity {
            target,
            expected: expected.len(),
            actual: values.len(),
        });
    }
    let mut slots = Vec::with_capacity(values.len());
    for (id, param) in values.iter().zip(expected) {
        // Target parameters are already allocated slots, so their type is known
        // without a second lookup through the value table.
        slots.push(state.expect(*id, state.types[*param as usize])?);
    }
    Ok((index, slots))
}

/// Lower every function of a verified module into a function table.
///
/// A `Call` names a function by module index, so the functions must be lowered
/// together to be executable.
pub fn lower_module(module: &tpt_wasm_ir::IrModule) -> Result<Vec<BaselineFunction>, CodegenError> {
    module.functions.iter().map(lower_function).collect()
}

/// The linear memory a lowered module executes against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BaselineMemory {
    data: Vec<u8>,
    max_pages: Option<u64>,
}

/// One 64 KiB page.
const PAGE_SIZE: usize = 65_536;
/// The architecture limit on page count, and so on address space.
const MAX_PAGES: u64 = 65_536;

impl BaselineMemory {
    /// Build a memory of `min_pages` zeroed pages, capped by `max_pages`.
    pub fn new(min_pages: u64, max_pages: Option<u64>) -> Result<Self, CodegenError> {
        if min_pages > MAX_PAGES {
            return Err(CodegenError::UnsupportedMemory("page count"));
        }
        let bytes = usize::try_from(min_pages * PAGE_SIZE as u64)
            .map_err(|_| CodegenError::UnsupportedMemory("page count"))?;
        Ok(Self {
            data: vec![0; bytes],
            max_pages,
        })
    }

    /// Current size in pages.
    pub fn pages(&self) -> u32 {
        u32::try_from(self.data.len() / PAGE_SIZE).unwrap_or(u32::MAX)
    }

    /// Read `width` bytes little-endian, trapping if the access is out of bounds.
    fn read(&self, address: u64, width: u64) -> Result<u64, Trap> {
        let end = address.checked_add(width).ok_or(Trap::MemoryOutOfBounds)?;
        if end > self.data.len() as u64 {
            return Err(Trap::MemoryOutOfBounds);
        }
        let start = address as usize;
        let mut bits = 0u64;
        for (index, byte) in self.data[start..start + width as usize].iter().enumerate() {
            bits |= u64::from(*byte) << (index * 8);
        }
        Ok(bits)
    }

    /// Write the low `width` bytes of `bits`, trapping if out of bounds.
    fn write(&mut self, address: u64, width: u64, bits: u64) -> Result<(), Trap> {
        let end = address.checked_add(width).ok_or(Trap::MemoryOutOfBounds)?;
        if end > self.data.len() as u64 {
            return Err(Trap::MemoryOutOfBounds);
        }
        let start = address as usize;
        for index in 0..width as usize {
            self.data[start + index] = (bits >> (index * 8)) as u8;
        }
        Ok(())
    }

    /// Grow by `delta` pages, returning the previous size or -1 on failure.
    fn grow(&mut self, delta: u64) -> i32 {
        let previous = self.pages() as u64;
        let requested = match previous.checked_add(delta) {
            Some(total) => total,
            None => return -1,
        };
        if let Some(max) = self.max_pages {
            if requested > max {
                return -1;
            }
        }
        if requested > MAX_PAGES {
            return -1;
        }
        let bytes = match usize::try_from(requested * PAGE_SIZE as u64) {
            Ok(bytes) => bytes,
            Err(_) => return -1,
        };
        self.data.resize(bytes, 0);
        previous as i32
    }
}

/// A lowered module: its functions plus the table, memory, and globals they
/// share.
///
/// State is shared across calls exactly as it is in Wasm, so a callee observes
/// the caller's stores and a `global.set` inside a callee is visible to the
/// caller.
///
/// This is `Debug` by hand rather than derived: the installed [`HostBoundary`] is
/// embedder-owned mutable state, so it is neither comparable nor cloneable, and
/// comparing two modules would have to ignore it anyway.
pub struct BaselineModule {
    pub functions: Vec<BaselineFunction>,
    /// One table, holding optional function indices into `functions`.
    table: Option<BaselineTable>,
    /// The module's function types, needed to check an indirect call's target.
    types: Vec<FunctionType>,
    memory: Option<BaselineMemory>,
    globals: Vec<Value>,
    /// The imported functions, in declaration order, that a `CallHost` names.
    imports: Vec<tpt_wasm_ir::IrImport>,
    /// The installed host boundary, absent until an embedder provides one.
    ///
    /// `Box` rather than an `Arc` because the boundary is stateful: it has to
    /// record the same effects Micro does, and it is reachable only from the
    /// execution state that owns the call.
    host: Option<Box<dyn HostBoundary>>,
}

/// A table of optional function references, as MVP `funcref` tables hold.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BaselineTable {
    /// `None` is a null reference, which `call_indirect` traps on.
    elements: Vec<Option<u32>>,
}

impl fmt::Debug for BaselineModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BaselineModule")
            .field("functions", &self.functions.len())
            .field("table", &self.table)
            .field("types", &self.types)
            .field("memory", &self.memory)
            .field("globals", &self.globals)
            .field("imports", &self.imports)
            // The boundary itself is embedder state, so only whether one is
            // installed is reported.
            .field("host", &self.host.as_ref().map(|_| "installed"))
            .finish()
    }
}

/// A host function refused or failed.
///
/// Reported with the name the host is registered under and the host's own
/// message, which is the shape the interpreter reports a host failure in. Keeping
/// the two identical is what lets an embedder treat a compiled module and an
/// interpreted one interchangeably.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCallFailure {
    /// The name the host function is registered under.
    pub name: String,
    /// The host's own message.
    pub message: String,
}

/// Why a baseline execution did not return.
///
/// A Wasm trap and a failed host call are different things: the first is the
/// module's own fault, the second is the embedder refusing a capability. The
/// interpreter reports them differently, so the baseline keeps them apart rather
/// than flattening a refused capability into a module trap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineError {
    /// The module trapped.
    Trap(Trap),
    /// A host function failed.
    Host(HostCallFailure),
}

impl From<Trap> for BaselineError {
    fn from(trap: Trap) -> Self {
        Self::Trap(trap)
    }
}

impl From<HostCallFailure> for BaselineError {
    fn from(failure: HostCallFailure) -> Self {
        Self::Host(failure)
    }
}

/// The host boundary a compiled module calls imported functions through.
///
/// This is the only way the baseline reaches outside itself, so a host effect is
/// always an explicit, grantable step rather than something a compiled module can
/// do on its own. The embedder implements this against whatever authorizes calls;
/// the runtime routes it through the same capability-mediated path the Micro
/// interpreter uses, so both backends produce the same effects and failures.
pub trait HostBoundary {
    /// Invoke the host function an import names, or fail.
    fn call(
        &mut self,
        module: &str,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, HostCallFailure>;
}

impl BaselineModule {
    /// Build a runnable module from a verified IR module and its lowered
    /// functions, which must be in the same order.
    pub fn new(
        module: &tpt_wasm_ir::IrModule,
        functions: Vec<BaselineFunction>,
    ) -> Result<Self, CodegenError> {
        let memory = match module.memory.as_ref() {
            Some(declaration) => {
                let mut memory = BaselineMemory::new(declaration.min_pages, declaration.max_pages)?;
                // Active data segments are applied in module order, so a later
                // segment overwrites an earlier one at the same address.
                for segment in &declaration.segments {
                    let start = segment.offset as usize;
                    let end = start + segment.bytes.len();
                    if end > memory.data.len() {
                        return Err(CodegenError::UnsupportedMemory(
                            "data segment does not fit the memory",
                        ));
                    }
                    memory.data[start..end].copy_from_slice(&segment.bytes);
                }
                Some(memory)
            }
            None => None,
        };
        let table = match module.tables.first() {
            Some(declaration) => {
                let length = usize::try_from(declaration.min)
                    .map_err(|_| CodegenError::UnsupportedMemory("table size"))?;
                let mut elements = vec![None; length];
                for (position, entry) in declaration.elements.iter().enumerate() {
                    let slot = declaration.offset as usize + position;
                    // The validator bounds-checks the segment against the table,
                    // so a slot past the end here means the two disagree.
                    let cell = elements
                        .get_mut(slot)
                        .ok_or(CodegenError::UnsupportedMemory(
                            "element segment does not fit the table",
                        ))?;
                    *cell = *entry;
                }
                Some(BaselineTable { elements })
            }
            None => None,
        };
        Ok(Self {
            functions,
            table,
            types: module.types.clone(),
            memory,
            globals: module.globals.iter().map(|g| g.init.clone()).collect(),
            imports: module.imports.clone(),
            host: None,
        })
    }

    /// Rebase a Wasm function index onto this module's own function table.
    ///
    /// Wasm numbers imported functions first, so an index below the import count
    /// names a host function with no counterpart here. `None` reports that,
    /// rather than silently pointing at a different function.
    pub fn defined_index(&self, wasm_index: u32) -> Option<usize> {
        let defined = wasm_index.checked_sub(self.imports.len() as u32)?;
        let index = usize::try_from(defined).ok()?;
        (index < self.functions.len()).then_some(index)
    }

    /// Install the boundary that imported functions are called through.
    ///
    /// Without one, a `CallHost` traps rather than reaching outside the module,
    /// so a compiled module cannot acquire a host capability by accident.
    pub fn set_host(&mut self, host: impl HostBoundary + 'static) -> &mut Self {
        self.host = Some(Box::new(host));
        self
    }

    /// Lower a verified IR module and wrap it so it can be executed.
    pub fn lower(module: &tpt_wasm_ir::IrModule) -> Result<Self, CodegenError> {
        Self::new(module, lower_module(module)?)
    }

    /// Call one function by index.
    pub fn call(&mut self, index: usize, args: Vec<Value>) -> Result<Vec<Value>, BaselineError> {
        if index >= self.functions.len() {
            return Err(BaselineError::Trap(Trap::HostFailure(
                "baseline call target is not in the module".into(),
            )));
        }
        // `functions` is copied out so the callee borrow is independent of
        // `self`, which lets the recursive call take `&mut state` as well.
        let functions: &[BaselineFunction] = &self.functions;
        let imports: &[tpt_wasm_ir::IrImport] = &self.imports;
        let mut state = ExecState {
            functions,
            imports,
            table: self.table.as_ref(),
            types: &self.types,
            memory: self.memory.as_mut(),
            globals: &mut self.globals,
            host: self.host.as_deref_mut(),
        };
        functions[index].run(&mut state, args)
    }
}

/// Everything one execution shares: the callee table, the table, memory, globals.
struct ExecState<'a> {
    functions: &'a [BaselineFunction],
    /// The imported functions a `CallHost` names.
    imports: &'a [tpt_wasm_ir::IrImport],
    table: Option<&'a BaselineTable>,
    /// Module types, so an indirect call can check its target's signature.
    types: &'a [FunctionType],
    memory: Option<&'a mut BaselineMemory>,
    globals: &'a mut Vec<Value>,
    /// The installed host boundary, absent when the embedder granted none.
    ///
    /// Bounded by `'static` rather than `'a`: the boundary is owned by the
    /// module, so the borrow only has to be as long as the execution.
    host: Option<&'a mut (dyn HostBoundary + 'static)>,
}

impl BaselineFunction {
    /// Execute this portable baseline function without invoking Micro.
    ///
    /// This has no module, so a function that calls, or that touches memory or
    /// globals, cannot run this way; use [`BaselineModule::call`].
    pub fn execute(&self, args: Vec<Value>) -> Result<Vec<Value>, BaselineError> {
        self.execute_with(&[], args)
    }

    /// Execute this function with `functions` as the module's function table.
    ///
    /// There is still no memory or global state, so a function using those traps
    /// rather than silently reading uninitialized data.
    pub fn execute_with(
        &self,
        functions: &[BaselineFunction],
        args: Vec<Value>,
    ) -> Result<Vec<Value>, BaselineError> {
        let functions: &[BaselineFunction] = functions;
        let mut state = ExecState {
            functions,
            imports: &[],
            table: None,
            types: &[],
            memory: None,
            globals: &mut Vec::new(),
            host: None,
        };
        self.run(&mut state, args)
    }

    /// Run this function against the module state, which is shared with any
    /// callee it invokes.
    fn run(
        &self,
        state: &mut ExecState<'_>,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, BaselineError> {
        if args.len() != self.params.len() {
            return Err(BaselineError::Trap(Trap::HostFailure(
                "baseline argument arity mismatch".into(),
            )));
        }
        let mut slots = vec![None; self.value_types.len()];
        for (slot, value) in self.params.iter().copied().zip(args) {
            check_value_type(&value, self.value_types[slot as usize])?;
            slots[slot as usize] = Some(value);
        }
        for slot in &self.locals {
            slots[*slot as usize] = Some(default_value(self.value_types[*slot as usize]));
        }
        // Walk the block graph from the entry block. A well-formed function always
        // reaches a `Return`, `Trap`, or `Unreachable`; a block that falls off the
        // end is a lowering bug and is reported rather than silently accepted.
        let mut current = self.entry;
        loop {
            let block = &self.blocks[current as usize];
            let mut next: Option<(u32, Vec<u32>)> = None;
            for op in &block.ops {
                match op {
                    BaselineOp::ConstI32 { slot, value } => {
                        slots[*slot as usize] = Some(Value::I32(*value));
                    }
                    BaselineOp::I32Binary {
                        result,
                        left,
                        right,
                        operation,
                    } => {
                        let left = i32_slot(&slots, *left)?;
                        let right = i32_slot(&slots, *right)?;
                        let value = eval_i32_binary(*operation, left, right)?;
                        slots[*result as usize] = Some(Value::I32(value));
                    }
                    BaselineOp::I32Eqz { result, value } => {
                        let value = i32_slot(&slots, *value)?;
                        slots[*result as usize] = Some(Value::I32((value == 0) as i32));
                    }
                    BaselineOp::I32Compare {
                        result,
                        left,
                        right,
                        comparison,
                    } => {
                        let left = i32_slot(&slots, *left)?;
                        let right = i32_slot(&slots, *right)?;
                        slots[*result as usize] =
                            Some(Value::I32(compare_i32(left, right, *comparison)));
                    }
                    BaselineOp::ConstI64 { slot, value } => {
                        slots[*slot as usize] = Some(Value::I64(*value));
                    }
                    BaselineOp::I64Binary {
                        result,
                        left,
                        right,
                        operation,
                    } => {
                        let left = i64_slot(&slots, *left)?;
                        let right = i64_slot(&slots, *right)?;
                        let value = eval_i64_binary(*operation, left, right)?;
                        slots[*result as usize] = Some(Value::I64(value));
                    }
                    BaselineOp::I64Eqz { result, value } => {
                        let value = i64_slot(&slots, *value)?;
                        slots[*result as usize] = Some(Value::I32((value == 0) as i32));
                    }
                    BaselineOp::I64Compare {
                        result,
                        left,
                        right,
                        comparison,
                    } => {
                        let left = i64_slot(&slots, *left)?;
                        let right = i64_slot(&slots, *right)?;
                        slots[*result as usize] =
                            Some(Value::I32(compare_i64(left, right, *comparison)));
                    }
                    BaselineOp::ConstF32 { slot, value } => {
                        slots[*slot as usize] = Some(Value::F32(*value));
                    }
                    BaselineOp::F32Binary {
                        result,
                        left,
                        right,
                        operation,
                    } => {
                        let left = f32_slot(&slots, *left)?;
                        let right = f32_slot(&slots, *right)?;
                        slots[*result as usize] =
                            Some(Value::F32(eval_f32_binary(*operation, left, right)));
                    }
                    BaselineOp::F32Unary {
                        result,
                        value,
                        operation,
                    } => {
                        let value = f32_slot(&slots, *value)?;
                        slots[*result as usize] =
                            Some(Value::F32(eval_f32_unary(*operation, value)));
                    }
                    BaselineOp::F32Compare {
                        result,
                        left,
                        right,
                        comparison,
                    } => {
                        let left = f32_slot(&slots, *left)?;
                        let right = f32_slot(&slots, *right)?;
                        slots[*result as usize] =
                            Some(Value::I32(compare_f32(left, right, *comparison) as i32));
                    }
                    BaselineOp::ConstF64 { slot, value } => {
                        slots[*slot as usize] = Some(Value::F64(*value));
                    }
                    BaselineOp::F64Binary {
                        result,
                        left,
                        right,
                        operation,
                    } => {
                        let left = f64_slot(&slots, *left)?;
                        let right = f64_slot(&slots, *right)?;
                        slots[*result as usize] =
                            Some(Value::F64(eval_f64_binary(*operation, left, right)));
                    }
                    BaselineOp::F64Unary {
                        result,
                        value,
                        operation,
                    } => {
                        let value = f64_slot(&slots, *value)?;
                        slots[*result as usize] =
                            Some(Value::F64(eval_f64_unary(*operation, value)));
                    }
                    BaselineOp::F64Compare {
                        result,
                        left,
                        right,
                        comparison,
                    } => {
                        let left = f64_slot(&slots, *left)?;
                        let right = f64_slot(&slots, *right)?;
                        slots[*result as usize] =
                            Some(Value::I32(compare_f64(left, right, *comparison) as i32));
                    }
                    BaselineOp::Drop { value } => {
                        // The operand was already evaluated; a straight-line `drop`
                        // only ends its live range, so the slot is left untouched.
                        let _ = read_slot(&slots, *value)?;
                    }
                    BaselineOp::Select {
                        result,
                        condition,
                        left,
                        right,
                    } => {
                        let condition = i32_slot(&slots, *condition)?;
                        let selected = if condition != 0 {
                            read_slot(&slots, *left)?
                        } else {
                            read_slot(&slots, *right)?
                        };
                        slots[*result as usize] = Some(selected);
                    }
                    BaselineOp::LocalGet { result, local } => {
                        slots[*result as usize] = Some(read_slot(&slots, *local)?);
                    }
                    BaselineOp::LocalSet { local, value } => {
                        slots[*local as usize] = Some(read_slot(&slots, *value)?);
                    }
                    BaselineOp::LocalTee {
                        result,
                        local,
                        value,
                    } => {
                        let value = read_slot(&slots, *value)?;
                        slots[*local as usize] = Some(value.clone());
                        slots[*result as usize] = Some(value);
                    }
                    BaselineOp::I32Unary {
                        result,
                        value,
                        operation,
                    } => {
                        let value = i32_slot(&slots, *value)?;
                        slots[*result as usize] =
                            Some(Value::I32(eval_i32_unary(*operation, value)));
                    }
                    BaselineOp::I64Unary {
                        result,
                        value,
                        operation,
                    } => {
                        let value = i64_slot(&slots, *value)?;
                        slots[*result as usize] =
                            Some(Value::I64(eval_i64_unary(*operation, value)));
                    }
                    BaselineOp::IntConvert {
                        result,
                        value,
                        operation,
                    } => {
                        slots[*result as usize] =
                            Some(eval_int_convert(&slots, *operation, *value)?);
                    }
                    BaselineOp::Reinterpret {
                        result,
                        value,
                        operation,
                    } => {
                        slots[*result as usize] =
                            Some(eval_reinterpret(&slots, *operation, *value)?);
                    }
                    BaselineOp::FloatConvert {
                        result,
                        value,
                        operation,
                    } => {
                        slots[*result as usize] =
                            Some(eval_float_convert(&slots, *operation, *value)?);
                    }
                    BaselineOp::FloatTrunc {
                        result,
                        value,
                        operation,
                    } => {
                        slots[*result as usize] =
                            Some(eval_float_trunc(&slots, *operation, *value)?);
                    }
                    BaselineOp::Return(results) => {
                        return results
                            .iter()
                            .map(|slot| {
                                slots
                                    .get(*slot as usize)
                                    .and_then(Clone::clone)
                                    .ok_or_else(|| {
                                        BaselineError::Trap(Trap::HostFailure(
                                            "baseline return slot is empty".into(),
                                        ))
                                    })
                            })
                            .collect();
                    }
                    BaselineOp::Trap(trap) => return Err(BaselineError::Trap(trap.clone())),
                    BaselineOp::Unreachable => return Err(BaselineError::Trap(Trap::Unreachable)),
                    BaselineOp::Load {
                        result,
                        address,
                        offset,
                        operation,
                    } => {
                        let memory = state.memory.as_deref_mut().ok_or_else(missing_memory)?;
                        let effective = effective_address(&slots, *address, *offset)?;
                        let bits = memory.read(effective, u64::from(operation.width()))?;
                        slots[*result as usize] = Some(decode_load(*operation, bits));
                    }
                    BaselineOp::Store {
                        address,
                        value,
                        offset,
                        operation,
                    } => {
                        let memory = state.memory.as_deref_mut().ok_or_else(missing_memory)?;
                        let effective = effective_address(&slots, *address, *offset)?;
                        let bits = value_bits(&slots, *value)?;
                        memory.write(effective, u64::from(operation.width()), bits)?;
                    }
                    BaselineOp::MemorySize { result } => {
                        let memory = state.memory.as_deref_mut().ok_or_else(missing_memory)?;
                        slots[*result as usize] = Some(Value::I32(memory.pages() as i32));
                    }
                    BaselineOp::MemoryGrow { result, delta } => {
                        let memory = state.memory.as_deref_mut().ok_or_else(missing_memory)?;
                        let delta = i32_slot(&slots, *delta)? as u32 as u64;
                        slots[*result as usize] = Some(Value::I32(memory.grow(delta)));
                    }
                    BaselineOp::GlobalGet { result, global } => {
                        let value = state
                            .globals
                            .get(*global as usize)
                            .ok_or_else(|| {
                                Trap::HostFailure("baseline global index is out of range".into())
                            })?
                            .clone();
                        slots[*result as usize] = Some(value);
                    }
                    BaselineOp::GlobalSet { global, value } => {
                        let stored = read_slot(&slots, *value)?;
                        let slot = state.globals.get_mut(*global as usize).ok_or_else(|| {
                            Trap::HostFailure("baseline global index is out of range".into())
                        })?;
                        *slot = stored;
                    }
                    BaselineOp::RefNull {
                        slot,
                        reference_type,
                    } => {
                        slots[*slot as usize] =
                            Some(Value::Ref(tpt_wasm_types::RefValue::Null(*reference_type)));
                    }
                    BaselineOp::RefFunc { slot, function } => {
                        // The index addresses this module's function table, the
                        // same space a direct `Call` dispatches through. A `Call`
                        // resolves its target at run time and reports an index
                        // outside the module as a host failure, so a reference to
                        // a function this module does not carry is refused the
                        // same way rather than producing a dangling reference.
                        if usize::try_from(*function)
                            .ok()
                            .and_then(|index| state.functions.get(index))
                            .is_none()
                        {
                            return Err(BaselineError::Trap(Trap::HostFailure(
                                "baseline ref.func target is not in the module".into(),
                            )));
                        }
                        slots[*slot as usize] =
                            Some(Value::Ref(tpt_wasm_types::RefValue::FuncRef(*function)));
                    }
                    BaselineOp::RefIsNull { result, value } => {
                        let reference = read_slot(&slots, *value)?;
                        let is_null = match reference {
                            Value::Ref(tpt_wasm_types::RefValue::Null(_)) => true,
                            Value::Ref(tpt_wasm_types::RefValue::FuncRef(_))
                            | Value::Ref(tpt_wasm_types::RefValue::ExternRef(_)) => false,
                            other => {
                                return Err(BaselineError::Trap(Trap::HostFailure(format!(
                                    "baseline ref.is_null operand is not a reference: {other:?}"
                                ))))
                            }
                        };
                        slots[*result as usize] = Some(Value::I32(is_null as i32));
                    }
                    BaselineOp::Branch { target, values } => {
                        next = Some((*target, values.clone()));
                        break;
                    }
                    BaselineOp::CondBranch {
                        condition,
                        then_target,
                        then_values,
                        else_target,
                        else_values,
                    } => {
                        let condition = i32_slot(&slots, *condition)?;
                        next = Some(if condition != 0 {
                            (*then_target, then_values.clone())
                        } else {
                            (*else_target, else_values.clone())
                        });
                        break;
                    }
                    BaselineOp::Call {
                        function,
                        arguments,
                        results,
                    } => {
                        // An index outside the table means the function table does
                        // not match the one this function was lowered against.
                        let callee = state.functions.get(*function as usize).ok_or_else(|| {
                            Trap::HostFailure("baseline call target is not in the module".into())
                        })?;
                        if callee.params.len() != arguments.len() {
                            return Err(BaselineError::Trap(Trap::HostFailure(
                                "baseline call argument arity mismatch".into(),
                            )));
                        }
                        let mut call_args = Vec::with_capacity(arguments.len());
                        for slot in arguments {
                            call_args.push(read_slot(&slots, *slot)?);
                        }
                        let returned = callee.run(state, call_args)?;
                        if returned.len() != results.len() {
                            return Err(BaselineError::Trap(Trap::HostFailure(
                                "baseline call result arity mismatch".into(),
                            )));
                        }
                        for (slot, value) in results.iter().copied().zip(returned) {
                            check_value_type(&value, self.value_types[slot as usize])?;
                            slots[slot as usize] = Some(value);
                        }
                    }
                    BaselineOp::CallHost {
                        import,
                        arguments,
                        results,
                    } => {
                        let declaration = state.imports.get(*import as usize).ok_or_else(|| {
                            Trap::HostFailure("baseline host import is out of range".into())
                        })?;
                        let qualified = format!("{}::{}", declaration.module, declaration.name);
                        // No boundary means the embedder granted no host
                        // capability, so the call is refused rather than
                        // resolved by guesswork. That is a host refusal rather
                        // than the module trapping, so it is reported as one.
                        let host = state.host.as_deref_mut().ok_or_else(|| {
                            BaselineError::Host(HostCallFailure {
                                name: qualified.clone(),
                                message: "no host boundary was installed".into(),
                            })
                        })?;
                        let mut call_args = Vec::with_capacity(arguments.len());
                        for slot in arguments {
                            call_args.push(read_slot(&slots, *slot)?);
                        }
                        let returned =
                            host.call(&declaration.module, &declaration.name, call_args)?;
                        // The import's declared result arity and types are
                        // re-checked here: the host is outside the verified
                        // module, so its return is not trusted. A host that
                        // breaks its declared signature is reported as a host
                        // failure, matching how the interpreter reports one.
                        if returned.len() != results.len() {
                            return Err(BaselineError::Host(HostCallFailure {
                                name: qualified.clone(),
                                message: format!(
                                    "host returned {} results, the import declares {}",
                                    returned.len(),
                                    results.len()
                                ),
                            }));
                        }
                        for (position, expected) in
                            declaration.function_type.results.0.iter().enumerate()
                        {
                            check_value_type(&returned[position], *expected).map_err(|trap| {
                                BaselineError::Host(HostCallFailure {
                                    name: qualified.clone(),
                                    message: Trap::HostFailure(format!(
                                        "host returned the wrong type for a result: {trap}"
                                    ))
                                    .to_string(),
                                })
                            })?;
                        }
                        for (slot, value) in results.iter().copied().zip(returned) {
                            slots[slot as usize] = Some(value);
                        }
                    }
                    BaselineOp::CallIndirect {
                        type_index,
                        operand,
                        arguments,
                        results,
                    } => {
                        let table = state.table.ok_or_else(|| {
                            Trap::HostFailure("baseline indirect call needs a table".into())
                        })?;
                        let index = i32_slot(&slots, *operand)? as u32;
                        // Out of range and null are distinct traps, matching the
                        // order in which the reference is resolved.
                        let target = table
                            .elements
                            .get(index as usize)
                            .copied()
                            .ok_or(Trap::TableOutOfBounds)?
                            .ok_or(Trap::NullReference)?;
                        let callee = state.functions.get(target as usize).ok_or_else(|| {
                            Trap::HostFailure(
                                "baseline indirect target is not in the module".into(),
                            )
                        })?;
                        // The signature is checked against the entry the table
                        // holds, not against the type index the call names, since
                        // a mismatch is exactly the trap we are modelling.
                        let expected = state.types.get(*type_index as usize).ok_or_else(|| {
                            Trap::HostFailure("baseline indirect call type is unknown".into())
                        })?;
                        if &callee.function_type != expected {
                            return Err(BaselineError::Trap(Trap::IndirectCallTypeMismatch));
                        }
                        let mut call_args = Vec::with_capacity(arguments.len());
                        for slot in arguments {
                            call_args.push(read_slot(&slots, *slot)?);
                        }
                        let returned = callee.run(state, call_args)?;
                        if returned.len() != results.len() {
                            return Err(BaselineError::Trap(Trap::HostFailure(
                                "baseline indirect call result arity mismatch".into(),
                            )));
                        }
                        for (slot, value) in results.iter().copied().zip(returned) {
                            check_value_type(&value, self.value_types[slot as usize])?;
                            slots[slot as usize] = Some(value);
                        }
                    }
                }
            }
            // Bind the incoming values to the target block's parameter slots,
            // then continue from that block.
            let Some((target, values)) = next else {
                return Err(BaselineError::Trap(Trap::HostFailure(
                    "baseline block has no terminator".into(),
                )));
            };
            let target_block = &self.blocks[target as usize];
            if target_block.params.len() != values.len() {
                return Err(BaselineError::Trap(Trap::HostFailure(
                    "baseline branch arity does not match target block".into(),
                )));
            }
            for (param, value) in target_block.params.iter().copied().zip(&values) {
                let value = slots
                    .get(*value as usize)
                    .and_then(Clone::clone)
                    .ok_or_else(|| {
                        Trap::HostFailure("baseline branch operand slot is empty".into())
                    })?;
                slots[param as usize] = Some(value);
            }
            current = target;
        }
    }
}

fn eval_i32_binary(operation: I32BinaryOp, left: i32, right: i32) -> Result<i32, Trap> {
    let value = match operation {
        I32BinaryOp::Add => left.wrapping_add(right),
        I32BinaryOp::Sub => left.wrapping_sub(right),
        I32BinaryOp::Mul => left.wrapping_mul(right),
        I32BinaryOp::DivS => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            if left == i32::MIN && right == -1 {
                return Err(Trap::IntegerOverflow);
            }
            left.wrapping_div(right)
        }
        I32BinaryOp::DivU => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            ((left as u32) / (right as u32)) as i32
        }
        I32BinaryOp::RemS => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            left.wrapping_rem(right)
        }
        I32BinaryOp::RemU => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            ((left as u32) % (right as u32)) as i32
        }
        I32BinaryOp::Shl => left.wrapping_shl(right as u32),
        I32BinaryOp::ShrS => left.wrapping_shr(right as u32),
        I32BinaryOp::ShrU => ((left as u32).wrapping_shr(right as u32)) as i32,
        I32BinaryOp::Rotl => left.rotate_left((right as u32) & 31),
        I32BinaryOp::Rotr => left.rotate_right((right as u32) & 31),
        I32BinaryOp::And => left & right,
        I32BinaryOp::Or => left | right,
        I32BinaryOp::Xor => left ^ right,
    };
    Ok(value)
}

fn compare_i32(left: i32, right: i32, comparison: tpt_wasm_ir::IntComparison) -> i32 {
    let result = match comparison {
        tpt_wasm_ir::IntComparison::Eq => left == right,
        tpt_wasm_ir::IntComparison::Ne => left != right,
        tpt_wasm_ir::IntComparison::LtS => left < right,
        tpt_wasm_ir::IntComparison::LtU => (left as u32) < (right as u32),
        tpt_wasm_ir::IntComparison::GtS => left > right,
        tpt_wasm_ir::IntComparison::GtU => (left as u32) > (right as u32),
        tpt_wasm_ir::IntComparison::LeS => left <= right,
        tpt_wasm_ir::IntComparison::LeU => (left as u32) <= (right as u32),
        tpt_wasm_ir::IntComparison::GeS => left >= right,
        tpt_wasm_ir::IntComparison::GeU => (left as u32) >= (right as u32),
    };
    result as i32
}

fn eval_i64_binary(operation: I64BinaryOp, left: i64, right: i64) -> Result<i64, Trap> {
    let value = match operation {
        I64BinaryOp::Add => left.wrapping_add(right),
        I64BinaryOp::Sub => left.wrapping_sub(right),
        I64BinaryOp::Mul => left.wrapping_mul(right),
        I64BinaryOp::DivS => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            if left == i64::MIN && right == -1 {
                return Err(Trap::IntegerOverflow);
            }
            left.wrapping_div(right)
        }
        I64BinaryOp::DivU => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            ((left as u64) / (right as u64)) as i64
        }
        I64BinaryOp::RemS => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            left.wrapping_rem(right)
        }
        I64BinaryOp::RemU => {
            if right == 0 {
                return Err(Trap::IntegerDivisionByZero);
            }
            ((left as u64) % (right as u64)) as i64
        }
        I64BinaryOp::Shl => left.wrapping_shl((right as u64 & 63) as u32),
        I64BinaryOp::ShrS => left.wrapping_shr((right as u64 & 63) as u32),
        I64BinaryOp::ShrU => ((left as u64).wrapping_shr((right as u64 & 63) as u32)) as i64,
        I64BinaryOp::Rotl => left.rotate_left((right as u64 & 63) as u32),
        I64BinaryOp::Rotr => left.rotate_right((right as u64 & 63) as u32),
        I64BinaryOp::And => left & right,
        I64BinaryOp::Or => left | right,
        I64BinaryOp::Xor => left ^ right,
    };
    Ok(value)
}

fn compare_i64(left: i64, right: i64, comparison: tpt_wasm_ir::IntComparison) -> i32 {
    let result = match comparison {
        tpt_wasm_ir::IntComparison::Eq => left == right,
        tpt_wasm_ir::IntComparison::Ne => left != right,
        tpt_wasm_ir::IntComparison::LtS => left < right,
        tpt_wasm_ir::IntComparison::LtU => (left as u64) < (right as u64),
        tpt_wasm_ir::IntComparison::GtS => left > right,
        tpt_wasm_ir::IntComparison::GtU => (left as u64) > (right as u64),
        tpt_wasm_ir::IntComparison::LeS => left <= right,
        tpt_wasm_ir::IntComparison::LeU => (left as u64) <= (right as u64),
        tpt_wasm_ir::IntComparison::GeS => left >= right,
        tpt_wasm_ir::IntComparison::GeU => (left as u64) >= (right as u64),
    };
    result as i32
}

fn i64_slot(slots: &[Option<Value>], slot: u32) -> Result<i64, Trap> {
    match slots
        .get(slot as usize)
        .and_then(Clone::clone)
        .ok_or_else(|| Trap::HostFailure("baseline operand slot is empty".into()))?
    {
        Value::I64(value) => Ok(value),
        _ => Err(Trap::HostFailure(
            "baseline i64 operand type mismatch".into(),
        )),
    }
}

fn value_type(value: &Value) -> ValueType {
    match value {
        Value::I32(_) => ValueType::I32,
        Value::I64(_) => ValueType::I64,
        Value::F32(_) => ValueType::F32,
        Value::F64(_) => ValueType::F64,
        Value::V128(_) => ValueType::V128,
        Value::Ref(value) => ValueType::Ref(match value {
            tpt_wasm_types::RefValue::Null(kind) => *kind,
            tpt_wasm_types::RefValue::FuncRef(_) => tpt_wasm_types::RefType::FuncRef,
            tpt_wasm_types::RefValue::ExternRef(_) => tpt_wasm_types::RefType::ExternRef,
        }),
    }
}

fn check_value_type(value: &Value, expected: ValueType) -> Result<(), Trap> {
    if value_type(value) == expected {
        Ok(())
    } else {
        Err(Trap::HostFailure("baseline value type mismatch".into()))
    }
}

fn default_value(value_type: ValueType) -> Value {
    match value_type {
        ValueType::I32 => Value::I32(0),
        ValueType::I64 => Value::I64(0),
        ValueType::F32 => Value::F32(0),
        ValueType::F64 => Value::F64(0),
        ValueType::V128 => Value::V128(0),
        ValueType::Ref(kind) => Value::Ref(tpt_wasm_types::RefValue::Null(kind)),
    }
}

fn i32_slot(slots: &[Option<Value>], slot: u32) -> Result<i32, Trap> {
    match slots
        .get(slot as usize)
        .and_then(Clone::clone)
        .ok_or_else(|| Trap::HostFailure("baseline operand slot is empty".into()))?
    {
        Value::I32(value) => Ok(value),
        _ => Err(Trap::HostFailure(
            "baseline i32 operand type mismatch".into(),
        )),
    }
}

/// Read a slot whose value is already known to be the right type.
///
/// Type safety is established at lowering time, so this only has to reject a
/// slot that was never written; it is the typed-slot counterpart of the
/// `i32_slot`/`f32_slot` accessors.
fn read_slot(slots: &[Option<Value>], slot: u32) -> Result<Value, Trap> {
    slots
        .get(slot as usize)
        .and_then(Clone::clone)
        .ok_or_else(|| Trap::HostFailure("baseline operand slot is empty".into()))
}

/// The error used when a memory instruction runs without a module memory.
fn missing_memory() -> Trap {
    Trap::HostFailure("baseline memory instruction needs a module memory".into())
}

/// The effective byte address of a memory access, `address + offset`, checked.
fn effective_address(slots: &[Option<Value>], address: u32, offset: u32) -> Result<u64, Trap> {
    let base = i32_slot(slots, address)? as u32 as u64;
    base.checked_add(u64::from(offset))
        .ok_or(Trap::MemoryOutOfBounds)
}

/// The raw bits a store writes, taken from the low bytes of the value.
fn value_bits(slots: &[Option<Value>], value: u32) -> Result<u64, Trap> {
    Ok(match read_slot(slots, value)? {
        Value::I32(stored) => u64::from(stored as u32),
        Value::I64(stored) => stored as u64,
        Value::F32(stored) => u64::from(stored),
        Value::F64(stored) => stored,
        other => {
            return Err(Trap::HostFailure(format!(
                "baseline store operand is not a number: {other:?}"
            )))
        }
    })
}

/// Turn the little-endian bits a load read into the value the opcode produces,
/// applying the opcode's sign or zero extension.
fn decode_load(operation: tpt_wasm_ir::MemoryLoad, bits: u64) -> Value {
    use tpt_wasm_ir::MemoryLoad;
    match operation {
        MemoryLoad::I32 => Value::I32(bits as u32 as i32),
        MemoryLoad::I64 => Value::I64(bits as i64),
        // A float load is a pure bit reinterpretation, so a NaN payload survives.
        MemoryLoad::F32 => Value::F32(bits as u32),
        MemoryLoad::F64 => Value::F64(bits),
        MemoryLoad::I32_8S => Value::I32(((bits as u8) as i8) as i32),
        MemoryLoad::I32_8U => Value::I32(bits as u8 as i32),
        MemoryLoad::I32_16S => Value::I32(((bits as u16) as i16) as i32),
        MemoryLoad::I32_16U => Value::I32(bits as u16 as i32),
        MemoryLoad::I64_8S => Value::I64(((bits as u8) as i8) as i64),
        MemoryLoad::I64_8U => Value::I64(bits as u8 as i64),
        MemoryLoad::I64_16S => Value::I64(((bits as u16) as i16) as i64),
        MemoryLoad::I64_16U => Value::I64(bits as u16 as i64),
        MemoryLoad::I64_32S => Value::I64(((bits as u32) as i32) as i64),
        MemoryLoad::I64_32U => Value::I64(bits as u32 as i64),
    }
}

fn eval_i32_unary(operation: tpt_wasm_ir::IntUnary, value: i32) -> i32 {
    match operation {
        tpt_wasm_ir::IntUnary::Clz => value.leading_zeros() as i32,
        tpt_wasm_ir::IntUnary::Ctz => value.trailing_zeros() as i32,
        tpt_wasm_ir::IntUnary::Popcnt => value.count_ones() as i32,
    }
}

fn eval_i64_unary(operation: tpt_wasm_ir::IntUnary, value: i64) -> i64 {
    match operation {
        tpt_wasm_ir::IntUnary::Clz => value.leading_zeros() as i64,
        tpt_wasm_ir::IntUnary::Ctz => value.trailing_zeros() as i64,
        tpt_wasm_ir::IntUnary::Popcnt => value.count_ones() as i64,
    }
}

fn eval_int_convert(
    slots: &[Option<Value>],
    operation: tpt_wasm_ir::IntConversion,
    value: u32,
) -> Result<Value, Trap> {
    use tpt_wasm_ir::IntConversion;
    let result = match operation {
        // `i32.wrap_i64` keeps the low 32 bits, reinterpreted as signed.
        IntConversion::I32WrapI64 => Value::I32(i64_slot(slots, value)? as i32),
        IntConversion::I64ExtendI32S => Value::I64(i64::from(i32_slot(slots, value)?)),
        IntConversion::I64ExtendI32U => Value::I64(i64::from(i32_slot(slots, value)? as u32)),
    };
    Ok(result)
}

fn eval_reinterpret(
    slots: &[Option<Value>],
    operation: tpt_wasm_ir::Reinterpret,
    value: u32,
) -> Result<Value, Trap> {
    use tpt_wasm_ir::Reinterpret;
    // Reinterpretation moves raw bits and never inspects or changes them, so
    // NaN payloads and signed zeros survive untouched.
    let result = match operation {
        Reinterpret::I32FromF32 => Value::I32(f32_slot(slots, value)?.to_bits() as i32),
        Reinterpret::I64FromF64 => Value::I64(f64_slot(slots, value)?.to_bits() as i64),
        Reinterpret::F32FromI32 => Value::F32(i32_slot(slots, value)? as u32),
        Reinterpret::F64FromI64 => Value::F64(i64_slot(slots, value)? as u64),
    };
    Ok(result)
}

fn eval_float_convert(
    slots: &[Option<Value>],
    operation: tpt_wasm_ir::FloatConversion,
    value: u32,
) -> Result<Value, Trap> {
    use tpt_wasm_ir::FloatConversion;
    let result = match operation {
        // These conversions are total, but the result still goes through NaN
        // canonicalization so the result is bit-identical to Micro's.
        FloatConversion::F32FromI32S => Value::F32(f32_result(i32_slot(slots, value)? as f32)),
        FloatConversion::F32FromI32U => {
            Value::F32(f32_result((i32_slot(slots, value)? as u32) as f32))
        }
        FloatConversion::F32FromI64S => Value::F32(f32_result(i64_slot(slots, value)? as f32)),
        FloatConversion::F32FromI64U => {
            Value::F32(f32_result((i64_slot(slots, value)? as u64) as f32))
        }
        FloatConversion::F32FromF64 => Value::F32(f32_result(f64_slot(slots, value)? as f32)),
        FloatConversion::F64FromI32S => Value::F64(f64_result(i32_slot(slots, value)? as f64)),
        FloatConversion::F64FromI32U => {
            Value::F64(f64_result((i32_slot(slots, value)? as u32) as f64))
        }
        FloatConversion::F64FromI64S => Value::F64(f64_result(i64_slot(slots, value)? as f64)),
        FloatConversion::F64FromI64U => {
            Value::F64(f64_result((i64_slot(slots, value)? as u64) as f64))
        }
        FloatConversion::F64FromF32 => Value::F64(f64_result(f32_slot(slots, value)? as f64)),
    };
    Ok(result)
}

fn eval_float_trunc(
    slots: &[Option<Value>],
    operation: tpt_wasm_ir::FloatTrunc,
    value: u32,
) -> Result<Value, Trap> {
    use tpt_wasm_ir::FloatTrunc;
    // Truncation toward zero then traps unless the truncated value fits the
    // destination. NaN and infinity are rejected by the same finiteness check,
    // and every failure is `InvalidConversion` rather than a Rust panic.
    let invalid = || Trap::InvalidConversion;
    let result = match operation {
        FloatTrunc::I32FromF32S => {
            let value = f32_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if truncated < i32::MIN as f32 || truncated >= 2147483648.0f32 {
                return Err(invalid());
            }
            Value::I32(truncated as i32)
        }
        FloatTrunc::I32FromF32U => {
            let value = f32_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if !(0.0..4294967296.0f32).contains(&truncated) {
                return Err(invalid());
            }
            Value::I32(truncated as u32 as i32)
        }
        FloatTrunc::I32FromF64S => {
            let value = f64_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if truncated < i32::MIN as f64 || truncated >= 2147483648.0f64 {
                return Err(invalid());
            }
            Value::I32(truncated as i32)
        }
        FloatTrunc::I32FromF64U => {
            let value = f64_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if !(0.0..4294967296.0f64).contains(&truncated) {
                return Err(invalid());
            }
            Value::I32(truncated as u32 as i32)
        }
        FloatTrunc::I64FromF32S => {
            let value = f32_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if !(-9223372036854775808.0f32..9223372036854775808.0f32).contains(&truncated) {
                return Err(invalid());
            }
            Value::I64(truncated as i64)
        }
        FloatTrunc::I64FromF32U => {
            let value = f32_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if !(0.0..18446744073709551616.0f32).contains(&truncated) {
                return Err(invalid());
            }
            Value::I64(truncated as u64 as i64)
        }
        FloatTrunc::I64FromF64S => {
            let value = f64_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if !(-9223372036854775808.0f64..9223372036854775808.0f64).contains(&truncated) {
                return Err(invalid());
            }
            Value::I64(truncated as i64)
        }
        FloatTrunc::I64FromF64U => {
            let value = f64_slot(slots, value)?;
            if !value.is_finite() {
                return Err(invalid());
            }
            let truncated = value.trunc();
            if !(0.0..18446744073709551616.0f64).contains(&truncated) {
                return Err(invalid());
            }
            Value::I64(truncated as u64 as i64)
        }
    };
    Ok(result)
}

/// The canonical NaN bit patterns produced by arithmetic IEEE 754 operations.
const F32_CANONICAL_NAN: u32 = 0x7fc0_0000;
const F64_CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

/// Canonicalize any NaN result, preserving every other bit pattern exactly.
///
/// Wasm permits non-deterministic NaN payloads; this backend fixes on the
/// canonical quiet NaN, matching Micro so differential tests compare
/// bit-for-bit rather than by IEEE equality.
fn f32_result(value: f32) -> u32 {
    if value.is_nan() {
        F32_CANONICAL_NAN
    } else {
        value.to_bits()
    }
}

fn f64_result(value: f64) -> u64 {
    if value.is_nan() {
        F64_CANONICAL_NAN
    } else {
        value.to_bits()
    }
}

fn f32_slot(slots: &[Option<Value>], slot: u32) -> Result<f32, Trap> {
    match slots
        .get(slot as usize)
        .and_then(Clone::clone)
        .ok_or_else(|| Trap::HostFailure("baseline operand slot is empty".into()))?
    {
        Value::F32(bits) => Ok(f32::from_bits(bits)),
        _ => Err(Trap::HostFailure(
            "baseline f32 operand type mismatch".into(),
        )),
    }
}

fn f64_slot(slots: &[Option<Value>], slot: u32) -> Result<f64, Trap> {
    match slots
        .get(slot as usize)
        .and_then(Clone::clone)
        .ok_or_else(|| Trap::HostFailure("baseline operand slot is empty".into()))?
    {
        Value::F64(bits) => Ok(f64::from_bits(bits)),
        _ => Err(Trap::HostFailure(
            "baseline f64 operand type mismatch".into(),
        )),
    }
}

/// `f32.min`: NaN propagates, and `min(-0, +0)` is `-0`.
fn f32_min(left: f32, right: f32) -> f32 {
    if left.is_nan() || right.is_nan() {
        f32::from_bits(F32_CANONICAL_NAN)
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else if left < right {
        left
    } else {
        right
    }
}

/// `f32.max`: NaN propagates, and `max(-0, +0)` is `+0`.
fn f32_max(left: f32, right: f32) -> f32 {
    if left.is_nan() || right.is_nan() {
        f32::from_bits(F32_CANONICAL_NAN)
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_positive() || right.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else if left > right {
        left
    } else {
        right
    }
}

fn f64_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::from_bits(F64_CANONICAL_NAN)
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else if left < right {
        left
    } else {
        right
    }
}

fn f64_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::from_bits(F64_CANONICAL_NAN)
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_positive() || right.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else if left > right {
        left
    } else {
        right
    }
}

fn eval_f32_binary(operation: F32BinaryOp, left: f32, right: f32) -> u32 {
    match operation {
        // `copysign` is a bit operation and must not canonicalize the NaN
        // payload; the arithmetic operations must.
        F32BinaryOp::Copysign => f32::copysign(left, right).to_bits(),
        F32BinaryOp::Add => f32_result(left + right),
        F32BinaryOp::Sub => f32_result(left - right),
        F32BinaryOp::Mul => f32_result(left * right),
        F32BinaryOp::Div => f32_result(left / right),
        F32BinaryOp::Min => f32_result(f32_min(left, right)),
        F32BinaryOp::Max => f32_result(f32_max(left, right)),
    }
}

fn eval_f64_binary(operation: F64BinaryOp, left: f64, right: f64) -> u64 {
    match operation {
        F64BinaryOp::Copysign => f64::copysign(left, right).to_bits(),
        F64BinaryOp::Add => f64_result(left + right),
        F64BinaryOp::Sub => f64_result(left - right),
        F64BinaryOp::Mul => f64_result(left * right),
        F64BinaryOp::Div => f64_result(left / right),
        F64BinaryOp::Min => f64_result(f64_min(left, right)),
        F64BinaryOp::Max => f64_result(f64_max(left, right)),
    }
}

fn eval_f32_unary(operation: tpt_wasm_ir::FloatUnary, value: f32) -> u32 {
    match operation {
        // `abs` and `neg` only manipulate the sign bit and preserve payloads.
        tpt_wasm_ir::FloatUnary::Abs => value.to_bits() & 0x7fff_ffff,
        tpt_wasm_ir::FloatUnary::Neg => value.to_bits() ^ 0x8000_0000,
        tpt_wasm_ir::FloatUnary::Ceil => f32_result(value.ceil()),
        tpt_wasm_ir::FloatUnary::Floor => f32_result(value.floor()),
        tpt_wasm_ir::FloatUnary::Trunc => f32_result(value.trunc()),
        tpt_wasm_ir::FloatUnary::Nearest => f32_result(value.round_ties_even()),
        tpt_wasm_ir::FloatUnary::Sqrt => f32_result(value.sqrt()),
    }
}

fn eval_f64_unary(operation: tpt_wasm_ir::FloatUnary, value: f64) -> u64 {
    match operation {
        tpt_wasm_ir::FloatUnary::Abs => value.to_bits() & 0x7fff_ffff_ffff_ffff,
        tpt_wasm_ir::FloatUnary::Neg => value.to_bits() ^ 0x8000_0000_0000_0000,
        tpt_wasm_ir::FloatUnary::Ceil => f64_result(value.ceil()),
        tpt_wasm_ir::FloatUnary::Floor => f64_result(value.floor()),
        tpt_wasm_ir::FloatUnary::Trunc => f64_result(value.trunc()),
        tpt_wasm_ir::FloatUnary::Nearest => f64_result(value.round_ties_even()),
        tpt_wasm_ir::FloatUnary::Sqrt => f64_result(value.sqrt()),
    }
}

fn compare_f32(left: f32, right: f32, comparison: tpt_wasm_ir::FloatComparison) -> bool {
    match comparison {
        tpt_wasm_ir::FloatComparison::Eq => left == right,
        tpt_wasm_ir::FloatComparison::Ne => left != right,
        tpt_wasm_ir::FloatComparison::Lt => left < right,
        tpt_wasm_ir::FloatComparison::Gt => left > right,
        tpt_wasm_ir::FloatComparison::Le => left <= right,
        tpt_wasm_ir::FloatComparison::Ge => left >= right,
    }
}

fn compare_f64(left: f64, right: f64, comparison: tpt_wasm_ir::FloatComparison) -> bool {
    match comparison {
        tpt_wasm_ir::FloatComparison::Eq => left == right,
        tpt_wasm_ir::FloatComparison::Ne => left != right,
        tpt_wasm_ir::FloatComparison::Lt => left < right,
        tpt_wasm_ir::FloatComparison::Gt => left > right,
        tpt_wasm_ir::FloatComparison::Le => left <= right,
        tpt_wasm_ir::FloatComparison::Ge => left >= right,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lower_function, lower_module, BaselineError, BaselineModule, BaselineOp, CodegenError,
        I32BinaryOp,
    };
    use tpt_wasm_format::{Element, ElementMode, Function, Module, Table};
    use tpt_wasm_ir::{
        lower_and_verify, BasicBlock, BlockId, IrFunction, IrInstr, IrValue, Terminator, ValueId,
    };
    use tpt_wasm_micro::instr::decode_body;
    use tpt_wasm_micro::machine::{Frame, Machine, Step};
    use tpt_wasm_micro::store::{Instance, Store};
    use tpt_wasm_types::{
        FunctionType, RefValue, ReferenceType, ResultType, Trap, Value, ValueType,
    };

    fn add_function() -> IrFunction {
        IrFunction {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            params: Vec::new(),
            locals: Vec::new(),
            values: vec![
                IrValue {
                    id: ValueId(0),
                    value_type: ValueType::I32,
                },
                IrValue {
                    id: ValueId(1),
                    value_type: ValueType::I32,
                },
                IrValue {
                    id: ValueId(2),
                    value_type: ValueType::I32,
                },
            ],
            entry: BlockId(0),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instrs: vec![
                    IrInstr::ConstI32 {
                        result: ValueId(0),
                        value: 20,
                    },
                    IrInstr::ConstI32 {
                        result: ValueId(1),
                        value: 22,
                    },
                    IrInstr::I32Add {
                        result: ValueId(2),
                        left: ValueId(0),
                        right: ValueId(1),
                    },
                ],
                terminator: Terminator::Return(vec![ValueId(2)]),
            }],
        }
    }

    #[test]
    fn lowers_deterministically_and_executes_without_micro() {
        let first = lower_function(&add_function()).unwrap();
        let second = lower_function(&add_function()).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.blocks[0].ops[0],
            BaselineOp::ConstI32 { slot: 0, value: 20 }
        );
        assert_eq!(
            first.blocks[0].ops[2],
            BaselineOp::I32Binary {
                result: 2,
                left: 0,
                right: 1,
                operation: I32BinaryOp::Add,
            }
        );
        assert_eq!(first.execute(Vec::new()).unwrap(), vec![Value::I32(42)]);
    }

    /// Run one body in Micro with the given local declarations, mirroring the
    /// zero-initialization the baseline backend performs for its local slots.
    fn execute_micro_with_locals(
        body: &[u8],
        arity: usize,
        locals: &[tpt_wasm_format::LocalDecl],
    ) -> Result<Vec<Value>, Trap> {
        let instructions = decode_body(body).unwrap();
        let mut frame_locals = Vec::new();
        for declaration in locals {
            for _ in 0..declaration.count {
                frame_locals.push(match declaration.value_type {
                    ValueType::I32 => Value::I32(0),
                    ValueType::I64 => Value::I64(0),
                    ValueType::F32 => Value::F32(0),
                    ValueType::F64 => Value::F64(0),
                    other => panic!("unsupported local type in fixture: {other:?}"),
                });
            }
        }
        let mut machine = Machine::new();
        machine
            .push_frame(Frame::new(0, 0, frame_locals, instructions, arity))
            .unwrap();
        match machine.run() {
            Step::Return(values) => Ok(values),
            Step::Trap(trap) => Err(trap),
            other => panic!("unexpected Micro result: {other:?}"),
        }
    }

    /// Lower one straight-line Wasm body that declares locals, through the real
    /// validate → IR → baseline pipeline.
    fn lower_body_with_locals(
        body: &[u8],
        result: ValueType,
        locals: &[tpt_wasm_format::LocalDecl],
    ) -> super::BaselineFunction {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![result]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: locals.to_vec(),
                body: body.to_vec(),
            }],
            ..Module::default()
        };
        let validated = tpt_wasm_validate::validate(module).expect("fixture must validate");
        let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
        lower_function(&verified.module().functions[0]).expect("codegen must accept the slice")
    }

    /// Assert the baseline backend and the Micro interpreter agree.
    fn assert_matches_micro(body: &[u8], result: ValueType) -> Result<Vec<Value>, Trap> {
        assert_matches_micro_with_locals(body, result, &[])
    }

    /// Assert agreement for a body that declares locals.
    fn assert_matches_micro_with_locals(
        body: &[u8],
        result: ValueType,
        locals: &[tpt_wasm_format::LocalDecl],
    ) -> Result<Vec<Value>, Trap> {
        let baseline = lower_body_with_locals(body, result, locals);
        let baseline_result = baseline.execute(Vec::new());
        let micro_result = execute_micro_with_locals(body, 1, locals);
        // The two backends report a trap through different types: Micro returns
        // the trap, the baseline wraps it so a failed host call stays distinct.
        // The trap itself has to be the same, which is what is compared here.
        let from_baseline: Result<&[Value], Trap> = match &baseline_result {
            Ok(values) => Ok(values),
            Err(error) => Err(trap_of(error)),
        };
        let from_micro: Result<&[Value], Trap> = match &micro_result {
            Ok(values) => Ok(values),
            Err(trap) => Err(trap.clone()),
        };
        assert_eq!(
            from_baseline, from_micro,
            "baseline and Micro diverged for {body:02x?}"
        );
        baseline_result.map_err(|error| trap_of(&error))
    }

    /// Reduce a baseline failure to the trap a caller observes, so the two
    /// backends' error shapes do not have to match in order to be compared.
    fn trap_of(error: &BaselineError) -> Trap {
        match error {
            BaselineError::Trap(trap) => trap.clone(),
            BaselineError::Host(failure) => Trap::HostFailure(failure.message.clone()),
        }
    }

    /// Build a two-operand `i32` body: `i32.const left; i32.const right; op; end`.
    fn binary_body(left: i32, right: i32, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x41];
        body.extend(encode_i32(left));
        body.push(0x41);
        body.extend(encode_i32(right));
        body.push(opcode);
        body.push(0x0b);
        body
    }

    /// Build a one-operand `i32` body: `i32.const value; op; end`.
    fn unary_body(value: i32, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x41];
        body.extend(encode_i32(value));
        body.push(opcode);
        body.push(0x0b);
        body
    }

    /// Build a two-operand `i64` body: `i64.const left; i64.const right; op; end`.
    fn binary_body_i64(left: i64, right: i64, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x42];
        body.extend(encode_i64(left));
        body.push(0x42);
        body.extend(encode_i64(right));
        body.push(opcode);
        body.push(0x0b);
        body
    }

    /// Build a one-operand `i64` body: `i64.const value; op; end`.
    fn unary_body_i64(value: i64, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x42];
        body.extend(encode_i64(value));
        body.push(opcode);
        body.push(0x0b);
        body
    }

    /// Encode a single `i64.const` signed LEB128 immediate.
    fn encode_i64(value: i64) -> Vec<u8> {
        let mut remaining = value;
        let mut out = Vec::new();
        loop {
            let byte = (remaining as u8) & 0x7f;
            remaining >>= 7;
            let sign = byte & 0x40 != 0;
            if (remaining == 0 && !sign) || (remaining == -1 && sign) {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out
    }

    #[test]
    fn baseline_matches_micro_for_every_supported_i64_binary_op() {
        // Mirrors the i32 coverage at 64-bit width, including 6-bit shift and
        // rotation masking and the i64 division trap identities.
        let cases: &[(i64, i64, u8)] = &[
            (20, 22, 0x7c),
            (20, 22, 0x7d),
            (6, 7, 0x7e),
            (i64::MAX, 1, 0x7c),
            (i64::MIN, -1, 0x7d),
            (7, 2, 0x7f),
            (-7, 2, 0x7f),
            (-7, -2, 0x7f),
            (7, -2, 0x7f),
            (-1, 2, 0x80),
            (-1, -2, 0x80),
            (-7, 2, 0x81),
            (-7, -2, 0x81),
            (-1, 2, 0x82),
            (1, 65, 0x86),
            (1, 65, 0x87),
            (1, 65, 0x88),
            (i64::MIN, 1, 0x88),
            (0b1010, 0b0110, 0x83),
            (0b1010, 0b0110, 0x84),
            (0b1010, 0b0110, 0x85),
            (1, 1, 0x89),
            (1, 65, 0x89),
            (1, 1, 0x8a),
            (1, 65, 0x8a),
        ];
        for (left, right, opcode) in cases {
            let _ = assert_matches_micro(&binary_body_i64(*left, *right, *opcode), ValueType::I64);
        }
    }

    #[test]
    fn baseline_reproduces_every_i64_division_trap() {
        for opcode in [0x7f, 0x80, 0x81, 0x82] {
            assert_eq!(
                assert_matches_micro(&binary_body_i64(1, 0, opcode), ValueType::I64),
                Err(Trap::IntegerDivisionByZero)
            );
        }
        assert_eq!(
            assert_matches_micro(&binary_body_i64(i64::MIN, -1, 0x7f), ValueType::I64),
            Err(Trap::IntegerOverflow)
        );
        assert_eq!(
            assert_matches_micro(&binary_body_i64(i64::MIN, -1, 0x81), ValueType::I64),
            Ok(vec![Value::I64(0)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_i64_eqz_and_every_i64_comparison() {
        // 0x50 is i64.eqz (unary); i64 comparisons are 0x51..=0x5a.
        // Every Wasm comparison yields an i32 result regardless of operand width.
        for opcode in 0x51..=0x5a {
            for (left, right) in [(0i64, 0i64), (-1, 1), (1, -1), (i64::MIN, -1)] {
                let _ = assert_matches_micro(&binary_body_i64(left, right, opcode), ValueType::I32);
            }
        }
        for value in [0i64, 1, -1, i64::MIN, i64::MAX] {
            let _ = assert_matches_micro(&unary_body_i64(value, 0x50), ValueType::I32);
        }
    }

    #[test]
    fn baseline_matches_micro_for_every_supported_i32_binary_op() {
        // (left, right, opcode) covers normal values, wrapping, masked shift
        // counts, rotation counts, and both division trap identities.
        let cases: &[(i32, i32, u8)] = &[
            (20, 22, 0x6a),
            (20, 22, 0x6b),
            (6, 7, 0x6c),
            (i32::MAX, 1, 0x6a),
            (i32::MIN, -1, 0x6b),
            (7, 2, 0x6d),
            (-7, 2, 0x6d),
            (-7, -2, 0x6d),
            (7, -2, 0x6d),
            (-1, 2, 0x6e),
            (-1, -2, 0x6e),
            (-7, 2, 0x6f),
            (-7, -2, 0x6f),
            (-1, 2, 0x70),
            (1, 33, 0x74),
            (1, 33, 0x75),
            (1, 33, 0x76),
            (i32::MIN, 1, 0x76),
            (0b1010, 0b0110, 0x71),
            (0b1010, 0b0110, 0x72),
            (0b1010, 0b0110, 0x73),
            (1, 1, 0x77),
            (1, 33, 0x77),
            (1, 1, 0x78),
            (1, 33, 0x78),
        ];
        for (left, right, opcode) in cases {
            let _ = assert_matches_micro(&binary_body(*left, *right, *opcode), ValueType::I32);
        }
    }

    #[test]
    fn baseline_reproduces_every_i32_division_trap() {
        // Division and remainder by zero trap identically in both backends.
        for opcode in [0x6d, 0x6e, 0x6f, 0x70] {
            assert_eq!(
                assert_matches_micro(&binary_body(1, 0, opcode), ValueType::I32),
                Err(Trap::IntegerDivisionByZero)
            );
        }
        // i32.div_s of INT_MIN by -1 overflows; i32.rem_s of the same pair is 0.
        assert_eq!(
            assert_matches_micro(&binary_body(i32::MIN, -1, 0x6d), ValueType::I32),
            Err(Trap::IntegerOverflow)
        );
        assert_eq!(
            assert_matches_micro(&binary_body(i32::MIN, -1, 0x6f), ValueType::I32),
            Ok(vec![Value::I32(0)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_eqz_and_every_i32_comparison() {
        // 0x45 is i32.eqz (unary); comparisons are 0x46..=0x4f.
        for opcode in 0x46..=0x4f {
            for (left, right) in [(0i32, 0i32), (-1, 1), (1, -1), (i32::MIN, -1)] {
                let _ = assert_matches_micro(&binary_body(left, right, opcode), ValueType::I32);
            }
        }
        for value in [0i32, 1, -1, i32::MIN, i32::MAX] {
            let _ = assert_matches_micro(&unary_body(value, 0x45), ValueType::I32);
        }
    }

    /// Build a two-operand `f32` body: `f32.const left; f32.const right; op; end`.
    ///
    /// `f32.const` immediates are raw little-endian bit patterns, so these
    /// fixtures can carry NaN payloads and signed zeros exactly.
    fn binary_body_f32(left: u32, right: u32, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x43];
        body.extend_from_slice(&left.to_le_bytes());
        body.push(0x43);
        body.extend_from_slice(&right.to_le_bytes());
        body.push(opcode);
        body.push(0x0b);
        body
    }

    /// Build a one-operand `f32` body: `f32.const value; op; end`.
    fn unary_body_f32(value: u32, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x43];
        body.extend_from_slice(&value.to_le_bytes());
        body.push(opcode);
        body.push(0x0b);
        body
    }

    /// Build a two-operand `f64` body: `f64.const left; f64.const right; op; end`.
    fn binary_body_f64(left: u64, right: u64, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x44];
        body.extend_from_slice(&left.to_le_bytes());
        body.push(0x44);
        body.extend_from_slice(&right.to_le_bytes());
        body.push(opcode);
        body.push(0x0b);
        body
    }

    /// Build a one-operand `f64` body: `f64.const value; op; end`.
    fn unary_body_f64(value: u64, opcode: u8) -> Vec<u8> {
        let mut body = vec![0x44];
        body.extend_from_slice(&value.to_le_bytes());
        body.push(opcode);
        body.push(0x0b);
        body
    }

    const F32_POS_ZERO: u32 = 0x0000_0000;
    const F32_NEG_ZERO: u32 = 0x8000_0000;
    const F32_NAN: u32 = 0x7fc0_1234;
    const F32_INFINITY: u32 = 0x7f80_0000;
    const F32_NEG_INFINITY: u32 = 0xff80_0000;
    const F64_POS_ZERO: u64 = 0x0000_0000_0000_0000;
    const F64_NEG_ZERO: u64 = 0x8000_0000_0000_0000;
    const F64_NAN: u64 = 0x7ff8_0000_0000_1234;
    const F64_INFINITY: u64 = 0x7ff0_0000_0000_0000;
    const F64_NEG_INFINITY: u64 = 0xfff0_0000_0000_0000;

    #[test]
    fn baseline_matches_micro_for_every_f32_binary_op() {
        // f32 binary opcodes are 0x92..=0x98: add, sub, mul, div, min, max,
        // copysign. The operand set covers ordinary values, signed zeros, a NaN
        // payload, and infinities so NaN and zero identity rules are exercised.
        let operands: &[(u32, u32)] = &[
            (1.5f32.to_bits(), 2.25f32.to_bits()),
            (F32_POS_ZERO, F32_NEG_ZERO),
            (F32_NEG_ZERO, F32_POS_ZERO),
            (F32_NAN, 1.0f32.to_bits()),
            (1.0f32.to_bits(), F32_NAN),
            (F32_INFINITY, F32_NEG_INFINITY),
            (F32_INFINITY, 0.0f32.to_bits()),
        ];
        for opcode in 0x92..=0x98 {
            for (left, right) in operands {
                let _ =
                    assert_matches_micro(&binary_body_f32(*left, *right, opcode), ValueType::F32);
            }
        }
    }

    #[test]
    fn baseline_matches_micro_for_every_f64_binary_op() {
        // f64 binary opcodes are 0xa0..=0xa6, mirroring the f32 set at 64 bits.
        let operands: &[(u64, u64)] = &[
            (1.5f64.to_bits(), 2.25f64.to_bits()),
            (F64_POS_ZERO, F64_NEG_ZERO),
            (F64_NEG_ZERO, F64_POS_ZERO),
            (F64_NAN, 1.0f64.to_bits()),
            (1.0f64.to_bits(), F64_NAN),
            (F64_INFINITY, F64_NEG_INFINITY),
            (F64_INFINITY, 0.0f64.to_bits()),
        ];
        for opcode in 0xa0..=0xa6 {
            for (left, right) in operands {
                let _ =
                    assert_matches_micro(&binary_body_f64(*left, *right, opcode), ValueType::F64);
            }
        }
    }

    #[test]
    fn baseline_min_and_max_follow_wasm_zero_and_nan_rules() {
        // 0x96 is f32.min, 0x97 is f32.max, 0xa4 is f64.min, 0xa5 is f64.max.
        assert_eq!(
            assert_matches_micro(
                &binary_body_f32(F32_NEG_ZERO, F32_POS_ZERO, 0x96),
                ValueType::F32,
            ),
            Ok(vec![Value::F32(F32_NEG_ZERO)])
        );
        assert_eq!(
            assert_matches_micro(
                &binary_body_f32(F32_NEG_ZERO, F32_POS_ZERO, 0x97),
                ValueType::F32,
            ),
            Ok(vec![Value::F32(F32_POS_ZERO)])
        );
        // NaN propagates through min and max as the canonical quiet NaN.
        assert_eq!(
            assert_matches_micro(
                &binary_body_f32(F32_NAN, 1.0f32.to_bits(), 0x96),
                ValueType::F32,
            ),
            Ok(vec![Value::F32(0x7fc0_0000)])
        );
        assert_eq!(
            assert_matches_micro(
                &binary_body_f64(F64_NAN, 1.0f64.to_bits(), 0xa5),
                ValueType::F64,
            ),
            Ok(vec![Value::F64(0x7ff8_0000_0000_0000)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_every_f32_unary_op() {
        // f32 unary opcodes are 0x8b..=0x91: abs, neg, ceil, floor, trunc,
        // nearest, sqrt. Halfway inputs are included so ties-to-even rounding
        // is exercised.
        let values = [
            1.5f32.to_bits(),
            (-1.5f32).to_bits(),
            0.5f32.to_bits(),
            2.5f32.to_bits(),
            F32_POS_ZERO,
            F32_NEG_ZERO,
            F32_NAN,
            F32_NEG_INFINITY,
        ];
        for opcode in 0x8b..=0x91 {
            for value in values {
                let _ = assert_matches_micro(&unary_body_f32(value, opcode), ValueType::F32);
            }
        }
        // f32.nearest (0x90) rounds halfway cases to the nearest even integer.
        assert_eq!(
            assert_matches_micro(&unary_body_f32(2.5f32.to_bits(), 0x90), ValueType::F32),
            Ok(vec![Value::F32(2.0f32.to_bits())])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_f32(3.5f32.to_bits(), 0x90), ValueType::F32),
            Ok(vec![Value::F32(4.0f32.to_bits())])
        );
    }

    #[test]
    fn baseline_matches_micro_for_every_f64_unary_op() {
        // f64 unary opcodes are 0x99..=0x9f, mirroring the f32 set.
        let values = [
            1.5f64.to_bits(),
            (-1.5f64).to_bits(),
            0.5f64.to_bits(),
            2.5f64.to_bits(),
            F64_POS_ZERO,
            F64_NEG_ZERO,
            F64_NAN,
            F64_NEG_INFINITY,
        ];
        for opcode in 0x99..=0x9f {
            for value in values {
                let _ = assert_matches_micro(&unary_body_f64(value, opcode), ValueType::F64);
            }
        }
        assert_eq!(
            assert_matches_micro(&unary_body_f64(2.5f64.to_bits(), 0x9e), ValueType::F64),
            Ok(vec![Value::F64(2.0f64.to_bits())])
        );
    }

    #[test]
    fn baseline_abs_neg_and_copysign_preserve_nan_payloads() {
        // These three operations are defined bitwise, so they must keep the NaN
        // payload rather than canonicalizing it the way arithmetic does.
        assert_eq!(
            assert_matches_micro(&unary_body_f32(F32_NAN, 0x8b), ValueType::F32),
            Ok(vec![Value::F32(0x7fc0_1234)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_f32(F32_NAN, 0x8c), ValueType::F32),
            Ok(vec![Value::F32(F32_NAN | 0x8000_0000)])
        );
        assert_eq!(
            assert_matches_micro(
                &binary_body_f32(1.0f32.to_bits(), F32_NEG_ZERO, 0x98),
                ValueType::F32,
            ),
            Ok(vec![Value::F32(1.0f32.to_bits() | 0x8000_0000)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_f64(F64_NAN, 0x99), ValueType::F64),
            Ok(vec![Value::F64(0x7ff8_0000_0000_1234)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_every_float_comparison() {
        // f32 comparisons are 0x5b..=0x60 and f64 comparisons are 0x61..=0x66.
        // Every comparison yields an i32, and NaN operands make all ordered
        // comparisons false while `ne` stays true.
        let f32_operands: &[(u32, u32)] = &[
            (1.0f32.to_bits(), 2.0f32.to_bits()),
            ((-1.0f32).to_bits(), 1.0f32.to_bits()),
            (F32_POS_ZERO, F32_NEG_ZERO),
            (F32_NAN, 1.0f32.to_bits()),
            (F32_NAN, F32_NAN),
        ];
        for opcode in 0x5b..=0x60 {
            for (left, right) in f32_operands {
                let _ =
                    assert_matches_micro(&binary_body_f32(*left, *right, opcode), ValueType::I32);
            }
        }
        let f64_operands: &[(u64, u64)] = &[
            (1.0f64.to_bits(), 2.0f64.to_bits()),
            ((-1.0f64).to_bits(), 1.0f64.to_bits()),
            (F64_POS_ZERO, F64_NEG_ZERO),
            (F64_NAN, 1.0f64.to_bits()),
            (F64_NAN, F64_NAN),
        ];
        for opcode in 0x61..=0x66 {
            for (left, right) in f64_operands {
                let _ =
                    assert_matches_micro(&binary_body_f64(*left, *right, opcode), ValueType::I32);
            }
        }
        // NaN is unordered: `eq` is false and `ne` is true.
        assert_eq!(
            assert_matches_micro(&binary_body_f32(F32_NAN, F32_NAN, 0x5b), ValueType::I32),
            Ok(vec![Value::I32(0)])
        );
        assert_eq!(
            assert_matches_micro(&binary_body_f32(F32_NAN, F32_NAN, 0x5c), ValueType::I32),
            Ok(vec![Value::I32(1)])
        );
    }

    #[test]
    fn baseline_preserves_f32_constants_bit_exactly() {
        // Floating-point constants are raw bit patterns and must round-trip
        // through lowering without being reinterpreted as Rust floats. `abs`
        // only clears the sign bit, so it exposes the stored payload directly.
        for bits in [
            F32_POS_ZERO,
            F32_NEG_ZERO,
            F32_NAN,
            F32_INFINITY,
            1.0f32.to_bits(),
        ] {
            assert_eq!(
                assert_matches_micro(&unary_body_f32(bits, 0x8b), ValueType::F32),
                Ok(vec![Value::F32(bits & 0x7fff_ffff)])
            );
        }
    }

    #[test]
    fn baseline_matches_micro_for_local_get_set_and_tee() {
        // Locals are zero-initialized. The body sets a local, reads it back, and
        // tees a second value, so all three access forms are exercised at once.
        let locals = [tpt_wasm_format::LocalDecl {
            count: 2,
            value_type: ValueType::I32,
        }];
        // i32.const 41; local.set 0; local.get 0; i32.const 1; i32.add;
        // local.tee 1; end  ->  42
        let body = [
            vec![0x41],
            encode_i32(41),
            vec![0x21, 0x00],
            vec![0x20, 0x00, 0x41],
            encode_i32(1),
            vec![0x6a, 0x22, 0x01],
            vec![0x0b],
        ]
        .concat();
        assert_eq!(
            assert_matches_micro_with_locals(&body, ValueType::I32, &locals),
            Ok(vec![Value::I32(42)])
        );
        // A declared-but-unset local reads as zero: local.get 0 directly.
        let read_unset = [vec![0x20, 0x00, 0x0b]].concat();
        assert_eq!(
            assert_matches_micro_with_locals(&read_unset, ValueType::I32, &locals),
            Ok(vec![Value::I32(0)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_select_and_drop() {
        // select is typed on both arms; drop simply discards a value.
        // i32.const 11; i32.const 22; i32.const 0; select -> 22
        let select_zero = [
            vec![0x41],
            encode_i32(11),
            vec![0x41],
            encode_i32(22),
            vec![0x41, 0x00, 0x1b, 0x0b],
        ]
        .concat();
        assert_eq!(
            assert_matches_micro(&select_zero, ValueType::I32),
            Ok(vec![Value::I32(22)])
        );
        // The same body with a non-zero condition selects the first arm.
        let select_nonzero = [
            vec![0x41],
            encode_i32(11),
            vec![0x41],
            encode_i32(22),
            vec![0x41, 0x01, 0x1b, 0x0b],
        ]
        .concat();
        assert_eq!(
            assert_matches_micro(&select_nonzero, ValueType::I32),
            Ok(vec![Value::I32(11)])
        );
        // i32.const 7; i32.const 8; i32.add; drop; i32.const 9 -> 9
        let dropped = [
            vec![0x41],
            encode_i32(7),
            vec![0x41],
            encode_i32(8),
            vec![0x6a, 0x1a],
            vec![0x41],
            encode_i32(9),
            vec![0x0b],
        ]
        .concat();
        assert_eq!(
            assert_matches_micro(&dropped, ValueType::I32),
            Ok(vec![Value::I32(9)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_every_integer_bit_count_op() {
        // i32 clz/ctz/popcnt are 0x67..=0x69 and i64 forms are 0x79..=0x7b.
        for opcode in 0x67..=0x69 {
            for value in [0i32, 1, -1, i32::MIN, i32::MAX, 0b1010_0101] {
                let _ = assert_matches_micro(&unary_body(value, opcode), ValueType::I32);
            }
        }
        for opcode in 0x79..=0x7b {
            for value in [0i64, 1, -1, i64::MIN, i64::MAX, 0b1010_0101] {
                let _ = assert_matches_micro(&unary_body_i64(value, opcode), ValueType::I64);
            }
        }
        // clz(0) is the full width and popcnt counts set bits.
        assert_eq!(
            assert_matches_micro(&unary_body(0, 0x67), ValueType::I32),
            Ok(vec![Value::I32(32)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_i64(0, 0x79), ValueType::I64),
            Ok(vec![Value::I64(64)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body(0b1010_0101, 0x69), ValueType::I32),
            Ok(vec![Value::I32(4)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_integer_width_conversions() {
        // 0xa7 is i32.wrap_i64, 0xac is i64.extend_i32_s, 0xad is i64.extend_i32_u.
        for value in [0i64, 1, -1, i64::MIN, i64::MAX, i64::from(i32::MAX) + 1] {
            let _ = assert_matches_micro(&unary_body_i64(value, 0xa7), ValueType::I32);
        }
        for value in [0i32, 1, -1, i32::MIN, i32::MAX] {
            let _ = assert_matches_micro(&unary_body(value, 0xac), ValueType::I64);
            let _ = assert_matches_micro(&unary_body(value, 0xad), ValueType::I64);
        }
        // Signed extension sign-extends; unsigned extension zero-extends.
        assert_eq!(
            assert_matches_micro(&unary_body(-1, 0xac), ValueType::I64),
            Ok(vec![Value::I64(-1)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body(-1, 0xad), ValueType::I64),
            Ok(vec![Value::I64(4294967295)])
        );
        // wrap keeps the low 32 bits.
        assert_eq!(
            assert_matches_micro(&unary_body_i64(0x1_0000_0001, 0xa7), ValueType::I32),
            Ok(vec![Value::I32(1)])
        );
    }

    #[test]
    fn baseline_reinterprets_bits_without_touching_them() {
        // 0xbc..=0xbf are the four reinterpret operations. Round-tripping a NaN
        // payload proves no canonicalization happens on the way through.
        assert_eq!(
            assert_matches_micro(&unary_body_f32(F32_NAN, 0xbc), ValueType::I32),
            Ok(vec![Value::I32(F32_NAN as i32)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body(F32_NAN as i32, 0xbe), ValueType::F32),
            Ok(vec![Value::F32(F32_NAN)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_f64(F64_NAN, 0xbd), ValueType::I64),
            Ok(vec![Value::I64(F64_NAN as i64)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_i64(F64_NAN as i64, 0xbf), ValueType::F64),
            Ok(vec![Value::F64(F64_NAN)])
        );
        // Reinterpret also preserves signed zero, which a numeric conversion
        // would lose.
        assert_eq!(
            assert_matches_micro(&unary_body_f32(F32_NEG_ZERO, 0xbc), ValueType::I32),
            Ok(vec![Value::I32(F32_NEG_ZERO as i32)])
        );
    }

    #[test]
    fn baseline_matches_micro_for_every_float_conversion() {
        // 0xb2..=0xbb are the ten non-trapping float conversions.
        for value in [0i32, 1, -1, i32::MIN, i32::MAX] {
            let _ = assert_matches_micro(&unary_body(value, 0xb2), ValueType::F32);
            let _ = assert_matches_micro(&unary_body(value, 0xb3), ValueType::F32);
            let _ = assert_matches_micro(&unary_body(value, 0xb7), ValueType::F64);
            let _ = assert_matches_micro(&unary_body(value, 0xb8), ValueType::F64);
        }
        for value in [0i64, 1, -1, i64::MIN, i64::MAX] {
            let _ = assert_matches_micro(&unary_body_i64(value, 0xb4), ValueType::F32);
            let _ = assert_matches_micro(&unary_body_i64(value, 0xb5), ValueType::F32);
            let _ = assert_matches_micro(&unary_body_i64(value, 0xb9), ValueType::F64);
            let _ = assert_matches_micro(&unary_body_i64(value, 0xba), ValueType::F64);
        }
        // 0xb6 narrows f64 to f32 and 0xbb widens f32 to f64, NaN included.
        for bits in [F32_NAN, F32_NEG_ZERO, 1.5f32.to_bits(), F32_INFINITY] {
            let _ = assert_matches_micro(&unary_body_f32(bits, 0xbb), ValueType::F64);
        }
        for bits in [F64_NAN, F64_NEG_ZERO, 1.5f64.to_bits(), F64_INFINITY] {
            let _ = assert_matches_micro(&unary_body_f64(bits, 0xb6), ValueType::F32);
        }
    }

    #[test]
    fn baseline_reproduces_every_float_truncation_trap() {
        // 0xa8..=0xab truncate to i32 and 0xae..=0xb1 truncate to i64.
        for opcode in [0xa8u8, 0xa9, 0xaa, 0xab, 0xae, 0xaf, 0xb0, 0xb1] {
            let result = if matches!(opcode, 0xa8..=0xab) {
                ValueType::I32
            } else {
                ValueType::I64
            };
            let from_f32 = matches!(opcode, 0xa8 | 0xa9 | 0xae | 0xaf);
            // NaN, both infinities, and out-of-range magnitudes all trap.
            let invalid: &[(u32, u64)] = &[
                (F32_NAN, F64_NAN),
                (F32_INFINITY, F64_INFINITY),
                (F32_NEG_INFINITY, F64_NEG_INFINITY),
                (1e30f32.to_bits(), 1e30f64.to_bits()),
                ((-1e30f32).to_bits(), (-1e30f64).to_bits()),
            ];
            for (f32_bits, f64_bits) in invalid {
                let body = if from_f32 {
                    unary_body_f32(*f32_bits, opcode)
                } else {
                    unary_body_f64(*f64_bits, opcode)
                };
                assert_eq!(
                    assert_matches_micro(&body, result),
                    Err(Trap::InvalidConversion),
                    "opcode {opcode:#04x} should trap"
                );
            }
        }
    }

    #[test]
    fn baseline_matches_micro_for_in_range_float_truncations() {
        // Truncation is toward zero, so fractional parts are discarded rather
        // than rounded, and the unsigned forms reject negatives.
        assert_eq!(
            assert_matches_micro(&unary_body_f32(3.9f32.to_bits(), 0xa8), ValueType::I32),
            Ok(vec![Value::I32(3)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_f32((-3.9f32).to_bits(), 0xa8), ValueType::I32),
            Ok(vec![Value::I32(-3)])
        );
        assert_eq!(
            assert_matches_micro(&unary_body_f64(3.9f64.to_bits(), 0xb0), ValueType::I64),
            Ok(vec![Value::I64(3)])
        );
        // A negative value has no unsigned representation and must trap.
        assert_eq!(
            assert_matches_micro(&unary_body_f32((-1.0f32).to_bits(), 0xa9), ValueType::I32),
            Err(Trap::InvalidConversion)
        );
        // -1.0 is exactly representable as i64 but still traps as unsigned.
        assert_eq!(
            assert_matches_micro(&unary_body_f64((-1.0f64).to_bits(), 0xb1), ValueType::I64),
            Err(Trap::InvalidConversion)
        );
    }

    /// Encode a single `i32.const` signed LEB128 immediate.
    fn encode_i32(value: i32) -> Vec<u8> {
        let mut remaining = value;
        let mut out = Vec::new();
        loop {
            let byte = (remaining as u8) & 0x7f;
            remaining >>= 7;
            let sign = byte & 0x40 != 0;
            if (remaining == 0 && !sign) || (remaining == -1 && sign) {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out
    }

    #[test]
    fn baseline_rejects_unresolvable_values_and_preserves_traps() {
        // An instruction reading a value that is not in the function's value
        // table must be rejected rather than read from a missing slot.
        let mut unknown = add_function();
        unknown.blocks[0].instrs[2] = IrInstr::I32Add {
            result: ValueId(2),
            left: ValueId(0),
            right: ValueId(9),
        };
        assert_eq!(
            lower_function(&unknown),
            Err(CodegenError::UnknownValue(ValueId(9)))
        );

        // Reading the same value twice is also rejected: the SSA value table
        // must not accept a duplicate definition.
        let mut duplicate = add_function();
        duplicate.blocks[0].instrs[1] = IrInstr::ConstI32 {
            result: ValueId(0),
            value: 7,
        };
        assert_eq!(
            lower_function(&duplicate),
            Err(CodegenError::DuplicateValue(ValueId(0)))
        );

        let trap = IrFunction {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(Vec::new()),
            },
            params: Vec::new(),
            locals: Vec::new(),
            values: Vec::new(),
            entry: BlockId(0),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instrs: Vec::new(),
                terminator: Terminator::Trap(Trap::IntegerDivisionByZero),
            }],
        };
        let baseline = lower_function(&trap).unwrap();
        assert_eq!(
            baseline.execute(Vec::new()),
            Err(BaselineError::Trap(Trap::IntegerDivisionByZero))
        );
        assert_eq!(
            baseline.blocks[0].ops,
            vec![BaselineOp::Trap(Trap::IntegerDivisionByZero)]
        );
    }

    /// `block (result i32) { i32.const 10; br 0 } end` — a `br` that carries a
    /// value into the enclosing block's result.
    const BR_OUT_OF_BLOCK: &[u8] = &[
        0x02, 0x7f, // block (result i32)
        0x41, 0x0a, // i32.const 10
        0x0c, 0x00, // br 0
        0x0b, // end (block)
        0x0b, // end (function)
    ];

    /// `i32.const c; if (result i32) { 20 } else { 30 } end`.
    fn if_else_body(condition: i32) -> Vec<u8> {
        let mut body = vec![0x41];
        body.extend(encode_i32(condition));
        body.extend_from_slice(&[
            0x04, 0x7f, // if (result i32)
            0x41, 0x14, // i32.const 20
            0x05, // else
            0x41, 0x1e, // i32.const 30
            0x0b, // end (if)
            0x0b, // end (function)
        ]);
        body
    }

    /// `block (result i32) { 7; c; br_if 0; 99 } end` — a conditional branch
    /// that either jumps out with 7 or falls through to 99.
    ///
    /// `br_if` pops the label's value as well as the condition, so the
    /// fall-through path starts from an empty stack.
    fn br_if_body(condition: i32) -> Vec<u8> {
        let mut body = vec![0x02, 0x7f]; // block (result i32)
        body.push(0x41);
        body.extend(encode_i32(7));
        body.push(0x41);
        body.extend(encode_i32(condition));
        body.extend_from_slice(&[0x0d, 0x00]); // br_if 0
                                               // `br_if` is `[t* i32] -> [t*]`, so the carried `7` stays on the stack
                                               // whether or not the branch is taken. The fall-through therefore has to
                                               // drop it before producing the other value, or the block would end with
                                               // two operands.
        body.push(0x1a); // drop
        body.push(0x41);
        body.extend(encode_i32(99));
        body.extend_from_slice(&[0x0b, 0x0b]); // end (block), end (function)
        body
    }

    /// Count up to 5 in a loop through a local, so the baseline must follow a
    /// genuine back edge: `loop { n = n + 1; br_if 0 while n < 5 }`.
    const LOOP_TO_FIVE: &[u8] = &[
        0x41, 0x00, // i32.const 0
        0x21, 0x00, // local.set 0
        0x02, 0x40, // block
        0x03, 0x40, // loop
        0x20, 0x00, // local.get 0
        0x41, 0x01, // i32.const 1
        0x6a, // i32.add
        0x22, 0x00, // local.tee 0
        0x41, 0x05, // i32.const 5
        0x48, // i32.lt_s
        0x0d, 0x00, // br_if 0
        0x0b, // end (loop)
        0x0b, // end (block)
        0x20, 0x00, // local.get 0
        0x0b, // end (function)
    ];

    #[test]
    fn baseline_matches_micro_for_br_carrying_a_value() {
        assert_eq!(
            assert_matches_micro(BR_OUT_OF_BLOCK, ValueType::I32).unwrap(),
            vec![Value::I32(10)]
        );
    }

    #[test]
    fn baseline_matches_micro_for_if_else_on_both_edges() {
        for condition in [0, 1, -1] {
            let body = if_else_body(condition);
            assert_eq!(
                assert_matches_micro(&body, ValueType::I32).unwrap(),
                vec![Value::I32(if condition == 0 { 30 } else { 20 })],
                "condition {condition}"
            );
        }
    }

    #[test]
    fn baseline_matches_micro_for_br_if_taken_and_falling_through() {
        for condition in [0, 1] {
            let body = br_if_body(condition);
            assert_eq!(
                assert_matches_micro(&body, ValueType::I32).unwrap(),
                vec![Value::I32(if condition == 0 { 99 } else { 7 })],
                "condition {condition}"
            );
        }
    }

    #[test]
    fn baseline_follows_a_loop_back_edge() {
        // A loop is only correct if the block walk can revisit a block, so this
        // is the test that would hang or exit early on a missing back edge.
        let locals = [tpt_wasm_format::LocalDecl {
            count: 1,
            value_type: ValueType::I32,
        }];
        assert_eq!(
            assert_matches_micro_with_locals(LOOP_TO_FIVE, ValueType::I32, &locals).unwrap(),
            vec![Value::I32(5)]
        );
    }

    /// A minimal single-block function, used as a starting point for the
    /// malformed-CFG cases below.
    fn one_block_function(terminator: Terminator) -> IrFunction {
        IrFunction {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            params: Vec::new(),
            locals: Vec::new(),
            values: vec![IrValue {
                id: ValueId(0),
                value_type: ValueType::I32,
            }],
            entry: BlockId(0),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instrs: vec![IrInstr::ConstI32 {
                    result: ValueId(0),
                    value: 1,
                }],
                terminator,
            }],
        }
    }

    #[test]
    fn baseline_rejects_malformed_control_flow() {
        // A branch to a block that does not exist.
        let dangling = one_block_function(Terminator::Branch {
            target: BlockId(9),
            values: Vec::new(),
        });
        assert_eq!(
            lower_function(&dangling),
            Err(CodegenError::UnknownBlock(BlockId(9)))
        );

        // An edge supplying the wrong number of values for its target.
        let mut arity = one_block_function(Terminator::Branch {
            target: BlockId(0),
            values: Vec::new(),
        });
        arity.blocks[0].params = vec![ValueId(1)];
        arity.values.push(IrValue {
            id: ValueId(1),
            value_type: ValueType::I32,
        });
        assert_eq!(
            lower_function(&arity),
            Err(CodegenError::BranchArity {
                target: BlockId(0),
                expected: 1,
                actual: 0,
            })
        );

        // Two blocks sharing one id, which would make a target ambiguous.
        let mut duplicate = one_block_function(Terminator::Return(vec![ValueId(0)]));
        duplicate.blocks.push(BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instrs: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        assert_eq!(
            lower_function(&duplicate),
            Err(CodegenError::DuplicateBlock(BlockId(0)))
        );

        // An entry block id that is not in the block list.
        let mut missing = one_block_function(Terminator::Return(vec![ValueId(0)]));
        missing.entry = BlockId(7);
        assert_eq!(
            lower_function(&missing),
            Err(CodegenError::MissingEntryBlock)
        );
    }

    /// The zero value a Wasm local starts with.
    fn zero_of(value_type: ValueType) -> Value {
        match value_type {
            ValueType::I32 => Value::I32(0),
            ValueType::I64 => Value::I64(0),
            ValueType::F32 => Value::F32(0),
            ValueType::F64 => Value::F64(0),
            // A reference local starts null, the only zero a reference has.
            ValueType::Ref(kind) => Value::Ref(RefValue::Null(kind)),
            other => panic!("unsupported local type in fixture: {other:?}"),
        }
    }

    /// Assert the baseline backend and Micro agree on a whole module, so that
    /// `call` is exercised across function boundaries.
    ///
    /// Micro resolves a call through the store's instance, so every function of
    /// the module is installed first and the entry function is pushed as a frame.
    fn assert_module_matches_micro(
        module: &Module,
        entry: usize,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, Trap> {
        let validated = tpt_wasm_validate::validate(module.clone()).expect("fixture must validate");
        let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
        let mut baseline =
            BaselineModule::lower(verified.module()).expect("codegen must accept the module");
        let baseline_result = baseline.call(entry, args.clone());
        let micro_result = run_in_micro(module, entry, args);
        let from_baseline: Result<&[Value], Trap> = match &baseline_result {
            Ok(values) => Ok(values),
            Err(error) => Err(trap_of(error)),
        };
        let from_micro: Result<&[Value], Trap> = match &micro_result {
            Ok(values) => Ok(values),
            Err(trap) => Err(trap.clone()),
        };
        assert_eq!(
            from_baseline, from_micro,
            "baseline and Micro diverged on the module"
        );
        baseline_result.map_err(|error| trap_of(&error))
    }

    /// `$double` doubles its argument; the entry function calls it.
    fn double_module() -> Module {
        Module {
            types: vec![
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(vec![ValueType::I32]),
                },
                FunctionType {
                    params: ResultType(vec![ValueType::I32]),
                    results: ResultType(vec![ValueType::I32]),
                },
            ],
            functions: vec![
                // 0: (result i32) i32.const 5; call $double
                Function {
                    type_index: 0,
                    locals: vec![],
                    body: vec![0x41, 0x05, 0x10, 0x01, 0x0b],
                },
                // 1: (param i32) (result i32) local.get 0; i32.const 2; i32.mul
                Function {
                    type_index: 1,
                    locals: vec![],
                    body: vec![0x20, 0x00, 0x41, 0x02, 0x6c, 0x0b],
                },
            ],
            ..Module::default()
        }
    }

    #[test]
    fn baseline_matches_micro_for_a_direct_call() {
        assert_eq!(
            assert_module_matches_micro(&double_module(), 0, Vec::new()).unwrap(),
            vec![Value::I32(10)]
        );
    }

    #[test]
    fn baseline_forwards_call_arguments_and_results() {
        // The entry takes the argument, so a wrong slot mapping shows up as a
        // wrong result rather than a fixed constant.
        let mut module = double_module();
        module.functions[0].type_index = 1;
        module.functions[0].body = vec![0x20, 0x00, 0x10, 0x01, 0x0b];
        for argument in [0, 1, 7, -3] {
            assert_eq!(
                assert_module_matches_micro(&module, 0, vec![Value::I32(argument)]).unwrap(),
                vec![Value::I32(argument.wrapping_mul(2))],
                "argument {argument}"
            );
        }
    }

    #[test]
    fn baseline_propagates_a_trap_out_of_a_callee() {
        // The callee divides by zero, so the trap must surface from the caller.
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            }],
            functions: vec![
                // 0: call $boom
                Function {
                    type_index: 0,
                    locals: vec![],
                    body: vec![0x10, 0x01, 0x0b],
                },
                // 1: (result i32) i32.const 1; i32.const 0; i32.div_s
                Function {
                    type_index: 0,
                    locals: vec![],
                    body: vec![0x41, 0x01, 0x41, 0x00, 0x6d, 0x0b],
                },
            ],
            ..Module::default()
        };
        assert_eq!(
            assert_module_matches_micro(&module, 0, Vec::new()),
            Err(Trap::IntegerDivisionByZero)
        );
    }

    #[test]
    fn baseline_supports_recursion() {
        // A recursive sum-to-n, so each callee needs its own slot frame.
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(vec![ValueType::I32]),
                results: ResultType(vec![ValueType::I32]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: vec![],
                body: vec![
                    0x20, 0x00, // local.get 0
                    0x45, // i32.eqz
                    0x04, 0x7f, // if (result i32)
                    0x41, 0x00, // i32.const 0
                    0x05, // else
                    // The condition above consumed the only `local.get 0`, so
                    // `n` is read again here before recursing.
                    0x20, 0x00, // local.get 0
                    0x20, 0x00, // local.get 0
                    0x41, 0x01, // i32.const 1
                    0x6b, // i32.sub
                    0x10, 0x00, // call 0
                    0x6a, // i32.add
                    0x0b, // end (if)
                    0x0b, // end (function)
                ],
            }],
            ..Module::default()
        };
        // sum(n) = sum(n - 1) + n, so the answer grows with the recursion depth.
        for argument in [0, 1, 2, 5, 20] {
            let expected = argument * (argument + 1) / 2;
            assert_eq!(
                assert_module_matches_micro(&module, 0, vec![Value::I32(argument)]).unwrap(),
                vec![Value::I32(expected)],
                "argument {argument}"
            );
        }
    }

    #[test]
    fn a_call_cannot_resolve_without_a_function_table() {
        // `execute` has no module, so a call cannot be resolved through it.
        let validated =
            tpt_wasm_validate::validate(double_module()).expect("fixture must validate");
        let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
        let baseline = lower_module(verified.module()).unwrap();
        let error = baseline[0].execute(Vec::new()).unwrap_err();
        assert!(
            matches!(&error, BaselineError::Trap(Trap::HostFailure(message)) if message.contains("call target")),
            "unexpected error: {error:?}"
        );
    }

    /// Run a module's `entry` function in both the baseline module and Micro.
    ///
    /// Unlike [`assert_module_matches_micro`], this builds a runnable
    /// [`super::BaselineModule`] so memory and globals are present on both sides.
    fn assert_runnable_matches_micro(
        module: &Module,
        entry: usize,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, Trap> {
        let validated = tpt_wasm_validate::validate(module.clone()).expect("fixture must validate");
        let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
        let mut baseline =
            super::BaselineModule::lower(verified.module()).expect("codegen must accept module");
        let baseline_result = baseline.call(entry, args.clone());
        let micro_result = run_in_micro(module, entry, args);
        let from_baseline: Result<&[Value], Trap> = match &baseline_result {
            Ok(values) => Ok(values),
            Err(error) => Err(trap_of(error)),
        };
        let from_micro: Result<&[Value], Trap> = match &micro_result {
            Ok(values) => Ok(values),
            Err(trap) => Err(trap.clone()),
        };
        assert_eq!(
            from_baseline, from_micro,
            "baseline and Micro diverged on the runnable module"
        );
        baseline_result.map_err(|error| trap_of(&error))
    }

    /// Run one function of a module in Micro with its memory and globals set up.
    ///
    /// `host` supplies any imported function, so a module that calls out reaches
    /// the same boundary the baseline is given.
    ///
    /// A refused capability is reported as [`Trap::HostFailure`] carrying the
    /// host's own message, which is the text the baseline's `Host` failure holds
    /// too, so the two can be compared on what the host actually said.
    fn run_in_micro_with_host(
        module: &Module,
        entry: usize,
        args: Vec<Value>,
        host: Option<Box<dyn super::HostBoundary>>,
    ) -> Result<Vec<Value>, Trap> {
        let mut store = Store::default();
        // Imported functions come first in the Wasm index space and are resolved
        // to host callbacks, exactly as the runtime resolves them.
        let mut func_addrs = Vec::new();
        for import in &module.imports {
            let tpt_wasm_format::ImportDesc::Function(type_index) = import.desc else {
                panic!("the Micro harness only resolves function imports");
            };
            let function_type = module.types[type_index as usize].clone();
            let name = format!("{}::{}", import.module, import.name);
            func_addrs.push(
                store
                    .add_host_function(function_type, name)
                    .expect("fixture host function must install"),
            );
        }
        // Defined functions start here in the Wasm index space.
        let imported = func_addrs.len();
        for (index, function) in module.functions.iter().enumerate() {
            let function_type = module.types[function.type_index as usize].clone();
            let mut local_types = function_type.params.0.clone();
            local_types.extend(function.locals.iter().map(|d| d.value_type));
            let address = store
                .add_wasm_function(
                    0,
                    (func_addrs.len() + index) as u32,
                    function_type,
                    decode_body(&function.body).expect("fixture body must decode"),
                    local_types,
                )
                .expect("fixture function must install");
            func_addrs.push(address);
        }
        let mut memory_addrs = Vec::new();
        for memory in &module.memories {
            let address = store
                .allocate_memory(memory.memory_type.limits.min, memory.memory_type.limits.max)
                .expect("test memory should fit in store");
            // Write the active data segments in module order, so a later segment
            // overwrites an earlier one at the same address.
            for segment in &module.data {
                let tpt_wasm_format::DataMode::Active {
                    memory_index,
                    offset,
                } = &segment.mode
                else {
                    panic!("test data segment must be active");
                };
                if *memory_index as usize != memory_addrs.len() {
                    continue;
                }
                let base = tpt_wasm_ir::const_expr_i32(offset)
                    .expect("test data offset must be a constant i32")
                    as usize;
                let end = base + segment.data.len();
                let instance = store
                    .memory_mut(address)
                    .expect("test memory should resolve");
                assert!(
                    end <= instance.data.len(),
                    "segment does not fit the memory"
                );
                instance.data[base..end].copy_from_slice(&segment.data);
            }
            memory_addrs.push(address);
        }
        let mut global_addrs = Vec::new();
        for global in &module.globals {
            let value = decode_global(&global.init);
            global_addrs.push(
                store
                    .add_global(global.global_type, value)
                    .expect("test global should fit in store"),
            );
        }
        // Tables, with their active element segments written in. Micro resolves
        // an indirect call through this, so a table left empty would turn every
        // index into an out-of-bounds trap.
        let mut table_addrs = Vec::new();
        for table in &module.tables {
            let min =
                u32::try_from(table.table_type.limits.min).expect("test table should fit a u32");
            let max = table
                .table_type
                .limits
                .max
                .map(|max| u32::try_from(max).expect("test table max should fit a u32"));
            table_addrs.push(
                store
                    .allocate_table(table.table_type.element_type, min, max)
                    .expect("test table should fit in store"),
            );
        }
        for element in &module.elements {
            let tpt_wasm_format::ElementMode::Active {
                table_index,
                offset,
            } = &element.mode
            else {
                panic!("test element segment must be active");
            };
            let base = tpt_wasm_ir::const_expr_i32(offset)
                .expect("test element offset must be a constant i32");
            let address = table_addrs[*table_index as usize];
            for (position, entry) in element_init_indices(element) {
                // A `ref.null` entry leaves the slot empty rather than naming a
                // function, which is what a `call_indirect` through it must see.
                let cell = entry.map(|function| RefValue::FuncRef(func_addrs[function as usize]));
                store
                    .table_mut(address)
                    .expect("test table should resolve")
                    .elements[base as usize + position] = cell;
            }
        }
        let instance = store
            .add_instance(Instance {
                module_types: module.types.clone(),
                func_addrs,
                table_addrs,
                memory_addrs,
                global_addrs,
            })
            .expect("test instance must install");
        let entry_function = &module.functions[entry];
        let mut locals = args;
        for declaration in &entry_function.locals {
            for _ in 0..declaration.count {
                locals.push(zero_of(declaration.value_type));
            }
        }
        let result_arity = module.types[entry_function.type_index as usize]
            .results
            .0
            .len();
        let mut machine = Machine::with_store(store);
        machine
            .push_frame(Frame::new(
                instance,
                // The frame's function index is the Wasm index, so an importing
                // module's first defined function is not function 0.
                (imported + entry) as u32,
                locals,
                decode_body(&entry_function.body).expect("fixture body must decode"),
                result_arity,
            ))
            .expect("entry frame must fit");
        // A host call is resumed here exactly as the runtime resumes it, so both
        // backends reach the same boundary with the same arguments.
        let mut host = host;
        loop {
            match machine.run() {
                Step::Return(values) => return Ok(values),
                Step::Trap(trap) => return Err(trap),
                Step::HostCall(call) => {
                    let boundary = host.as_deref_mut().ok_or_else(|| {
                        Trap::HostFailure("the fixture declared no host boundary".into())
                    })?;
                    let mut parts = call.func_name.splitn(2, "::");
                    let module_name = parts.next().unwrap_or_default();
                    let field_name = parts.next().unwrap_or_default();
                    // A refused capability is reported as a trap in this
                    // harness, matching what the runtime surfaces to a caller.
                    let results = boundary
                        .call(module_name, field_name, call.args)
                        .map_err(|failure| Trap::HostFailure(failure.message))?;
                    machine.resume_host_call(results)?;
                }
                other => {
                    return Err(Trap::HostFailure(format!(
                        "Micro did not finish: {other:?}"
                    )))
                }
            }
        }
    }

    /// Run a module in Micro with no host boundary, for fixtures that import
    /// nothing.
    fn run_in_micro(module: &Module, entry: usize, args: Vec<Value>) -> Result<Vec<Value>, Trap> {
        run_in_micro_with_host(module, entry, args, None)
    }

    /// Decode a validated global initializer for the Micro driver. The trailing
    /// `end` byte is not part of the immediate.
    fn decode_global(expr: &tpt_wasm_format::ConstExpr) -> Value {
        let bytes = &expr.0;
        match bytes.first() {
            Some(0x41) => Value::I32(i64_from_leb(&bytes[1..]) as i32),
            Some(0x42) => Value::I64(i64_from_leb(&bytes[1..])),
            Some(0x43) => Value::F32(u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]])),
            Some(0x44) => {
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&bytes[1..9]);
                Value::F64(u64::from_le_bytes(raw))
            }
            other => panic!("unexpected global initializer opcode: {other:?}"),
        }
    }

    fn i64_from_leb(bytes: &[u8]) -> i64 {
        let mut result: i64 = 0;
        let mut shift = 0;
        for byte in bytes {
            if shift < 64 {
                result |= i64::from(*byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if *byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                break;
            }
        }
        result
    }

    /// Attach a one-page memory to a module.
    fn with_memory(inner: Module) -> Module {
        let mut module = inner;
        module.memories.push(tpt_wasm_format::Memory {
            memory_type: tpt_wasm_types::MemoryType {
                limits: tpt_wasm_types::Limits { min: 1, max: None },
                memory64: false,
            },
        });
        module
    }

    /// Attach a global to a module.
    fn with_global(inner: Module, value_type: ValueType, mutable: bool, init: Vec<u8>) -> Module {
        let mut module = inner;
        let mut init = init;
        init.push(0x0b);
        module.globals.push(tpt_wasm_format::Global {
            global_type: tpt_wasm_types::GlobalType {
                value_type,
                mutable,
            },
            init: tpt_wasm_format::ConstExpr(init),
        });
        module
    }

    /// A one-function module taking no parameters and returning `results`.
    fn module(_params: Vec<ValueType>, results: Vec<ValueType>, body: Vec<u8>) -> Module {
        Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(results),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: vec![],
                body,
            }],
            ..Module::default()
        }
    }

    /// An `i32.const` with a canonical immediate.
    fn const_i32(value: i32) -> Vec<u8> {
        let mut bytes = vec![0x41];
        bytes.extend(encode_i32(value));
        bytes
    }

    #[test]
    fn baseline_memory_matches_micro() {
        // Store then load a full word.
        let mut body = const_i32(4);
        body.extend(const_i32(0x1234_5678));
        body.extend_from_slice(&[0x36, 0x02, 0x00]);
        body.extend(const_i32(4));
        body.extend_from_slice(&[0x28, 0x02, 0x00]);
        body.push(0x0b);
        let module = with_memory(module(Vec::new(), vec![ValueType::I32], body));
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(0x1234_5678u32 as i32)]
        );
    }

    #[test]
    fn baseline_narrow_loads_match_micro() {
        for (opcode, name, result_type, expected) in [
            (0x2c, "i32.load8_s", ValueType::I32, Value::I32(-1)),
            (0x2d, "i32.load8_u", ValueType::I32, Value::I32(255)),
            (0x30, "i64.load8_s", ValueType::I64, Value::I64(-1)),
            (0x31, "i64.load8_u", ValueType::I64, Value::I64(255)),
        ] {
            let mut body = const_i32(0);
            body.extend(const_i32(255));
            body.extend_from_slice(&[0x3a, 0x00, 0x00]);
            body.extend(const_i32(0));
            body.extend_from_slice(&[opcode, 0x00, 0x00]);
            body.push(0x0b);
            let module = with_memory(module(Vec::new(), vec![result_type], body));
            assert_eq!(
                assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
                vec![expected],
                "{name}"
            );
        }
    }

    #[test]
    fn baseline_memory_traps_out_of_bounds_like_micro() {
        let mut body = const_i32(65_535);
        body.extend_from_slice(&[0x28, 0x02, 0x00]);
        body.push(0x0b);
        let module = with_memory(module(Vec::new(), vec![ValueType::I32], body));
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()),
            Err(Trap::MemoryOutOfBounds)
        );
    }

    #[test]
    fn baseline_memory_size_and_grow_match_micro() {
        let mut body = vec![0x3f, 0x00];
        body.extend(const_i32(2));
        body.extend_from_slice(&[0x40, 0x00, 0x6a, 0x0b]);
        let module = with_memory(module(Vec::new(), vec![ValueType::I32], body));
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(2)]
        );
    }

    /// The entries of an element segment as function indices, paired with their
    /// position, where `None` is a `ref.null` that leaves the slot empty.
    ///
    /// This is the Micro-side fixture's own reading of the segment. It is
    /// deliberately independent of the runtime's: if both are right, a null
    /// entry is observed to be null, and if only the runtime is right, a
    /// `call_indirect` through that slot would trap in one backend and not the
    /// other.
    fn element_init_indices(element: &tpt_wasm_format::Element) -> Vec<(usize, Option<u32>)> {
        let entries: Vec<Option<u32>> = match &element.init {
            tpt_wasm_format::ElementInit::FuncIndices(indices) => {
                indices.iter().map(|index| Some(*index)).collect()
            }
            tpt_wasm_format::ElementInit::Expressions(expressions) => expressions
                .iter()
                .map(|expression| {
                    // `ref.null t end` or `ref.func <leb> end`.
                    assert_eq!(expression.0.last(), Some(&0x0b), "entry must end");
                    match expression.0[0] {
                        0xd0 => None,
                        0xd2 => {
                            let mut value = 0u32;
                            let mut shift = 0;
                            for byte in &expression.0[1..expression.0.len() - 1] {
                                value |= u32::from(byte & 0x7f) << shift;
                                if byte & 0x80 == 0 {
                                    break;
                                }
                                shift += 7;
                            }
                            Some(value)
                        }
                        other => panic!("element entry is neither reference form: {other:#x}"),
                    }
                })
                .collect(),
        };
        entries.into_iter().enumerate().collect()
    }

    #[test]
    fn baseline_global_read_write_matches_micro() {
        let mut body = vec![0x23, 0x00];
        body.extend(const_i32(5));
        body.extend_from_slice(&[0x6a, 0x24, 0x00, 0x23, 0x00, 0x0b]);
        let module = with_global(
            module(Vec::new(), vec![ValueType::I32], body),
            ValueType::I32,
            true,
            const_i32(55),
        );
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(60)]
        );
    }

    #[test]
    fn baseline_shares_global_state_across_a_call() {
        // Function 1 bumps the global; the entry calls it and reads the result,
        // which only works if both frames see the same global slots.
        let module = with_global(
            Module {
                types: vec![
                    FunctionType {
                        params: ResultType(Vec::new()),
                        results: ResultType(vec![ValueType::I32]),
                    },
                    FunctionType {
                        params: ResultType(Vec::new()),
                        results: ResultType(vec![ValueType::I32]),
                    },
                ],
                functions: vec![
                    // 0: call 1; drop; global.get 0
                    Function {
                        type_index: 0,
                        locals: vec![],
                        body: vec![0x10, 0x01, 0x1a, 0x23, 0x00, 0x0b],
                    },
                    // 1: global.get 0; +1; global.set 0; global.get 0
                    Function {
                        type_index: 1,
                        locals: vec![],
                        body: vec![0x23, 0x00, 0x41, 0x01, 0x6a, 0x24, 0x00, 0x23, 0x00, 0x0b],
                    },
                ],
                ..Module::default()
            },
            ValueType::I32,
            true,
            const_i32(10),
        );
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(11)]
        );
    }

    #[test]
    fn a_memory_instruction_without_a_memory_traps() {
        // `execute` has no module memory, so the access is refused rather than
        // reading uninitialized data.
        let mut body = const_i32(0);
        body.extend_from_slice(&[0x28, 0x02, 0x00, 0x0b]);
        let validated = tpt_wasm_validate::validate(with_memory(module(
            Vec::new(),
            vec![ValueType::I32],
            body,
        )))
        .expect("fixture must validate");
        let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
        let function = lower_function(&verified.module().functions[0]).unwrap();
        let error = function.execute(Vec::new()).unwrap_err();
        assert!(
            matches!(&error, BaselineError::Trap(Trap::HostFailure(message)) if message.contains("module memory")),
            "unexpected error: {error:?}"
        );
    }
    /// A table of `$double`, with the entry function dispatching through it.
    ///
    /// The element segment places `$double` at index 1 of a two-entry table, so
    /// index 0 is deliberately null and index 1 dispatches.
    fn dispatch_module() -> Module {
        Module {
            types: vec![
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(vec![ValueType::I32]),
                },
                FunctionType {
                    params: ResultType(vec![ValueType::I32]),
                    results: ResultType(vec![ValueType::I32]),
                },
            ],
            // 0: (result i32) i32.const 1; call_indirect type 1, table 0
            // 1: (param i32) (result i32) local.get 0; i32.const 2; i32.mul
            functions: vec![
                Function {
                    type_index: 0,
                    locals: vec![],
                    body: vec![0x41, 0x05, 0x41, 0x01, 0x11, 0x01, 0x00, 0x0b],
                },
                Function {
                    type_index: 1,
                    locals: vec![],
                    body: vec![0x20, 0x00, 0x41, 0x02, 0x6c, 0x0b],
                },
            ],
            tables: vec![Table {
                table_type: tpt_wasm_types::TableType {
                    element_type: tpt_wasm_types::ReferenceType::FuncRef,
                    limits: tpt_wasm_types::Limits { min: 2, max: None },
                },
                init: None,
            }],
            elements: vec![Element {
                element_type: tpt_wasm_types::RefType::FuncRef,
                mode: ElementMode::Active {
                    table_index: 0,
                    // A constant `i32` offset of 1, as `i32.const 1; end`.
                    offset: tpt_wasm_format::ConstExpr(vec![0x41, 0x01, 0x0b]),
                },
                init: tpt_wasm_format::ElementInit::FuncIndices(vec![1]),
            }],
            ..Module::default()
        }
    }

    #[test]
    fn baseline_matches_micro_for_an_indirect_call() {
        assert_eq!(
            assert_module_matches_micro(&dispatch_module(), 0, Vec::new()).unwrap(),
            vec![Value::I32(10)]
        );
    }

    #[test]
    fn an_indirect_call_out_of_range_traps_like_micro() {
        // Index 7 is past the end of the two-entry table.
        let mut module = dispatch_module();
        module.functions[0].body = vec![0x41, 0x05, 0x41, 0x07, 0x11, 0x01, 0x00, 0x0b];
        assert_eq!(
            assert_module_matches_micro(&module, 0, Vec::new()),
            Err(Trap::TableOutOfBounds)
        );
    }

    #[test]
    fn an_indirect_call_through_a_null_entry_traps_like_micro() {
        // Index 0 is inside the table but was never written by the segment.
        let mut module = dispatch_module();
        module.functions[0].body = vec![0x41, 0x05, 0x41, 0x00, 0x11, 0x01, 0x00, 0x0b];
        assert_eq!(
            assert_module_matches_micro(&module, 0, Vec::new()),
            Err(Trap::NullReference)
        );
    }

    #[test]
    fn an_element_written_as_a_null_expression_traps_like_micro() {
        // The same dispatch, but the segment now spells its single entry as
        // `ref.null funcref end` instead of a function index. Table index 1 is
        // therefore written and null, where the index form left it holding
        // `$double`. Both backends read the segment independently, so this only
        // passes if each one really saw a null rather than a function index: an
        // implementation that narrowed the expression to an index would call
        // `$double` here and return 10.
        let mut module = dispatch_module();
        module.elements[0].init = tpt_wasm_format::ElementInit::Expressions(vec![
            tpt_wasm_format::ConstExpr(vec![0xd0, 0x70, 0x0b]),
        ]);
        assert_eq!(
            assert_module_matches_micro(&module, 0, Vec::new()),
            Err(Trap::NullReference)
        );
    }

    #[test]
    fn an_element_written_as_a_ref_func_expression_dispatches_like_micro() {
        // The mirror of the case above: the same segment written as
        // `ref.func 1 end` names `$double` and must still dispatch, so the
        // expression family is not simply rejected or read as null.
        let mut module = dispatch_module();
        module.elements[0].init = tpt_wasm_format::ElementInit::Expressions(vec![
            tpt_wasm_format::ConstExpr(vec![0xd2, 0x01, 0x0b]),
        ]);
        assert_eq!(
            assert_module_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(10)]
        );
    }

    #[test]
    fn an_indirect_call_with_the_wrong_type_traps_like_micro() {
        // Function 0 has type 0, but the call names type 1, so the entry's
        // signature does not match what the call requires. The segment is moved
        // to offset 0 so that the call actually reaches function 0.
        let mut module = dispatch_module();
        module.elements[0].mode = ElementMode::Active {
            table_index: 0,
            offset: tpt_wasm_format::ConstExpr(vec![0x41, 0x00, 0x0b]),
        };
        module.elements[0].init = tpt_wasm_format::ElementInit::FuncIndices(vec![0]);
        module.functions[0].body = vec![0x41, 0x05, 0x41, 0x00, 0x11, 0x01, 0x00, 0x0b];
        assert_eq!(
            assert_module_matches_micro(&module, 0, Vec::new()),
            Err(Trap::IndirectCallTypeMismatch)
        );
    }

    #[test]
    fn an_indirect_call_forwards_its_arguments() {
        // The table index is the topmost operand, above the arguments, so a
        // mis-ordered pop shows up as the wrong argument reaching the callee.
        let mut module = dispatch_module();
        // (param i32) (result i32) local.get 0; i32.const 0; call_indirect
        module.types[0] = FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        };
        module.functions[0].type_index = 0;
        module.functions[0].body = vec![0x20, 0x00, 0x41, 0x01, 0x11, 0x01, 0x00, 0x0b];
        for argument in [0, 1, 7, -3] {
            assert_eq!(
                assert_module_matches_micro(&module, 0, vec![Value::I32(argument)]).unwrap(),
                vec![Value::I32(argument.wrapping_mul(2))],
                "argument {argument}"
            );
        }
    }

    /// Attach an active data segment to a module that already has a memory.
    ///
    /// The offset is a constant `i32`, encoded the same way the decoder reads it.
    fn with_data(mut inner: Module, offset: i32, bytes: &[u8]) -> Module {
        // `encode_i32` emits only the LEB128 immediate, so the opcode and the
        // terminating `end` byte are added here.
        let mut encoded = vec![0x41];
        encoded.extend(encode_i32(offset));
        encoded.push(0x0b);
        inner.data.push(tpt_wasm_format::DataSegment {
            mode: tpt_wasm_format::DataMode::Active {
                memory_index: 0,
                offset: tpt_wasm_format::ConstExpr(encoded),
            },
            data: bytes.to_vec(),
        });
        inner
    }

    #[test]
    fn an_active_data_segment_initializes_memory() {
        // Load the four bytes the segment wrote, little-endian.
        let mut body = const_i32(0);
        body.extend_from_slice(&[0x28, 0x02, 0x00]);
        body.push(0x0b);
        let module = with_data(
            with_memory(module(Vec::new(), vec![ValueType::I32], body)),
            0,
            &[0x78, 0x56, 0x34, 0x12],
        );
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(0x1234_5678u32 as i32)]
        );
    }

    #[test]
    fn a_data_segment_at_a_nonzero_offset_lands_where_asked() {
        // A load at address 0 must miss the segment written at address 4, so a
        // wrong offset shows up as a zero rather than the expected bytes.
        let mut body = const_i32(0);
        body.extend_from_slice(&[0x28, 0x02, 0x00]);
        body.push(0x0b);
        let module = with_data(
            with_memory(module(Vec::new(), vec![ValueType::I32], body)),
            4,
            &[0xff, 0xff, 0xff, 0xff],
        );
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(0)]
        );
    }

    #[test]
    fn a_store_overwrites_data_segment_contents() {
        // The segment seeds address 0; the function then replaces it, so a
        // missing segment would still pass but a missing store would not.
        let mut body = const_i32(0);
        body.extend(const_i32(0x1111_1111));
        body.extend_from_slice(&[0x36, 0x02, 0x00]);
        body.extend(const_i32(0));
        body.extend_from_slice(&[0x28, 0x02, 0x00]);
        body.push(0x0b);
        let module = with_data(
            with_memory(module(Vec::new(), vec![ValueType::I32], body)),
            0,
            &[0x22, 0x33, 0x44, 0x55],
        );
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(0x1111_1111u32 as i32)]
        );
    }

    #[test]
    fn a_later_data_segment_overwrites_an_earlier_one() {
        // Two segments overlap at address 0; the second must win, which is the
        // order the specification requires them to be applied in.
        let mut body = const_i32(0);
        body.extend_from_slice(&[0x28, 0x02, 0x00]);
        body.push(0x0b);
        let module = with_data(
            with_data(
                with_memory(module(Vec::new(), vec![ValueType::I32], body)),
                0,
                &[0x01, 0x00, 0x00, 0x00],
            ),
            0,
            &[0x02, 0x00, 0x00, 0x00],
        );
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(2)]
        );
    }

    #[test]
    fn memory_grow_keeps_data_segment_contents() {
        // Growth appends zeroed pages; the segment's bytes must survive it. The
        // grow result is dropped so only the reload is left on the stack.
        let mut body = const_i32(1);
        body.extend_from_slice(&[0x40, 0x00]); // memory.grow
        body.extend_from_slice(&[0x1a]); // drop the old page count
        body.extend(const_i32(0));
        body.extend_from_slice(&[0x28, 0x02, 0x00]);
        body.push(0x0b);
        let module = with_data(
            with_memory(module(Vec::new(), vec![ValueType::I32], body)),
            0,
            &[0x2a, 0x00, 0x00, 0x00],
        );
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    /// `ref.null` with the given reference type, 0x70 being `funcref`.
    fn ref_null(reference_type: u8) -> Vec<u8> {
        vec![0xd0, reference_type]
    }

    /// `ref.is_null`, which turns any reference into an `i32`.
    const REF_IS_NULL: u8 = 0xd1;

    /// A module whose first function exercises a reference and returns an
    /// `i32`, with a second function present so `ref.func 1` has something to
    /// name.
    fn reference_module(body: Vec<u8>) -> Module {
        let mut module = module(Vec::new(), vec![ValueType::I32], body);
        module.functions.push(Function {
            type_index: 0,
            locals: vec![],
            body: const_i32(0).into_iter().chain([0x0b]).collect(),
        });
        module
    }

    #[test]
    fn baseline_matches_micro_for_ref_null() {
        for (name, reference_type) in [("funcref", 0x70u8), ("externref", 0x6f)] {
            let mut body = ref_null(reference_type);
            body.push(REF_IS_NULL);
            body.push(0x0b);
            let module = reference_module(body);
            assert_eq!(
                assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
                vec![Value::I32(1)],
                "{name}"
            );
        }
    }

    #[test]
    fn baseline_matches_micro_for_ref_func() {
        // `ref.func 1` names a real function, so the result is not null.
        let mut body = vec![0xd2, 0x01];
        body.push(REF_IS_NULL);
        body.push(0x0b);
        let module = reference_module(body);
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(0)]
        );
    }

    #[test]
    fn baseline_matches_micro_for_a_funcref_returned_unchanged() {
        // Returning the reference itself compares the two backends' value
        // representations directly, not only the `i32` derived from it. A second
        // type is added so `$target` keeps its own `i32` signature and does not
        // have to satisfy the reference-typed one.
        let mut module = reference_module(Vec::new());
        module.types.push(FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::Ref(ReferenceType::FuncRef)]),
        });
        for (body, expected) in [
            (
                vec![0xd0, 0x70, 0x0b],
                Value::Ref(RefValue::Null(ReferenceType::FuncRef)),
            ),
            (vec![0xd2, 0x00, 0x0b], Value::Ref(RefValue::FuncRef(0))),
        ] {
            module.functions[0].type_index = 1;
            module.functions[0].body = body;
            assert_eq!(
                assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
                vec![expected]
            );
        }
    }

    #[test]
    fn baseline_matches_micro_for_a_reference_through_a_local() {
        // A reference stored in a local and read back exercises `local.set` and
        // `local.get` against a reference-typed slot.
        let mut body = ref_null(0x70);
        body.extend_from_slice(&[0x21, 0x00]); // local.set 0
        body.extend_from_slice(&[0x20, 0x00]); // local.get 0
        body.push(REF_IS_NULL);
        body.push(0x0b);
        let mut module = reference_module(body);
        module.functions[0].locals = vec![tpt_wasm_format::LocalDecl {
            count: 1,
            value_type: ValueType::Ref(ReferenceType::FuncRef),
        }];
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(1)]
        );
    }

    #[test]
    fn baseline_matches_micro_for_ref_is_null_under_control_flow() {
        // The null test decides which branch runs, so a `ref.is_null` that
        // always answered the same way would take the wrong edge.
        let mut body = ref_null(0x70);
        body.push(REF_IS_NULL);
        body.extend_from_slice(&[0x04, 0x7f]); // if (result i32)
        body.extend(const_i32(10)); // then
        body.push(0x05); // else
        body.extend(const_i32(20));
        body.push(0x0b); // end if
        body.push(0x0b); // end function
        let module = reference_module(body);
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(10)],
            "a null reference takes the then arm"
        );

        // The same shape over a real function reference must take the other
        // edge, so the two together pin the answer rather than just the edge.
        let mut body = vec![0xd2, 0x00];
        body.push(REF_IS_NULL);
        body.extend_from_slice(&[0x04, 0x7f]); // if (result i32)
        body.extend(const_i32(10)); // then
        body.push(0x05); // else
        body.extend(const_i32(20));
        body.push(0x0b); // end if
        body.push(0x0b); // end function
        let module = reference_module(body);
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(20)],
            "a non-null reference takes the else arm"
        );
    }

    #[test]
    fn baseline_matches_micro_for_a_reference_through_a_block() {
        // A `funcref` result on the block type is the block-parameter path: the
        // value is carried on the branch edge into the merge block rather than
        // left on a stack, so this covers a reference surviving control flow.
        // The block's result is produced by its body, so the null is pushed
        // inside the block rather than before it.
        let mut body = vec![0x02, 0x70]; // block (result funcref)
        body.extend(ref_null(0x70));
        body.push(0x0b); // end block
        body.push(REF_IS_NULL);
        body.push(0x0b); // end function
        let module = reference_module(body);
        assert_eq!(
            assert_runnable_matches_micro(&module, 0, Vec::new()).unwrap(),
            vec![Value::I32(1)]
        );
    }

    #[test]
    fn a_ref_func_to_a_missing_function_is_refused() {
        // The index names no function. The validator is the first gate and
        // rejects it, so the module never reaches lowering or execution.
        let mut body = vec![0xd2, 0x09];
        body.push(REF_IS_NULL);
        body.push(0x0b);
        assert_eq!(
            tpt_wasm_validate::validate(reference_module(body)),
            Err(tpt_wasm_validate::ValidationError::UnknownFunction(9))
        );
    }

    /// A host that records the calls it received, so the arguments a backend
    /// passes can be compared, not only the result it gets back.
    #[derive(Default)]
    struct RecordingHost {
        calls: Vec<(String, Vec<Value>)>,
        /// Doubles the single `i32` argument, the usual host shape.
        double: bool,
        fail: bool,
    }

    impl super::HostBoundary for RecordingHost {
        fn call(
            &mut self,
            module: &str,
            name: &str,
            args: Vec<Value>,
        ) -> Result<Vec<Value>, super::HostCallFailure> {
            let qualified = format!("{module}::{name}");
            self.calls.push((qualified.clone(), args.clone()));
            if self.fail {
                return Err(super::HostCallFailure {
                    name: qualified,
                    message: "refused by the test host".into(),
                });
            }
            let Value::I32(value) = args.first().cloned().unwrap_or(Value::I32(0)) else {
                return Err(super::HostCallFailure {
                    name: qualified,
                    message: "the test host only takes an i32".into(),
                });
            };
            let result = if self.double { value * 2 } else { value };
            Ok(vec![Value::I32(result)])
        }
    }

    /// A module importing `env::twice(i32) -> i32`, whose `run` calls it and
    /// returns the result.
    fn host_calling_module() -> Module {
        let twice = FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        };
        Module {
            types: vec![twice.clone()],
            imports: vec![tpt_wasm_format::Import {
                module: "env".into(),
                name: "twice".into(),
                desc: tpt_wasm_format::ImportDesc::Function(0),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: vec![],
                // local.get 0; call 0 (the import); end
                body: vec![0x20, 0x00, 0x10, 0x00, 0x0b],
            }],
            ..Module::default()
        }
    }

    /// Run an importing module through both backends with the same host
    /// behaviour, returning both results and both call logs.
    fn assert_host_call_matches_micro(module: &Module, double: bool, fail: bool) {
        let validated = tpt_wasm_validate::validate(module.clone()).expect("fixture must validate");
        let verified = lower_and_verify(&validated).expect("fixture must lower and verify");
        let mut baseline =
            BaselineModule::lower(verified.module()).expect("codegen must accept it");
        let baseline_host = RecordingHost {
            double,
            fail,
            ..RecordingHost::default()
        };
        baseline.set_host(RefCellHost(baseline_host));
        // The entry function is Wasm index 1: the import occupies index 0, and
        // the baseline's own table starts at 0.
        let baseline_result = baseline.call(0, vec![Value::I32(21)]);

        let micro_host = RecordingHost {
            double,
            fail,
            ..RecordingHost::default()
        };
        let micro_result = run_in_micro_with_host(
            module,
            0,
            vec![Value::I32(21)],
            Some(Box::new(RefCellHost(micro_host))),
        );

        // The success path is compared exactly. The failure path is compared on
        // the host's own message: the baseline keeps a host failure distinct
        // from a trap, while this harness reports Micro's as a trap, so the
        // common part is the message the host itself produced.
        let from_baseline: Result<String, String> = baseline_result
            .as_ref()
            .map(|values| format!("{values:?}"))
            .map_err(|error| match error {
                BaselineError::Host(failure) => failure.message.clone(),
                BaselineError::Trap(trap) => format!("{trap:?}"),
            });
        let from_micro: Result<String, String> = micro_result
            .as_ref()
            .map(|values| format!("{values:?}"))
            .map_err(|trap| match trap {
                Trap::HostFailure(message) => message.clone(),
                other => format!("{other:?}"),
            });
        assert_eq!(
            from_baseline, from_micro,
            "the two backends disagreed about the host call"
        );
    }

    /// Lets the harness hand out `&mut` access to a host it also owns.
    struct RefCellHost(RecordingHost);

    impl super::HostBoundary for RefCellHost {
        fn call(
            &mut self,
            module: &str,
            name: &str,
            args: Vec<Value>,
        ) -> Result<Vec<Value>, super::HostCallFailure> {
            self.0.call(module, name, args)
        }
    }

    #[test]
    fn baseline_and_micro_reach_the_same_host_result() {
        // The host doubles the argument, so a call that did not cross the
        // boundary, crossed it with the wrong argument, or dropped the result
        // would all be caught.
        assert_host_call_matches_micro(&host_calling_module(), true, false);
    }

    #[test]
    fn a_refused_host_call_fails_in_both_backends() {
        assert_host_call_matches_micro(&host_calling_module(), true, true);
    }

    /// A deterministic xorshift generator.
    ///
    /// The project forbids ambient randomness in execution, and a randomized
    /// test needs the same discipline: every program is reproducible from its
    /// seed alone, so a failure can be replayed without recording the input.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            // A zero state is a fixed point of xorshift, so it is avoided.
            Self(seed | 1)
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, bound: u32) -> u32 {
            (self.next_u64() >> 33) as u32 % bound.max(1)
        }

        fn bits32(&mut self) -> u32 {
            (self.next_u64() >> 32) as u32
        }

        fn bits64(&mut self) -> u64 {
            self.next_u64()
        }
    }

    /// Builds one random but well-typed Wasm function body.
    ///
    /// The generator tracks the operand stack it is emitting against, so every
    /// program it produces is valid by construction. That matters: a fuzzer that
    /// mostly produced invalid modules would exercise the validator rather than
    /// the two backends.
    struct Generator {
        rng: Rng,
        body: Vec<u8>,
        /// The value types currently on the operand stack.
        stack: Vec<ValueType>,
        /// How deeply control flow is currently nested.
        ///
        /// The structured steps below recurse into `step`, so this bounds the
        /// nesting rather than letting a seed grow it without limit.
        depth: u32,
    }

    /// The conversions that produce an `i32`, none of which can trap.
    ///
    /// A program has to end with an `i32` to be a usable fixture, and trapping on
    /// the way out would replace the interesting part with a trap before the
    /// program finished.
    const TO_I32: &[(ValueType, &[u8])] = &[
        (ValueType::I32, &[]),
        (ValueType::I64, &[0xa7]),       // i32.wrap_i64
        (ValueType::F32, &[0xbc]),       // i32.reinterpret_f32
        (ValueType::F64, &[0xbd, 0xa7]), // i64.reinterpret_f64 then i32.wrap_i64
    ];

    impl Generator {
        fn new(seed: u64) -> Self {
            Self {
                rng: Rng::new(seed),
                body: Vec::new(),
                stack: Vec::new(),
                depth: 0,
            }
        }

        /// Push a constant of `value_type`. Float constants are written as raw
        /// bits, so the generator reaches NaN payloads, infinities, subnormals,
        /// and signed zeroes rather than only round decimal values.
        fn push(&mut self, value_type: ValueType) {
            match value_type {
                ValueType::I32 => {
                    self.body.push(0x41);
                    self.body.extend(encode_i32(self.rng.bits32() as i32));
                }
                ValueType::I64 => {
                    self.body.push(0x42);
                    self.body.extend(encode_i64(self.rng.bits64() as i64));
                }
                ValueType::F32 => {
                    self.body.push(0x43);
                    self.body.extend(self.rng.bits32().to_le_bytes());
                }
                ValueType::F64 => {
                    self.body.push(0x44);
                    self.body.extend(self.rng.bits64().to_le_bytes());
                }
                other => panic!("the generator does not produce {other:?}"),
            }
            self.stack.push(value_type);
        }

        /// Pop `count` operands, asserting the generator's own stack discipline.
        fn pop(&mut self, count: usize) {
            for _ in 0..count {
                self.stack
                    .pop()
                    .expect("the generator emitted more operands than it produced");
            }
        }

        /// Push a specific `i32`, for a memory address or a divisor the program
        /// chooses. Tracking the push is what keeps the generated stack in step
        /// with the emitted one.
        fn push_i32(&mut self, value: i32) {
            self.body.push(0x41);
            self.body.extend(encode_i32(value));
            self.stack.push(ValueType::I32);
        }

        /// Push a specific `i64`, the 64-bit counterpart of [`Generator::push_i32`].
        fn push_i64(&mut self, value: i64) {
            self.body.push(0x42);
            self.body.extend(encode_i64(value));
            self.stack.push(ValueType::I64);
        }

        /// Emit a binary operation drawn from a contiguous opcode range.
        fn binary(&mut self, operand: ValueType, first: u8, last: u8, result: ValueType) {
            self.push(operand);
            self.push(operand);
            self.body
                .push(first + self.rng.below(u32::from(last - first) + 1) as u8);
            self.pop(2);
            self.stack.push(result);
        }

        /// Emit a unary operation drawn from a contiguous opcode range.
        fn unary(&mut self, operand: ValueType, first: u8, last: u8, result: ValueType) {
            self.push(operand);
            self.body
                .push(first + self.rng.below(u32::from(last - first) + 1) as u8);
            self.pop(1);
            self.stack.push(result);
        }

        /// Emit a conversion that always succeeds.
        fn convert(&mut self, from: ValueType, opcodes: &[u8], to: ValueType) {
            self.push(from);
            self.body.extend_from_slice(opcodes);
            self.pop(1);
            self.stack.push(to);
        }

        /// Push a float small enough that converting it to an integer cannot
        /// trap, so a truncation is exercised on its real path rather than
        /// ending the program at its first use.
        fn push_truncatable(&mut self, value_type: ValueType) {
            let small = (self.rng.below(2_001) as i32) - 1000;
            match value_type {
                ValueType::F32 => {
                    self.body.push(0x43);
                    self.body.extend((small as f32).to_bits().to_le_bytes());
                }
                ValueType::F64 => {
                    self.body.push(0x44);
                    self.body.extend((small as f64).to_bits().to_le_bytes());
                }
                other => panic!("{other:?} is not truncatable"),
            }
            self.stack.push(value_type);
        }

        /// Emit a float-to-integer truncation whose operand stays in range.
        fn truncate(&mut self, from: ValueType, opcodes: &[u8], to: ValueType) {
            self.push_truncatable(from);
            self.body.extend_from_slice(opcodes);
            self.pop(1);
            self.stack.push(to);
        }

        /// Emit an integer division or remainder whose divisor is non-zero and
        /// positive.
        ///
        /// A zero divisor traps and `i32::MIN / -1` overflows, so either would
        /// end the program at the division instead of exercising the arithmetic.
        fn divrem(&mut self, width32: bool, opcode: u8) {
            let value_type = if width32 {
                ValueType::I32
            } else {
                ValueType::I64
            };
            let divisor = 1 + i64::from(self.rng.below(0x3fff_ffff));
            // Both operands are pushed through the tracked helpers, because the
            // operation pops the dividend and the divisor together.
            self.push(value_type);
            if width32 {
                self.push_i32(divisor as i32);
            } else {
                self.push_i64(divisor);
            }
            self.body.push(opcode);
            self.pop(2);
            self.stack.push(value_type);
        }

        /// Emit a store of a fresh value at an in-bounds address, then a load of
        /// the same width at another, so the two backends must agree on what the
        /// round trip observes.
        fn memory_round_trip(&mut self, value_type: ValueType, store: u8, load: u8, align: u8) {
            // The address stays low so every width remains inside one page. Both
            // addresses go through `push_i32`, so the tracked stack matches the
            // emitted one; a store pops the address and the value together.
            let first = self.rng.below(1024) as i32;
            self.push_i32(first);
            self.push(value_type);
            self.body.push(store);
            self.body.push(align);
            self.body.push(0x00); // offset
            self.pop(2);
            let second = self.rng.below(1024) as i32;
            self.push_i32(second);
            self.body.push(load);
            self.body.push(align);
            self.body.push(0x00); // offset
            self.pop(1);
            self.stack.push(value_type);
        }

        /// Add one random, well-typed step to the program.
        ///
        /// Every branch emits only operations whose operand types it supplies
        /// itself, so the generated body is valid without a type-inference pass.
        fn step(&mut self) {
            // 22 rather than 19: `br_table` and the two reference cases join the
            // control-flow and call families.
            match self.rng.below(22) {
                0 => self.binary(ValueType::I32, 0x6a, 0x78, ValueType::I32),
                1 => self.binary(ValueType::I32, 0x46, 0x4f, ValueType::I32),
                2 => self.binary(ValueType::I64, 0x7c, 0x8a, ValueType::I64),
                // 0x52, not 0x51: `i64.eqz` is unary, so a range starting there
                // would emit an operation with the wrong operand arity.
                3 => self.binary(ValueType::I64, 0x52, 0x5a, ValueType::I32),
                4 => self.binary(ValueType::F32, 0x92, 0x98, ValueType::F32),
                5 => self.binary(ValueType::F32, 0x5b, 0x60, ValueType::I32),
                6 => self.binary(ValueType::F64, 0xa0, 0xa6, ValueType::F64),
                7 => self.binary(ValueType::F64, 0x61, 0x66, ValueType::I32),
                8 => self.unary(ValueType::F32, 0x8b, 0x91, ValueType::F32),
                9 => self.unary(ValueType::F64, 0x99, 0x9f, ValueType::F64),
                10 => {
                    if self.rng.below(2) == 0 {
                        // i32 div_s, div_u, rem_s, or rem_u. The opcode is drawn
                        // first so the borrow ends before the call below.
                        let opcode = 0x6d + self.rng.below(4) as u8;
                        self.divrem(true, opcode);
                    } else {
                        // The i64 equivalents.
                        let opcode = 0x7f + self.rng.below(4) as u8;
                        self.divrem(false, opcode);
                    }
                }
                11 => match self.rng.below(9) {
                    // The bit-count operations, at both widths.
                    0 => self.unary(ValueType::I32, 0x67, 0x69, ValueType::I32),
                    1 => self.unary(ValueType::I64, 0x79, 0x7b, ValueType::I64),
                    2 => self.convert(ValueType::I64, &[0xa7], ValueType::I32),
                    3 => self.convert(ValueType::I32, &[0xac], ValueType::I64),
                    4 => self.convert(ValueType::I32, &[0xad], ValueType::I64),
                    5 => self.convert(ValueType::F64, &[0xb6], ValueType::F32),
                    6 => self.convert(ValueType::F32, &[0xbb], ValueType::F64),
                    7 => self.convert(ValueType::F32, &[0xbc], ValueType::I32),
                    8 => self.convert(ValueType::I32, &[0xbe], ValueType::F32),
                    _ => self.convert(ValueType::I64, &[0xbf], ValueType::F64),
                },
                12 => match self.rng.below(4) {
                    0 => self.truncate(ValueType::F32, &[0xa8], ValueType::I32),
                    1 => self.truncate(ValueType::F64, &[0xaa], ValueType::I32),
                    2 => self.truncate(ValueType::F32, &[0xae], ValueType::I64),
                    _ => self.truncate(ValueType::F64, &[0xb0], ValueType::I64),
                },
                13 => self.if_else(),
                14 => self.block_with_branch(),
                15 => self.counted_loop(self.depth),
                16 => self.call_helper(),
                17 => self.call_indirect(),
                18 => self.branch_table(),
                19 => self.reference_is_null(),
                20 => self.reference_through_a_block(),
                _ => {
                    if self.rng.below(2) == 0 {
                        // A narrow store and load, whose sign extension differs.
                        self.memory_round_trip(ValueType::I32, 0x3a, 0x2c, 0x00);
                    } else {
                        match self.rng.below(4) {
                            0 => self.memory_round_trip(ValueType::I32, 0x36, 0x28, 0x02),
                            1 => self.memory_round_trip(ValueType::I64, 0x37, 0x29, 0x03),
                            2 => self.memory_round_trip(ValueType::F32, 0x38, 0x2a, 0x02),
                            _ => self.memory_round_trip(ValueType::F64, 0x39, 0x2b, 0x03),
                        }
                    }
                }
            }
        }

        /// Discard everything above `height` on the tracked stack.
        ///
        /// A structured step must leave the operand stack as it found it, and
        /// `drop` is how the generator gets there without asking the validator.
        fn drop_to(&mut self, height: usize) {
            while self.stack.len() > height {
                self.body.push(0x1a); // drop
                self.pop(1);
            }
        }

        /// A `block` with a conditional branch out of it, carrying an `i32`.
        ///
        /// The branch is taken or not depending on the drawn condition, so both
        /// the taken and the falling-through path are exercised, and the value
        /// survives either way.
        fn block_with_branch(&mut self) {
            let result = ValueType::I32;
            let height = self.stack.len();
            self.body.push(0x02); // block
            self.body.push(0x7f); // (result i32)
            self.depth += 1;
            if self.depth <= 2 && self.rng.below(2) == 0 {
                self.step();
                // The block's own result is pushed next, so whatever the nested
                // step left has to go first: a block leaves exactly its result
                // type, and `br_if` to it needs an `i32` on top.
                self.drop_to(height);
            }
            self.push(result);
            // The condition is drawn, not derived, so a branch is sometimes
            // taken; both edges then have to agree between the backends.
            let taken = i32::from(self.rng.below(2) == 0);
            self.push_i32(taken);
            self.body.push(0x0d); // br_if 0
            self.body.push(0x00); // to the block's end
            self.pop(1);
            self.depth -= 1;
            self.body.push(0x0b); // end
            self.stack.truncate(height);
            self.stack.push(result);
        }

        /// An `if`/`else` on a drawn condition, with an `i32` on each arm.
        ///
        /// Both arms leave exactly the block's result type, so the program stays
        /// well typed whichever edge runs.
        fn if_else(&mut self) {
            let result = ValueType::I32;
            let height = self.stack.len();
            let condition = i32::from(self.rng.below(2) == 0);
            self.push_i32(condition);
            self.body.push(0x04); // if
            self.body.push(0x7f); // (result i32)
            self.depth += 1;
            self.push(result);
            self.body.push(0x05); // else
                                  // The else arm is a *separate* path: it starts from the block's entry
                                  // height, not from whatever the then arm left, so the tracked stack is
                                  // reset to match the value stack the runtime will actually see. The
                                  // same reset discards the `if` condition, which `if` itself consumed.
            self.stack.truncate(height);
            if self.depth <= 2 && self.rng.below(2) == 0 {
                self.step();
                self.drop_to(height);
            }
            self.push(result);
            self.depth -= 1;
            self.body.push(0x0b); // end
            self.stack.truncate(height);
            self.stack.push(result);
        }

        /// A counted `loop`: the body runs, then branches back while the counter
        /// local is nonzero.
        ///
        /// The counter is seeded with a small bound, so the loop always
        /// terminates. `local` is the index of the `i32` local reserved for it,
        /// and each nesting level takes its own: a nested loop sharing the
        /// outer's counter would zero it on the way out, leaving the outer loop
        /// to count down through the whole `i32` range and never terminate.
        fn counted_loop(&mut self, local: u32) {
            let height = self.stack.len();
            let bound = 1 + self.rng.below(6) as i32;
            self.push_i32(bound);
            self.body.push(0x21); // local.set
            self.body.push(local as u8);
            self.pop(1);
            self.body.push(0x03); // loop
            self.body.push(0x40); // (no result)
            self.depth += 1;
            if self.depth <= 2 && self.rng.below(2) == 0 {
                self.step();
                self.drop_to(height);
            }
            self.body.push(0x20); // local.get
            self.body.push(local as u8);
            self.body.push(0x41); // i32.const 1
            self.body.push(0x01);
            self.body.push(0x6b); // i32.sub
            self.body.push(0x22); // local.tee
            self.body.push(local as u8);
            self.body.push(0x0d); // br_if 0
            self.body.push(0x00); // to the loop header
            self.depth -= 1;
            self.body.push(0x0b); // end
            self.stack.truncate(height);
        }

        /// A direct call to the generated module's helper function.
        ///
        /// The helper takes and returns an `i32`, so the argument and the result
        /// both have to survive the call for the two backends to agree.
        fn call_helper(&mut self) {
            let height = self.stack.len();
            self.push(ValueType::I32);
            self.body.push(0x10); // call
            self.body.push(0x01); // function 1, the helper
            self.pop(1);
            self.stack.push(ValueType::I32);
            debug_assert!(self.stack.len() > height);
        }

        /// An indirect call through the table, against the generated module's
        /// single entry.
        ///
        /// The index is usually in range and the declared type usually matches, so
        /// a call often succeeds. The remaining draws reach the other three
        /// outcomes: a null entry, an index past the end, and a declared type the
        /// seeded entry does not have, which is a signature mismatch rather than an
        /// invalid module.
        fn call_indirect(&mut self) {
            let choice = self.rng.below(6);
            let (index, type_index) = match choice {
                // The seeded entry, called with its own type: a real dispatch.
                0 => (0, 0x01),
                1 => (0, 0x01),
                2 => (0, 0x01),
                // A null entry, an index past the end of the table, and a
                // declared type the entry does not have.
                3 => (1, 0x01),
                4 => (3, 0x01),
                _ => (0, 0x00),
            };
            // Type 1 is `(i32) -> i32`, so that call needs an argument pushed
            // *below* the table index; type 0 takes none. The argument count has
            // to follow the declared type, or the call is missing an operand.
            let arguments = usize::from(type_index == 0x01);
            for _ in 0..arguments {
                self.push(ValueType::I32);
            }
            self.push_i32(index);
            self.body.push(0x11); // call_indirect
            self.body.push(type_index);
            self.body.push(0x00); // table 0
            self.pop(arguments + 1);
            self.stack.push(ValueType::I32);
        }

        /// A `br_table` over two nested blocks that both carry an `i32`.
        ///
        /// Every label of a `br_table` must have the same arity, so the two blocks
        /// are declared with the same result type and the table's two targets plus
        /// its default are drawn from them. The selector is drawn rather than
        /// derived, so an out-of-range index takes the default and both the
        /// in-range and the default edge are exercised.
        fn branch_table(&mut self) {
            let result = ValueType::I32;
            let height = self.stack.len();
            self.body.push(0x02); // block
            self.body.push(0x7f); // (result i32)
            self.body.push(0x02); // block
            self.body.push(0x7f); // (result i32), the same arity
            self.depth += 1;
            self.push(result);
            // The selector is drawn from 0..=2 against two labels, so the third
            // draw falls past the end and takes the default.
            let selector = self.rng.below(3) as i32;
            self.push_i32(selector);
            self.body.push(0x0e); // br_table
            self.body.push(0x02); // two labels
            self.body.push(0x00); // label 0: the inner block
            self.body.push(0x01); // label 1: the outer block
            self.body.push(0x01); // default: the outer block
            self.pop(2); // the selector and the carried value
            self.depth -= 1;
            self.body.push(0x0b); // end (inner)
            self.body.push(0x0b); // end (outer)
                                  // A `br_table` is unconditional, so nothing after it runs: both
                                  // blocks are left with the carried `i32` and nothing else.
            self.stack.truncate(height);
            self.stack.push(result);
        }

        /// A reference value reduced to the `i32` the fixture's signature wants.
        ///
        /// A `null` of either reference type, and a `ref.func` naming the
        /// generated entry point itself, are each tested with `ref.is_null`. The
        /// non-null case matters as much as the null one: only a real function
        /// address distinguishes a correct `ref.func` from a fabricated one.
        fn reference_is_null(&mut self) {
            match self.rng.below(3) {
                // ref.null func, ref.null extern, then ref.is_null.
                0 => {
                    self.body.push(0xd0);
                    self.body.push(0x70); // func
                }
                1 => {
                    self.body.push(0xd0);
                    self.body.push(0x6f); // extern
                }
                // ref.func 0, naming the generated function itself.
                _ => {
                    self.body.push(0xd2);
                    self.body.push(0x00);
                }
            }
            self.stack.push(ValueType::Ref(ReferenceType::FuncRef));
            self.body.push(0xd1); // ref.is_null
            self.pop(1);
            self.stack.push(ValueType::I32);
        }

        /// A `funcref` carried out of a block, then reduced to an `i32`.
        ///
        /// This is the two reference features at once: the block's result type is
        /// a reference, and the value is moved by a branch rather than by falling
        /// off the end, so a reference has to survive both a taken and an
        /// untaken branch.
        fn reference_through_a_block(&mut self) {
            let height = self.stack.len();
            self.body.push(0x02); // block
            self.body.push(0x70); // (result funcref)
            self.depth += 1;
            // The null the fall-through starts from.
            self.body.push(0xd0);
            self.body.push(0x70); // ref.null func
            self.stack.push(ValueType::Ref(ReferenceType::FuncRef));
            self.depth -= 1;
            let taken = i32::from(self.rng.below(2) == 0);
            self.push_i32(taken);
            self.body.push(0x0d); // br_if 0
            self.body.push(0x00);
            self.pop(1);
            // The fall-through replaces the null with a real function reference.
            self.body.push(0x1a); // drop
            self.body.push(0xd2); // ref.func
            self.body.push(0x00);
            self.stack.push(ValueType::Ref(ReferenceType::FuncRef));
            self.body.push(0x0b); // end
            self.stack.truncate(height);
            self.stack.push(ValueType::Ref(ReferenceType::FuncRef));
            self.body.push(0xd1); // ref.is_null
            self.pop(1);
            self.stack.push(ValueType::I32);
        }

        /// Keep a mutable global in step with the values on the operand stack.
        ///
        /// A `global.set` is emitted only when the top of the tracked stack is the
        /// global's type, and the value is read straight back, so a stale or
        /// shared slot shows up as a divergence rather than passing by luck.
        fn spill_to_global(&mut self) {
            if self.stack.last().copied() != Some(ValueType::I32) {
                return;
            }
            // The index immediate is required; without it the following byte would
            // be read as the global index.
            self.body.push(0x24);
            self.body.push(0x00); // global.set 0
            self.pop(1);
            self.body.push(0x23);
            self.body.push(0x00); // global.get 0
            self.stack.push(ValueType::I32);
        }

        /// Drop everything above the bottom operand and report the type left on it.
        ///
        /// This is the body proper: the conversion and the final `end` that
        /// `finish` appends are left off, so a caller can validate a prefix.
        fn finish_prefix(mut self) -> (Vec<u8>, ValueType) {
            if self.stack.is_empty() {
                self.push(ValueType::I32);
            }
            while self.stack.len() > 1 {
                self.body.push(0x1a); // drop
                self.pop(1);
            }
            let top = self.stack[0];
            (self.body, top)
        }

        /// Drop everything above the bottom operand and convert it to the `i32`
        /// the fixture's signature declares.
        fn finish(self) -> Vec<u8> {
            let (mut body, top) = self.finish_prefix();
            let conversion = TO_I32
                .iter()
                .find(|(from, _)| *from == top)
                .expect("every generated type converts to an i32")
                .1;
            body.extend_from_slice(conversion);
            body.push(0x0b); // end
            body
        }
    }

    /// The module a generated body is placed in.
    ///
    /// The generated function is the entry point, alongside everything it can
    /// reach: a page of memory, a mutable global, an `i32` local for the loop
    /// counter, a helper function, and a table seeded with that same helper so an
    /// indirect call can succeed. Giving the generated program that surface is
    /// what lets the steps exercise calls and branches rather than only
    /// arithmetic.
    fn generated_program(seed: u64) -> Module {
        let mut generator = Generator::new(seed);
        let steps = 3 + generator.rng.below(24);
        for _ in 0..steps {
            generator.step();
            // Route an `i32` through the module-level global, so a stale or
            // shared slot shows up as a divergence rather than passing by luck.
            generator.spill_to_global();
        }
        let body = generator.finish();
        let mut module = with_memory(Module {
            types: vec![
                // 0: the generated entry point.
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(vec![ValueType::I32]),
                },
                // 1: the helper, shared by the direct and indirect call.
                FunctionType {
                    params: ResultType(vec![ValueType::I32]),
                    results: ResultType(vec![ValueType::I32]),
                },
            ],
            functions: vec![
                Function {
                    type_index: 0,
                    // One `i32` local per nesting level, reserved for loop
                    // counters. Locals are zero-initialized, so a loop that read
                    // before writing would still terminate.
                    locals: vec![tpt_wasm_format::LocalDecl {
                        count: 4,
                        value_type: ValueType::I32,
                    }],
                    body,
                },
                // 1: local.get 0; i32.const 3; i32.mul; i32.const 1; i32.add.
                // Doubling and offsetting means a wrong argument, a wrong
                // dispatch, or a shared call frame would all show up.
                Function {
                    type_index: 1,
                    locals: vec![],
                    body: vec![0x20, 0x00, 0x41, 0x03, 0x6c, 0x41, 0x01, 0x6a, 0x0b],
                },
            ],
            tables: vec![tpt_wasm_format::Table {
                table_type: tpt_wasm_types::TableType {
                    element_type: tpt_wasm_types::ReferenceType::FuncRef,
                    // Two entries, only the first of which is initialized, so an
                    // index of 1 is a null entry and 2 or more is out of range.
                    limits: tpt_wasm_types::Limits { min: 2, max: None },
                },
                init: None,
            }],
            elements: vec![tpt_wasm_format::Element {
                element_type: tpt_wasm_types::RefType::FuncRef,
                mode: tpt_wasm_format::ElementMode::Active {
                    table_index: 0,
                    offset: tpt_wasm_format::ConstExpr(vec![0x41, 0x00, 0x0b]),
                },
                init: tpt_wasm_format::ElementInit::FuncIndices(vec![1]),
            }],
            ..Module::default()
        });
        // `global.get`/`global.set` round trip through the module-level slot the
        // compiled instance owns, so a stale or shared global shows up as a
        // divergence rather than passing by luck.
        module.globals.push(tpt_wasm_format::Global {
            global_type: tpt_wasm_types::GlobalType {
                value_type: ValueType::I32,
                mutable: true,
            },
            init: tpt_wasm_format::ConstExpr(vec![0x41, 0x00, 0x0b]),
        });
        module
    }

    /// Compile one generated program both ways and require the same outcome.
    ///
    /// Each stage reports separately, so a failure says whether the generator
    /// produced something invalid, the pipeline rejected it, or the two backends
    /// actually disagreed. The seed and the body bytes are in every message, so a
    /// failure is reproducible without having saved an artifact.
    fn assert_generated_matches_micro(seed: u64) {
        let module = generated_program(seed);
        let body = format!("{:02x?}", module.functions[0].body);
        let validated = tpt_wasm_validate::validate(module.clone())
            .unwrap_or_else(|error| panic!("seed {seed}: invalid module: {error:?}\n{body}"));
        let verified = lower_and_verify(&validated)
            .unwrap_or_else(|error| panic!("seed {seed}: did not lower: {error:?}\n{body}"));
        let mut baseline = BaselineModule::lower(verified.module())
            .unwrap_or_else(|error| panic!("seed {seed}: did not compile: {error:?}\n{body}"));
        let micro_result = run_in_micro(&module, 0, Vec::new());
        let baseline_result = baseline.call(0, Vec::new());
        let from_baseline: Result<&[Value], Trap> = match &baseline_result {
            Ok(values) => Ok(values),
            Err(error) => Err(trap_of(error)),
        };
        let from_micro: Result<&[Value], Trap> = match &micro_result {
            Ok(values) => Ok(values),
            Err(trap) => Err(trap.clone()),
        };
        assert_eq!(
            from_baseline, from_micro,
            "seed {seed}: the backends disagreed\n{body}"
        );
    }

    /// The smallest `br_table` that the generator emits, checked directly so a
    /// disagreement is attributed to the construct rather than to a whole program.
    #[test]
    fn br_table_over_two_nested_blocks_matches_micro() {
        for selector in 0..4i32 {
            let mut body = vec![0x02, 0x7f, 0x02, 0x7f];
            body.extend(const_i32(7));
            body.extend(const_i32(selector));
            body.extend_from_slice(&[0x0e, 0x02, 0x00, 0x01, 0x01, 0x0b, 0x0b, 0x0b]);
            let module = module(Vec::new(), vec![ValueType::I32], body);
            assert_eq!(
                assert_matches_micro(&module.functions[0].body, ValueType::I32).unwrap(),
                vec![Value::I32(7)],
                "selector {selector}"
            );
        }
    }

    #[test]
    fn baseline_matches_micro_over_generated_programs() {
        // Enough programs to reach combinations the hand-written fixtures do
        // not, while staying fast enough for ordinary `cargo test`.
        for seed in 1..=25_000u64 {
            assert_generated_matches_micro(seed);
        }
    }

    /// A seed has to pin one program, or a reported divergence could not be
    /// replayed and the sweep would be testing the same program 25,000 times.
    #[test]
    fn a_seed_replays_exactly() {
        assert_eq!(
            generated_program(99).functions[0].body,
            generated_program(99).functions[0].body
        );
        assert_ne!(
            generated_program(99).functions[0].body,
            generated_program(100).functions[0].body
        );
    }
}
