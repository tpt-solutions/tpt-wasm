// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Executable projection from certified TPT IR into the formal model.

use std::fmt;

use tpt_wasm_ir::{IrInstr, Terminator, ValueId, VerifiedIrModule};
use tpt_wasm_semantics::{
    Comparison, FloatComparison, FloatConversion, FloatTrunc, FloatUnaryOperation, FunctionState,
    Instruction, IntegerConversion, ReinterpretOperation, UnaryOperation,
};
use tpt_wasm_types::ValueType;

/// Operations that cannot yet be represented by the formal model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrProjectionError {
    UnknownFunction(usize),
    MissingValue(ValueId),
    UnsupportedInstruction(&'static str),
    UnsupportedTerminator(&'static str),
}

impl fmt::Display for IrProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for IrProjectionError {}

/// Project one certified IR function into the executable formal model.
///
/// This is a V4 correspondence hook, not a proof. It rejects any operation not
/// represented by `tpt-wasm-semantics` rather than silently changing behavior.
pub fn project_function_to_model(
    verified: &VerifiedIrModule,
    function_index: usize,
) -> Result<FunctionState, IrProjectionError> {
    let function = verified
        .module()
        .functions
        .get(function_index)
        .ok_or(IrProjectionError::UnknownFunction(function_index))?;
    let block = function
        .blocks
        .first()
        .expect("certificate-backed IR always has an entry block");
    let mut body = Vec::with_capacity(block.instrs.len() + 1);

    for instruction in &block.instrs {
        body.push(match instruction {
            IrInstr::ConstI32 { value, .. } => Instruction::I32Const(*value),
            IrInstr::ConstI64 { value, .. } => Instruction::I64Const(*value),
            IrInstr::ConstF32 { value, .. } => Instruction::F32Const(*value),
            IrInstr::ConstF64 { value, .. } => Instruction::F64Const(*value),
            IrInstr::Drop { .. } => Instruction::Drop,
            IrInstr::LocalGet { local, .. } => {
                Instruction::LocalGet(local_index(function, *local)?)
            }
            IrInstr::LocalSet { local, .. } => {
                Instruction::LocalSet(local_index(function, *local)?)
            }
            IrInstr::LocalTee { local, .. } => {
                Instruction::LocalTee(local_index(function, *local)?)
            }
            IrInstr::Select { left, .. } => match value_type(function, *left)? {
                ValueType::I32 => Instruction::SelectI32,
                ValueType::I64 => Instruction::SelectI64,
                ValueType::F32 => Instruction::SelectF32,
                ValueType::F64 => Instruction::SelectF64,
                _ => return Err(IrProjectionError::UnsupportedInstruction("select type")),
            },
            IrInstr::I32Add { .. } => Instruction::AddI32,
            IrInstr::I32Sub { .. } => Instruction::SubI32,
            IrInstr::I32Mul { .. } => Instruction::MulI32,
            IrInstr::I32DivS { .. } => Instruction::DivSI32,
            IrInstr::I32DivU { .. } => Instruction::DivUI32,
            IrInstr::I32RemS { .. } => Instruction::RemSI32,
            IrInstr::I32RemU { .. } => Instruction::RemUI32,
            IrInstr::I32And { .. } => Instruction::AndI32,
            IrInstr::I32Or { .. } => Instruction::OrI32,
            IrInstr::I32Xor { .. } => Instruction::XorI32,
            IrInstr::I32Shl { .. } => Instruction::ShlI32,
            IrInstr::I32ShrS { .. } => Instruction::ShrSI32,
            IrInstr::I32ShrU { .. } => Instruction::ShrUI32,
            IrInstr::I32Rotl { .. } => Instruction::RotlI32,
            IrInstr::I32Rotr { .. } => Instruction::RotrI32,
            IrInstr::I32Eqz { .. } => Instruction::EqzI32,
            IrInstr::I32Unary { operation, .. } => Instruction::UnaryI32(match operation {
                tpt_wasm_ir::IntUnary::Clz => UnaryOperation::Clz,
                tpt_wasm_ir::IntUnary::Ctz => UnaryOperation::Ctz,
                tpt_wasm_ir::IntUnary::Popcnt => UnaryOperation::Popcnt,
            }),
            IrInstr::I32Compare { comparison, .. } => Instruction::CompareI32(match comparison {
                tpt_wasm_ir::IntComparison::Eq => Comparison::Eq,
                tpt_wasm_ir::IntComparison::Ne => Comparison::Ne,
                tpt_wasm_ir::IntComparison::LtS => Comparison::LtS,
                tpt_wasm_ir::IntComparison::LtU => Comparison::LtU,
                tpt_wasm_ir::IntComparison::GtS => Comparison::GtS,
                tpt_wasm_ir::IntComparison::GtU => Comparison::GtU,
                tpt_wasm_ir::IntComparison::LeS => Comparison::LeS,
                tpt_wasm_ir::IntComparison::LeU => Comparison::LeU,
                tpt_wasm_ir::IntComparison::GeS => Comparison::GeS,
                tpt_wasm_ir::IntComparison::GeU => Comparison::GeU,
            }),
            IrInstr::F32Unary { operation, .. } => Instruction::UnaryF32(match operation {
                tpt_wasm_ir::FloatUnary::Abs => FloatUnaryOperation::Abs,
                tpt_wasm_ir::FloatUnary::Neg => FloatUnaryOperation::Neg,
                tpt_wasm_ir::FloatUnary::Ceil => FloatUnaryOperation::Ceil,
                tpt_wasm_ir::FloatUnary::Floor => FloatUnaryOperation::Floor,
                tpt_wasm_ir::FloatUnary::Trunc => FloatUnaryOperation::Trunc,
                tpt_wasm_ir::FloatUnary::Nearest => FloatUnaryOperation::Nearest,
                tpt_wasm_ir::FloatUnary::Sqrt => FloatUnaryOperation::Sqrt,
            }),
            IrInstr::F64Unary { operation, .. } => Instruction::UnaryF64(match operation {
                tpt_wasm_ir::FloatUnary::Abs => FloatUnaryOperation::Abs,
                tpt_wasm_ir::FloatUnary::Neg => FloatUnaryOperation::Neg,
                tpt_wasm_ir::FloatUnary::Ceil => FloatUnaryOperation::Ceil,
                tpt_wasm_ir::FloatUnary::Floor => FloatUnaryOperation::Floor,
                tpt_wasm_ir::FloatUnary::Trunc => FloatUnaryOperation::Trunc,
                tpt_wasm_ir::FloatUnary::Nearest => FloatUnaryOperation::Nearest,
                tpt_wasm_ir::FloatUnary::Sqrt => FloatUnaryOperation::Sqrt,
            }),
            IrInstr::F32Compare { comparison, .. } => Instruction::CompareF32(match comparison {
                tpt_wasm_ir::FloatComparison::Eq => FloatComparison::Eq,
                tpt_wasm_ir::FloatComparison::Ne => FloatComparison::Ne,
                tpt_wasm_ir::FloatComparison::Lt => FloatComparison::Lt,
                tpt_wasm_ir::FloatComparison::Gt => FloatComparison::Gt,
                tpt_wasm_ir::FloatComparison::Le => FloatComparison::Le,
                tpt_wasm_ir::FloatComparison::Ge => FloatComparison::Ge,
            }),
            IrInstr::F64Compare { comparison, .. } => Instruction::CompareF64(match comparison {
                tpt_wasm_ir::FloatComparison::Eq => FloatComparison::Eq,
                tpt_wasm_ir::FloatComparison::Ne => FloatComparison::Ne,
                tpt_wasm_ir::FloatComparison::Lt => FloatComparison::Lt,
                tpt_wasm_ir::FloatComparison::Gt => FloatComparison::Gt,
                tpt_wasm_ir::FloatComparison::Le => FloatComparison::Le,
                tpt_wasm_ir::FloatComparison::Ge => FloatComparison::Ge,
            }),
            IrInstr::F32Add { .. } => Instruction::AddF32,
            IrInstr::F32Sub { .. } => Instruction::SubF32,
            IrInstr::F32Mul { .. } => Instruction::MulF32,
            IrInstr::F32Div { .. } => Instruction::DivF32,
            IrInstr::F32Min { .. } => Instruction::MinF32,
            IrInstr::F32Max { .. } => Instruction::MaxF32,
            IrInstr::F32Copysign { .. } => Instruction::CopysignF32,
            IrInstr::F64Add { .. } => Instruction::AddF64,
            IrInstr::F64Sub { .. } => Instruction::SubF64,
            IrInstr::F64Mul { .. } => Instruction::MulF64,
            IrInstr::F64Div { .. } => Instruction::DivF64,
            IrInstr::F64Min { .. } => Instruction::MinF64,
            IrInstr::F64Max { .. } => Instruction::MaxF64,
            IrInstr::F64Copysign { .. } => Instruction::CopysignF64,
            IrInstr::I64Unary { operation, .. } => Instruction::UnaryI64(match operation {
                tpt_wasm_ir::IntUnary::Clz => UnaryOperation::Clz,
                tpt_wasm_ir::IntUnary::Ctz => UnaryOperation::Ctz,
                tpt_wasm_ir::IntUnary::Popcnt => UnaryOperation::Popcnt,
            }),
            IrInstr::I64Eqz { .. } => Instruction::EqzI64,
            IrInstr::I64Compare { comparison, .. } => Instruction::CompareI64(match comparison {
                tpt_wasm_ir::IntComparison::Eq => Comparison::Eq,
                tpt_wasm_ir::IntComparison::Ne => Comparison::Ne,
                tpt_wasm_ir::IntComparison::LtS => Comparison::LtS,
                tpt_wasm_ir::IntComparison::LtU => Comparison::LtU,
                tpt_wasm_ir::IntComparison::GtS => Comparison::GtS,
                tpt_wasm_ir::IntComparison::GtU => Comparison::GtU,
                tpt_wasm_ir::IntComparison::LeS => Comparison::LeS,
                tpt_wasm_ir::IntComparison::LeU => Comparison::LeU,
                tpt_wasm_ir::IntComparison::GeS => Comparison::GeS,
                tpt_wasm_ir::IntComparison::GeU => Comparison::GeU,
            }),
            IrInstr::I64Add { .. } => Instruction::AddI64,
            IrInstr::I64Sub { .. } => Instruction::SubI64,
            IrInstr::I64Mul { .. } => Instruction::MulI64,
            IrInstr::I64DivS { .. } => Instruction::DivSI64,
            IrInstr::I64DivU { .. } => Instruction::DivUI64,
            IrInstr::I64RemS { .. } => Instruction::RemSI64,
            IrInstr::I64RemU { .. } => Instruction::RemUI64,
            IrInstr::I64And { .. } => Instruction::AndI64,
            IrInstr::I64Or { .. } => Instruction::OrI64,
            IrInstr::I64Xor { .. } => Instruction::XorI64,
            IrInstr::I64Shl { .. } => Instruction::ShlI64,
            IrInstr::I64ShrS { .. } => Instruction::ShrSI64,
            IrInstr::I64ShrU { .. } => Instruction::ShrUI64,
            IrInstr::I64Rotl { .. } => Instruction::RotlI64,
            IrInstr::I64Rotr { .. } => Instruction::RotrI64,
            IrInstr::IntConvert { operation, .. } => Instruction::IntConvert(match operation {
                tpt_wasm_ir::IntConversion::I32WrapI64 => IntegerConversion::I32WrapI64,
                tpt_wasm_ir::IntConversion::I64ExtendI32S => IntegerConversion::I64ExtendI32S,
                tpt_wasm_ir::IntConversion::I64ExtendI32U => IntegerConversion::I64ExtendI32U,
            }),
            IrInstr::Reinterpret { operation, .. } => Instruction::Reinterpret(match operation {
                tpt_wasm_ir::Reinterpret::I32FromF32 => ReinterpretOperation::I32FromF32,
                tpt_wasm_ir::Reinterpret::I64FromF64 => ReinterpretOperation::I64FromF64,
                tpt_wasm_ir::Reinterpret::F32FromI32 => ReinterpretOperation::F32FromI32,
                tpt_wasm_ir::Reinterpret::F64FromI64 => ReinterpretOperation::F64FromI64,
            }),
            IrInstr::FloatTrunc { operation, .. } => Instruction::FloatTrunc(match operation {
                tpt_wasm_ir::FloatTrunc::I32FromF32S => FloatTrunc::I32FromF32S,
                tpt_wasm_ir::FloatTrunc::I32FromF32U => FloatTrunc::I32FromF32U,
                tpt_wasm_ir::FloatTrunc::I32FromF64S => FloatTrunc::I32FromF64S,
                tpt_wasm_ir::FloatTrunc::I32FromF64U => FloatTrunc::I32FromF64U,
                tpt_wasm_ir::FloatTrunc::I64FromF32S => FloatTrunc::I64FromF32S,
                tpt_wasm_ir::FloatTrunc::I64FromF32U => FloatTrunc::I64FromF32U,
                tpt_wasm_ir::FloatTrunc::I64FromF64S => FloatTrunc::I64FromF64S,
                tpt_wasm_ir::FloatTrunc::I64FromF64U => FloatTrunc::I64FromF64U,
            }),
            IrInstr::FloatConvert { operation, .. } => Instruction::FloatConvert(match operation {
                tpt_wasm_ir::FloatConversion::F32FromI32S => FloatConversion::F32FromI32S,
                tpt_wasm_ir::FloatConversion::F32FromI32U => FloatConversion::F32FromI32U,
                tpt_wasm_ir::FloatConversion::F32FromI64S => FloatConversion::F32FromI64S,
                tpt_wasm_ir::FloatConversion::F32FromI64U => FloatConversion::F32FromI64U,
                tpt_wasm_ir::FloatConversion::F32FromF64 => FloatConversion::F32FromF64,
                tpt_wasm_ir::FloatConversion::F64FromI32S => FloatConversion::F64FromI32S,
                tpt_wasm_ir::FloatConversion::F64FromI32U => FloatConversion::F64FromI32U,
                tpt_wasm_ir::FloatConversion::F64FromI64S => FloatConversion::F64FromI64S,
                tpt_wasm_ir::FloatConversion::F64FromI64U => FloatConversion::F64FromI64U,
                tpt_wasm_ir::FloatConversion::F64FromF32 => FloatConversion::F64FromF32,
            }),
            IrInstr::Call { function, .. } => Instruction::Call(*function),
        });
    }

    body.push(match &block.terminator {
        Terminator::Return(_) => Instruction::End,
        Terminator::Unreachable => Instruction::Unreachable,
        Terminator::Trap(_) => return Err(IrProjectionError::UnsupportedTerminator("trap")),
    });

    let local_types = function
        .locals
        .iter()
        .map(|id| value_type(function, *id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(FunctionState {
        function_type: function.function_type.clone(),
        local_types,
        body,
    })
}

fn local_index(function: &tpt_wasm_ir::IrFunction, id: ValueId) -> Result<u32, IrProjectionError> {
    if let Some(index) = function.params.iter().position(|param| *param == id) {
        return u32::try_from(index).map_err(|_| IrProjectionError::MissingValue(id));
    }
    let offset = function.function_type.params.0.len();
    function
        .locals
        .iter()
        .position(|local| *local == id)
        .and_then(|index| u32::try_from(index + offset).ok())
        .ok_or(IrProjectionError::MissingValue(id))
}

fn value_type(
    function: &tpt_wasm_ir::IrFunction,
    id: ValueId,
) -> Result<ValueType, IrProjectionError> {
    function
        .values
        .iter()
        .find(|value| value.id == id)
        .map(|value| value.value_type)
        .ok_or(IrProjectionError::MissingValue(id))
}
