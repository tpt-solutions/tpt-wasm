// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Structural and type verification for the current TPT IR slice.

use std::collections::{HashMap, HashSet};
use std::fmt;

use tpt_wasm_types::ValueType;

use super::{IrModule, ValueId};
use crate::{lower_module, LoweringError};

/// Errors found while verifying a lowered IR module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    DuplicateValue(ValueId),
    MissingBlock(super::BlockId),
    UnknownFunction(u32),
    CallArity {
        function: u32,
        expected: usize,
        actual: usize,
    },
    CallResultArity {
        function: u32,
        expected: usize,
        actual: usize,
    },
    UnsupportedBlockCount {
        function: usize,
        actual: usize,
    },
    RedefinedValue(ValueId),
    UndefinedValue(ValueId),
    UseBeforeDefinition(ValueId),
    ParameterArity {
        function: usize,
        expected: usize,
        actual: usize,
    },
    ReturnArity {
        function: usize,
        expected: usize,
        actual: usize,
    },
    TypeMismatch {
        function: usize,
        value: ValueId,
        expected: ValueType,
        actual: ValueType,
    },
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for VerificationError {}

/// Proof that an IR module passed the current structural verifier.
///
/// The private marker ensures callers obtain this value only through
/// [`verify_module`] or [`lower_and_verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrVerificationCertificate {
    _private: (),
}

/// A verified module that has not yet been mutated by compiler passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIrModule {
    module: IrModule,
    certificate: IrVerificationCertificate,
}

impl VerifiedIrModule {
    pub fn module(&self) -> &IrModule {
        &self.module
    }

    pub fn certificate(&self) -> &IrVerificationCertificate {
        &self.certificate
    }

    pub fn into_module(self) -> IrModule {
        self.module
    }
}

/// Combined errors from the lowering and verification stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerAndVerifyError {
    Lowering(LoweringError),
    Verification(VerificationError),
}

impl fmt::Display for LowerAndVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LowerAndVerifyError {}

/// Lower and verify a module without exposing an unverified intermediate value.
pub fn lower_and_verify(
    validated: &tpt_wasm_validate::ValidatedModule,
) -> Result<VerifiedIrModule, LowerAndVerifyError> {
    let module = lower_module(validated).map_err(LowerAndVerifyError::Lowering)?;
    let certificate = verify_module(&module).map_err(LowerAndVerifyError::Verification)?;
    Ok(VerifiedIrModule {
        module,
        certificate,
    })
}

/// Verify value definitions, instruction types, entry points, and returns.
pub fn verify_module(module: &IrModule) -> Result<IrVerificationCertificate, VerificationError> {
    for (function_index, function) in module.functions.iter().enumerate() {
        verify_function(function, function_index, &module.functions)?;
    }
    Ok(IrVerificationCertificate { _private: () })
}

fn verify_function(
    function: &super::IrFunction,
    function_index: usize,
    functions: &[super::IrFunction],
) -> Result<(), VerificationError> {
    let values = value_types(&function.values)?;
    verify_blocks(function, function_index)?;
    verify_parameters(function, function_index, &values)?;

    let mut defined = HashSet::new();
    for id in function.params.iter().chain(&function.locals) {
        if !defined.insert(*id) {
            return Err(VerificationError::RedefinedValue(*id));
        }
    }

    for block in &function.blocks {
        for instruction in &block.instrs {
            verify_instruction(
                instruction,
                function_index,
                functions,
                &values,
                &mut defined,
            )?;
        }
        if let super::Terminator::Return(results) = &block.terminator {
            verify_results(
                results,
                &function.function_type.results.0,
                function_index,
                &values,
                &defined,
            )?;
        }
    }

    for value in &function.values {
        if !defined.contains(&value.id) {
            return Err(VerificationError::UndefinedValue(value.id));
        }
    }
    Ok(())
}

fn value_types(
    values: &[super::IrValue],
) -> Result<HashMap<ValueId, ValueType>, VerificationError> {
    let mut types = HashMap::with_capacity(values.len());
    for value in values {
        if types.insert(value.id, value.value_type).is_some() {
            return Err(VerificationError::DuplicateValue(value.id));
        }
    }
    Ok(types)
}

fn verify_blocks(
    function: &super::IrFunction,
    function_index: usize,
) -> Result<(), VerificationError> {
    if function.blocks.len() != 1 {
        return Err(VerificationError::UnsupportedBlockCount {
            function: function_index,
            actual: function.blocks.len(),
        });
    }
    if function.blocks[0].id != function.entry {
        return Err(VerificationError::MissingBlock(function.entry));
    }
    Ok(())
}

