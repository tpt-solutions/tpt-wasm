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
//! set represented by the IR; native backends pending.

use std::collections::HashMap;
use std::fmt;

use tpt_wasm_ir::{IrFunction, IrInstr, Terminator, ValueId};
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
    DuplicateValue(ValueId),
    UnknownValue(ValueId),
    TypeMismatch {
        value: ValueId,
        expected: ValueType,
        actual: ValueType,
    },
    UnsupportedInstruction(&'static str),
    UnsupportedTerminator(&'static str),
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
}

/// A certified single-block IR function lowered to portable baseline code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineFunction {
    pub function_type: FunctionType,
    pub params: Vec<u32>,
    pub locals: Vec<u32>,
    pub value_types: Vec<ValueType>,
    pub ops: Vec<BaselineOp>,
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

/// Lower one certified single-block IR function to portable baseline code.
pub fn lower_function(function: &IrFunction) -> Result<BaselineFunction, CodegenError> {
    if function.blocks.len() != 1 {
        return Err(CodegenError::UnsupportedBlockCount {
            actual: function.blocks.len(),
        });
    }
    let block = &function.blocks[0];
    if block.id != function.entry {
        return Err(CodegenError::MissingEntryBlock);
    }
    let mut state = SlotState::new(function)?;
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
            _ => return Err(CodegenError::UnsupportedInstruction("instruction")),
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
        // The portable baseline executor is still single-block; control flow
        // is rejected rather than approximated, per the lowering contract.
        Terminator::Branch { .. } | Terminator::CondBranch { .. } => {
            return Err(CodegenError::UnsupportedTerminator("control flow"))
        }
    };
    ops.push(terminator);
    Ok(BaselineFunction {
        function_type: function.function_type.clone(),
        params,
        locals,
        value_types: state.types,
        ops,
    })
}

impl BaselineFunction {
    /// Execute this portable baseline function without invoking Micro.
    pub fn execute(&self, args: Vec<Value>) -> Result<Vec<Value>, Trap> {
        if args.len() != self.params.len() {
            return Err(Trap::HostFailure("baseline argument arity mismatch".into()));
        }
        let mut slots = vec![None; self.value_types.len()];
        for (slot, value) in self.params.iter().copied().zip(args) {
            check_value_type(&value, self.value_types[slot as usize])?;
            slots[slot as usize] = Some(value);
        }
        for slot in &self.locals {
            slots[*slot as usize] = Some(default_value(self.value_types[*slot as usize]));
        }
        for op in &self.ops {
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
                    slots[*result as usize] = Some(Value::F32(eval_f32_unary(*operation, value)));
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
                    slots[*result as usize] = Some(Value::F64(eval_f64_unary(*operation, value)));
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
                    slots[*result as usize] = Some(Value::I32(eval_i32_unary(*operation, value)));
                }
                BaselineOp::I64Unary {
                    result,
                    value,
                    operation,
                } => {
                    let value = i64_slot(&slots, *value)?;
                    slots[*result as usize] = Some(Value::I64(eval_i64_unary(*operation, value)));
                }
                BaselineOp::IntConvert {
                    result,
                    value,
                    operation,
                } => {
                    slots[*result as usize] = Some(eval_int_convert(&slots, *operation, *value)?);
                }
                BaselineOp::Reinterpret {
                    result,
                    value,
                    operation,
                } => {
                    slots[*result as usize] = Some(eval_reinterpret(&slots, *operation, *value)?);
                }
                BaselineOp::FloatConvert {
                    result,
                    value,
                    operation,
                } => {
                    slots[*result as usize] = Some(eval_float_convert(&slots, *operation, *value)?);
                }
                BaselineOp::FloatTrunc {
                    result,
                    value,
                    operation,
                } => {
                    slots[*result as usize] = Some(eval_float_trunc(&slots, *operation, *value)?);
                }
                BaselineOp::Return(results) => {
                    return results
                        .iter()
                        .map(|slot| {
                            slots
                                .get(*slot as usize)
                                .and_then(Clone::clone)
                                .ok_or_else(|| {
                                    Trap::HostFailure("baseline return slot is empty".into())
                                })
                        })
                        .collect();
                }
                BaselineOp::Trap(trap) => return Err(trap.clone()),
                BaselineOp::Unreachable => return Err(Trap::Unreachable),
            }
        }
        Err(Trap::HostFailure(
            "baseline function has no terminator".into(),
        ))
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
    use super::{lower_function, BaselineOp, CodegenError, I32BinaryOp};
    use tpt_wasm_format::{Function, Module};
    use tpt_wasm_ir::{
        lower_and_verify, BasicBlock, BlockId, IrFunction, IrInstr, IrValue, Terminator, ValueId,
    };
    use tpt_wasm_micro::instr::decode_body;
    use tpt_wasm_micro::machine::{Frame, Machine, Step};
    use tpt_wasm_types::{FunctionType, ResultType, Trap, Value, ValueType};

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
        assert_eq!(first.ops[0], BaselineOp::ConstI32 { slot: 0, value: 20 });
        assert_eq!(
            first.ops[2],
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
        assert_eq!(
            baseline_result,
            execute_micro_with_locals(body, 1, locals),
            "baseline and Micro diverged for {body:02x?}"
        );
        baseline_result
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
    fn baseline_rejects_unsupported_ops_and_preserves_traps() {
        // Direct calls are modeled in the IR but not yet lowered by the baseline
        // backend, so they must be rejected rather than silently ignored.
        let mut unsupported = add_function();
        unsupported.blocks[0].instrs[0] = IrInstr::Call {
            function: 0,
            arguments: Vec::new(),
            results: vec![ValueId(0)],
        };
        assert_eq!(
            lower_function(&unsupported),
            Err(CodegenError::UnsupportedInstruction("instruction"))
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
            Err(Trap::IntegerDivisionByZero)
        );
        assert_eq!(
            baseline.ops,
            vec![BaselineOp::Trap(Trap::IntegerDivisionByZero)]
        );
    }
}
