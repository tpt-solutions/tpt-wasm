// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Baseline code generation.
//!
//! Priorities: correctness, startup time, predictable output, easy debugging.
//! NOT peak performance — that is the Optimizing compiler's job.
//!
//! Pipeline: Wasm → TPT IR → simple lowering → native code
//!
//! Status: M7 — portable baseline slice implemented; native backends pending.

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

    fn execute_micro(body: &[u8], arity: usize) -> Result<Vec<Value>, Trap> {
        let instructions = decode_body(body).unwrap();
        let mut machine = Machine::new();
        machine
            .push_frame(Frame::new(0, 0, Vec::new(), instructions, arity))
            .unwrap();
        match machine.run() {
            Step::Return(values) => Ok(values),
            Step::Trap(trap) => Err(trap),
            other => panic!("unexpected Micro result: {other:?}"),
        }
    }

    /// Lower one straight-line Wasm body through the real
    /// validate → IR → baseline pipeline.
    fn lower_body(body: &[u8], result: ValueType) -> super::BaselineFunction {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![result]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
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
        let baseline = lower_body(body, result);
        let baseline_result = baseline.execute(Vec::new());
        assert_eq!(
            baseline_result,
            execute_micro(body, 1),
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
        // Local access is modeled in the IR but not yet lowered by the baseline
        // backend, so it must be rejected rather than silently ignored.
        let mut unsupported = add_function();
        unsupported.blocks[0].instrs[0] = IrInstr::LocalGet {
            result: ValueId(0),
            local: ValueId(0),
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