fn verify_parameters(
    function: &super::IrFunction,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
) -> Result<(), VerificationError> {
    let expected = function.function_type.params.0.len();
    if function.params.len() != expected {
        return Err(VerificationError::ParameterArity {
            function: function_index,
            expected,
            actual: function.params.len(),
        });
    }
    for (id, expected_type) in function.params.iter().zip(&function.function_type.params.0) {
        expect_type(*id, *expected_type, function_index, values)?;
    }
    for id in &function.locals {
        value_type(*id, values)?;
    }
    Ok(())
}

fn value_type(
    id: ValueId,
    values: &HashMap<ValueId, ValueType>,
) -> Result<ValueType, VerificationError> {
    values
        .get(&id)
        .copied()
        .ok_or(VerificationError::UndefinedValue(id))
}

fn expect_type(
    id: ValueId,
    expected: ValueType,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
) -> Result<(), VerificationError> {
    let actual = value_type(id, values)?;
    if actual == expected {
        Ok(())
    } else {
        Err(VerificationError::TypeMismatch {
            function: function_index,
            value: id,
            expected,
            actual,
        })
    }
}

fn verify_instruction(
    instruction: &super::IrInstr,
    function_index: usize,
    functions: &[super::IrFunction],
    values: &HashMap<ValueId, ValueType>,
    defined: &mut HashSet<ValueId>,
) -> Result<(), VerificationError> {
    match instruction {
        super::IrInstr::ConstI32 { result, .. } => {
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::ConstI64 { result, .. } => {
            define_value(*result, ValueType::I64, function_index, values, defined)
        }
        super::IrInstr::ConstF32 { result, .. } => {
            define_value(*result, ValueType::F32, function_index, values, defined)
        }
        super::IrInstr::ConstF64 { result, .. } => {
            define_value(*result, ValueType::F64, function_index, values, defined)
        }
        super::IrInstr::Drop { value } => {
            require_defined(*value, values, defined)?;
            Ok(())
        }
        super::IrInstr::Select {
            result,
            condition,
            left,
            right,
        } => {
            expect_defined_type(*condition, ValueType::I32, function_index, values, defined)?;
            let left_type = value_type(*left, values)?;
            expect_defined_type(*right, left_type, function_index, values, defined)?;
            define_value(*result, left_type, function_index, values, defined)
        }
        super::IrInstr::LocalGet { result, local } => {
            let value_type = value_type(*local, values)?;
            require_defined(*local, values, defined)?;
            define_value(*result, value_type, function_index, values, defined)
        }
        super::IrInstr::LocalSet { local, value } => {
            let value_type = value_type(*local, values)?;
            require_defined(*local, values, defined)?;
            expect_defined_type(*value, value_type, function_index, values, defined)
        }
        super::IrInstr::LocalTee {
            result,
            local,
            value,
        } => {
            let value_type = value_type(*local, values)?;
            require_defined(*local, values, defined)?;
            expect_defined_type(*value, value_type, function_index, values, defined)?;
            define_value(*result, value_type, function_index, values, defined)
        }
        super::IrInstr::I32Eqz { result, value } => {
            expect_defined_type(*value, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I32Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::I32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Eqz { result, value } => {
            expect_defined_type(*value, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::I64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I32Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I64, function_index, values, defined)
        }
        super::IrInstr::I32Add {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::I32DivS {
            result,
            left,
            right,
        }
        | super::IrInstr::I32DivU {
            result,
            left,
            right,
        }
        | super::IrInstr::I32RemS {
            result,
            left,
            right,
        }
        | super::IrInstr::I32RemU {
            result,
            left,
            right,
        }
        | super::IrInstr::I32And {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Or {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Xor {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Shl {
            result,
            left,
            right,
        }
        | super::IrInstr::I32ShrS {
            result,
            left,
            right,
        }
        | super::IrInstr::I32ShrU {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Rotl {
            result,
            left,
            right,
        }
        | super::IrInstr::I32Rotr {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::I32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::I64Add {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::I64DivS {
            result,
            left,
            right,
        }
        | super::IrInstr::I64DivU {
            result,
            left,
            right,
        }
        | super::IrInstr::I64RemS {
            result,
            left,
            right,
        }
        | super::IrInstr::I64RemU {
            result,
            left,
            right,
        }
        | super::IrInstr::I64And {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Or {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Xor {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Shl {
            result,
            left,
            right,
        }
        | super::IrInstr::I64ShrS {
            result,
            left,
            right,
        }
        | super::IrInstr::I64ShrU {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Rotl {
            result,
            left,
            right,
        }
        | super::IrInstr::I64Rotr {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::I64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::I64, function_index, values, defined)?;
            define_value(*result, ValueType::I64, function_index, values, defined)
        }
        super::IrInstr::F32Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::F32, function_index, values, defined)?;
            define_value(*result, ValueType::F32, function_index, values, defined)
        }
        super::IrInstr::F64Unary { result, value, .. } => {
            expect_defined_type(*value, ValueType::F64, function_index, values, defined)?;
            define_value(*result, ValueType::F64, function_index, values, defined)
        }
        super::IrInstr::IntConvert {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::Reinterpret {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::FloatConvert {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::FloatTrunc {
            result,
            value,
            operation,
        } => {
            expect_defined_type(
                *value,
                operation.source_type(),
                function_index,
                values,
                defined,
            )?;
            define_value(
                *result,
                operation.result_type(),
                function_index,
                values,
                defined,
            )
        }
        super::IrInstr::F32Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::F32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F32, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::F64Compare {
            result,
            left,
            right,
            ..
        } => {
            expect_defined_type(*left, ValueType::F64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F64, function_index, values, defined)?;
            define_value(*result, ValueType::I32, function_index, values, defined)
        }
        super::IrInstr::F32Add {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Div {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Min {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Max {
            result,
            left,
            right,
        }
        | super::IrInstr::F32Copysign {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::F32, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F32, function_index, values, defined)?;
            define_value(*result, ValueType::F32, function_index, values, defined)
        }
        super::IrInstr::F64Add {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Sub {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Mul {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Div {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Min {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Max {
            result,
            left,
            right,
        }
        | super::IrInstr::F64Copysign {
            result,
            left,
            right,
        } => {
            expect_defined_type(*left, ValueType::F64, function_index, values, defined)?;
            expect_defined_type(*right, ValueType::F64, function_index, values, defined)?;
            define_value(*result, ValueType::F64, function_index, values, defined)
        }
        super::IrInstr::Call {
            function,
            arguments,
            results,
        } => {
            let callee_index = usize::try_from(*function)
                .map_err(|_| VerificationError::UnknownFunction(*function))?;
            let callee = functions
                .get(callee_index)
                .ok_or(VerificationError::UnknownFunction(*function))?;
            if arguments.len() != callee.function_type.params.0.len() {
                return Err(VerificationError::CallArity {
                    function: *function,
                    expected: callee.function_type.params.0.len(),
                    actual: arguments.len(),
                });
            }
            for (argument, expected) in arguments.iter().zip(&callee.function_type.params.0) {
                expect_defined_type(*argument, *expected, function_index, values, defined)?;
            }
            if results.len() != callee.function_type.results.0.len() {
                return Err(VerificationError::CallResultArity {
                    function: *function,
                    expected: callee.function_type.results.0.len(),
                    actual: results.len(),
                });
            }
            for (result, expected) in results.iter().zip(&callee.function_type.results.0) {
                define_value(*result, *expected, function_index, values, defined)?;
            }
            Ok(())
        }
    }
}

fn define_value(
    id: ValueId,
    expected: ValueType,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
    defined: &mut HashSet<ValueId>,
) -> Result<(), VerificationError> {
    expect_type(id, expected, function_index, values)?;
    if !defined.insert(id) {
        return Err(VerificationError::RedefinedValue(id));
    }
    Ok(())
}

fn require_defined(
    id: ValueId,
    values: &HashMap<ValueId, ValueType>,
    defined: &HashSet<ValueId>,
) -> Result<(), VerificationError> {
    value_type(id, values)?;
    if defined.contains(&id) {
        Ok(())
    } else {
        Err(VerificationError::UseBeforeDefinition(id))
    }
}

fn expect_defined_type(
    id: ValueId,
    expected: ValueType,
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
    defined: &HashSet<ValueId>,
) -> Result<(), VerificationError> {
    require_defined(id, values, defined)?;
    expect_type(id, expected, function_index, values)
}

fn verify_results(
    results: &[ValueId],
    expected_types: &[ValueType],
    function_index: usize,
    values: &HashMap<ValueId, ValueType>,
    defined: &HashSet<ValueId>,
) -> Result<(), VerificationError> {
    if results.len() != expected_types.len() {
        return Err(VerificationError::ReturnArity {
            function: function_index,
            expected: expected_types.len(),
            actual: results.len(),
        });
    }
    for (id, expected) in results.iter().zip(expected_types) {
        expect_defined_type(*id, *expected, function_index, values, defined)?;
    }
    Ok(())
}
