// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Lowering, verification, and differential tests for the current IR slice.

use std::collections::HashMap;

use tpt_wasm_format::{Export, ExportDesc, Function, LocalDecl, Module};
use tpt_wasm_micro::instr::decode_body;
use tpt_wasm_micro::machine::{Frame, Machine, Step};
use tpt_wasm_micro::store::{Instance as StoreInstance, Store};
use tpt_wasm_types::{FunctionType, ResultType, Trap, Value, ValueType};
use tpt_wasm_validate::{ValidatedModule, Validator};

use super::{
    lower_and_verify, lower_module, verify_module, BlockId, FloatComparison, FloatConversion,
    FloatTrunc, IntComparison, IntConversion, IntUnary, IrInstr, IrModule, LoweringError,
    Reinterpret, Terminator, ValueId, VerificationError,
};

fn function_type(params: Vec<ValueType>, results: Vec<ValueType>) -> FunctionType {
    FunctionType {
        params: ResultType(params),
        results: ResultType(results),
    }
}

fn module(params: Vec<ValueType>, results: Vec<ValueType>, body: Vec<u8>) -> Module {
    module_with_locals(params, results, Vec::new(), body)
}

fn module_with_locals(
    params: Vec<ValueType>,
    results: Vec<ValueType>,
    locals: Vec<LocalDecl>,
    body: Vec<u8>,
) -> Module {
    Module {
        types: vec![function_type(params, results)],
        functions: vec![Function {
            type_index: 0,
            locals,
            body,
        }],
        ..Module::default()
    }
}

fn validated(module: Module) -> ValidatedModule {
    Validator::new()
        .validate(module)
        .expect("test module should validate")
}

fn encode_i32(value: i32) -> Vec<u8> {
    encode_signed_leb(value as i64, 32)
}

fn encode_i64(value: i64) -> Vec<u8> {
    encode_signed_leb(value, 64)
}

fn encode_signed_leb(mut value: i64, bits: u32) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut shift = 0;
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let sign_bit_set = byte & 0x40 != 0;
        let final_byte = shift + 7 >= bits;
        let done = final_byte || (value == 0 && !sign_bit_set) || (value == -1 && sign_bit_set);
        if done {
            encoded.push(byte);
            return encoded;
        }
        encoded.push(byte | 0x80);
        shift += 7;
    }
}

fn execute_ir_function(
    module: &IrModule,
    function_index: usize,
    args: Vec<Value>,
    depth: usize,
) -> Result<Vec<Value>, Trap> {
    if depth >= 1000 {
        return Err(Trap::CallDepthExceeded);
    }
    let function = module
        .functions
        .get(function_index)
        .expect("verified IR has the called function");
    assert_eq!(function.params.len(), args.len());
    let mut values = HashMap::new();
    let mut locals = HashMap::new();
    for (id, value) in function.params.iter().copied().zip(args) {
        values.insert(id, value.clone());
        locals.insert(id, value);
    }
    for id in &function.locals {
        let value_type = function
            .values
            .iter()
            .find(|value| value.id == *id)
            .expect("verified IR has local type")
            .value_type;
        let value = default_value(value_type);
        values.insert(*id, value.clone());
        locals.insert(*id, value);
    }
    let block = function
        .blocks
        .iter()
        .find(|block| block.id == function.entry)
        .expect("verified IR has an entry block");

    for instruction in &block.instrs {
        match instruction {
            IrInstr::ConstI32 { result, value } => {
                values.insert(*result, Value::I32(*value));
            }
            IrInstr::ConstI64 { result, value } => {
                values.insert(*result, Value::I64(*value));
            }
            IrInstr::ConstF32 { result, value } => {
                values.insert(*result, Value::F32(*value));
            }
            IrInstr::ConstF64 { result, value } => {
                values.insert(*result, Value::F64(*value));
            }
            IrInstr::Drop { .. } => {}
            IrInstr::Select {
                result,
                condition,
                left,
                right,
            } => {
                let condition = i32_value(values.get(condition))?;
                let selected = if condition != 0 {
                    values
                        .get(left)
                        .cloned()
                        .ok_or_else(|| Trap::HostFailure("missing select left".into()))?
                } else {
                    values
                        .get(right)
                        .cloned()
                        .ok_or_else(|| Trap::HostFailure("missing select right".into()))?
                };
                values.insert(*result, selected);
            }
            IrInstr::LocalGet { result, local } => {
                let value = locals
                    .get(local)
                    .cloned()
                    .ok_or_else(|| Trap::HostFailure(format!("missing local {local:?}")))?;
                values.insert(*result, value);
            }
            IrInstr::LocalSet { local, value } => {
                let value = values
                    .get(value)
                    .cloned()
                    .ok_or_else(|| Trap::HostFailure(format!("missing IR value {value:?}")))?;
                locals.insert(*local, value);
            }
            IrInstr::LocalTee {
                result,
                local,
                value,
            } => {
                let value = values
                    .get(value)
                    .cloned()
                    .ok_or_else(|| Trap::HostFailure(format!("missing IR value {value:?}")))?;
                locals.insert(*local, value.clone());
                values.insert(*result, value);
            }
            IrInstr::I32Eqz { result, value } => {
                let value = i32_value(values.get(value))?;
                values.insert(*result, Value::I32((value == 0) as i32));
            }
            IrInstr::I32Compare {
                result,
                left,
                right,
                comparison,
            } => {
                let left = i32_value(values.get(left))?;
                let right = i32_value(values.get(right))?;
                values.insert(*result, Value::I32(compare_i32(left, right, *comparison)));
            }
            IrInstr::I64Eqz { result, value } => {
                let value = i64_value(values.get(value))?;
                values.insert(*result, Value::I32((value == 0) as i32));
            }
            IrInstr::I64Compare {
                result,
                left,
                right,
                comparison,
            } => {
                let left = i64_value(values.get(left))?;
                let right = i64_value(values.get(right))?;
                values.insert(*result, Value::I32(compare_i64(left, right, *comparison)));
            }
            IrInstr::I32Unary {
                result,
                value,
                operation,
            } => {
                let value = i32_value(values.get(value))?;
                values.insert(*result, Value::I32(unary_i32(value, *operation)));
            }
            IrInstr::I64Unary {
                result,
                value,
                operation,
            } => {
                let value = i64_value(values.get(value))?;
                values.insert(*result, Value::I64(unary_i64(value, *operation)));
            }
            IrInstr::IntConvert {
                result,
                value,
                operation,
            } => {
                let converted = match operation {
                    IntConversion::I32WrapI64 => Value::I32(i64_value(values.get(value))? as i32),
                    IntConversion::I64ExtendI32S => {
                        Value::I64(i64::from(i32_value(values.get(value))?))
                    }
                    IntConversion::I64ExtendI32U => {
                        Value::I64(i64::from(i32_value(values.get(value))? as u32))
                    }
                };
                values.insert(*result, converted);
            }
            IrInstr::Reinterpret {
                result,
                value,
                operation,
            } => {
                let reinterpreted = match operation {
                    Reinterpret::I32FromF32 => {
                        let bits = f32_value(values.get(value))?.to_bits();
                        Value::I32(bits as i32)
                    }
                    Reinterpret::I64FromF64 => {
                        let bits = f64_value(values.get(value))?.to_bits();
                        Value::I64(bits as i64)
                    }
                    Reinterpret::F32FromI32 => Value::F32(i32_value(values.get(value))? as u32),
                    Reinterpret::F64FromI64 => Value::F64(i64_value(values.get(value))? as u64),
                };
                values.insert(*result, reinterpreted);
            }
            IrInstr::FloatConvert {
                result,
                value,
                operation,
            } => {
                let converted = match operation {
                    FloatConversion::F32FromI32S => {
                        Value::F32(f32_result(i32_value(values.get(value))? as f32))
                    }
                    FloatConversion::F32FromI32U => {
                        Value::F32(f32_result(i32_value(values.get(value))? as u32 as f32))
                    }
                    FloatConversion::F32FromI64S => {
                        Value::F32(f32_result(i64_value(values.get(value))? as f32))
                    }
                    FloatConversion::F32FromI64U => {
                        Value::F32(f32_result(i64_value(values.get(value))? as u64 as f32))
                    }
                    FloatConversion::F32FromF64 => {
                        Value::F32(f32_result(f64_value(values.get(value))? as f32))
                    }
                    FloatConversion::F64FromI32S => {
                        Value::F64(f64_result(i32_value(values.get(value))? as f64))
                    }
                    FloatConversion::F64FromI32U => {
                        Value::F64(f64_result(i32_value(values.get(value))? as u32 as f64))
                    }
                    FloatConversion::F64FromI64S => {
                        Value::F64(f64_result(i64_value(values.get(value))? as f64))
                    }
                    FloatConversion::F64FromI64U => {
                        Value::F64(f64_result(i64_value(values.get(value))? as u64 as f64))
                    }
                    FloatConversion::F64FromF32 => {
                        Value::F64(f64_result(f32_value(values.get(value))? as f64))
                    }
                };
                values.insert(*result, converted);
            }
            IrInstr::FloatTrunc {
                result,
                value,
                operation,
            } => {
                let converted = match operation {
                    FloatTrunc::I32FromF32S => {
                        trunc_f32_to_i32(f32_value(values.get(value))?, true)?
                    }
                    FloatTrunc::I32FromF32U => {
                        trunc_f32_to_i32(f32_value(values.get(value))?, false)?
                    }
                    FloatTrunc::I32FromF64S => {
                        trunc_f64_to_i32(f64_value(values.get(value))?, true)?
                    }
                    FloatTrunc::I32FromF64U => {
                        trunc_f64_to_i32(f64_value(values.get(value))?, false)?
                    }
                    FloatTrunc::I64FromF32S => {
                        trunc_f32_to_i64(f32_value(values.get(value))?, true)?
                    }
                    FloatTrunc::I64FromF32U => {
                        trunc_f32_to_i64(f32_value(values.get(value))?, false)?
                    }
                    FloatTrunc::I64FromF64S => {
                        trunc_f64_to_i64(f64_value(values.get(value))?, true)?
                    }
                    FloatTrunc::I64FromF64U => {
                        trunc_f64_to_i64(f64_value(values.get(value))?, false)?
                    }
                };
                values.insert(*result, converted);
            }
            IrInstr::Call {
                function,
                arguments,
                results,
            } => {
                let call_arguments = arguments
                    .iter()
                    .map(|argument| {
                        values
                            .get(argument)
                            .cloned()
                            .ok_or_else(|| Trap::HostFailure("missing call argument".into()))
                    })
                    .collect::<Result<Vec<_>, Trap>>()?;
                let call_results =
                    execute_ir_function(module, *function as usize, call_arguments, depth + 1)?;
                if call_results.len() != results.len() {
                    return Err(Trap::HostFailure("call result arity mismatch".into()));
                }
                for (result, value) in results.iter().zip(call_results) {
                    values.insert(*result, value);
                }
            }
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
            } => {
                let left = i32_value(values.get(left))?;
                let right = i32_value(values.get(right))?;
                let value = match instruction {
                    IrInstr::I32Add { .. } => left.wrapping_add(right),
                    IrInstr::I32Sub { .. } => left.wrapping_sub(right),
                    IrInstr::I32Mul { .. } => left.wrapping_mul(right),
                    IrInstr::I32DivS { .. } => div_i32_s(left, right)?,
                    IrInstr::I32DivU { .. } => div_i32_u(left, right)?,
                    IrInstr::I32RemS { .. } => rem_i32_s(left, right)?,
                    IrInstr::I32RemU { .. } => rem_i32_u(left, right)?,
                    IrInstr::I32And { .. } => left & right,
                    IrInstr::I32Or { .. } => left | right,
                    IrInstr::I32Xor { .. } => left ^ right,
                    IrInstr::I32Shl { .. } => left.wrapping_shl(right as u32),
                    IrInstr::I32ShrS { .. } => left.wrapping_shr(right as u32),
                    IrInstr::I32ShrU { .. } => ((left as u32).wrapping_shr(right as u32)) as i32,
                    IrInstr::I32Rotl { .. } => left.rotate_left((right as u32) & 31),
                    IrInstr::I32Rotr { .. } => left.rotate_right((right as u32) & 31),
                    _ => unreachable!(),
                };
                values.insert(*result, Value::I32(value));
            }
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
            } => {
                let left = i64_value(values.get(left))?;
                let right = i64_value(values.get(right))?;
                let value = match instruction {
                    IrInstr::I64Add { .. } => left.wrapping_add(right),
                    IrInstr::I64Sub { .. } => left.wrapping_sub(right),
                    IrInstr::I64Mul { .. } => left.wrapping_mul(right),
                    IrInstr::I64DivS { .. } => div_i64_s(left, right)?,
                    IrInstr::I64DivU { .. } => div_i64_u(left, right)?,
                    IrInstr::I64RemS { .. } => rem_i64_s(left, right)?,
                    IrInstr::I64RemU { .. } => rem_i64_u(left, right)?,
                    IrInstr::I64And { .. } => left & right,
                    IrInstr::I64Or { .. } => left | right,
                    IrInstr::I64Xor { .. } => left ^ right,
                    IrInstr::I64Shl { .. } => left.wrapping_shl(right as u32),
                    IrInstr::I64ShrS { .. } => left.wrapping_shr(right as u32),
                    IrInstr::I64ShrU { .. } => ((left as u64).wrapping_shr(right as u32)) as i64,
                    IrInstr::I64Rotl { .. } => left.rotate_left((right as u64 & 63) as u32),
                    IrInstr::I64Rotr { .. } => left.rotate_right((right as u64 & 63) as u32),
                    _ => unreachable!(),
                };
                values.insert(*result, Value::I64(value));
            }
            IrInstr::F32Unary {
                result,
                value,
                operation,
            } => {
                let value = f32_value(values.get(value))?;
                let value = match operation {
                    super::FloatUnary::Abs => {
                        if value.is_nan() {
                            f32::from_bits(value.to_bits() & 0x7fff_ffff)
                        } else {
                            value.abs()
                        }
                    }
                    super::FloatUnary::Neg => {
                        if value.is_nan() {
                            f32::from_bits(value.to_bits() ^ 0x8000_0000)
                        } else {
                            -value
                        }
                    }
                    super::FloatUnary::Ceil => value.ceil(),
                    super::FloatUnary::Floor => value.floor(),
                    super::FloatUnary::Trunc => value.trunc(),
                    super::FloatUnary::Nearest => value.round_ties_even(),
                    super::FloatUnary::Sqrt => value.sqrt(),
                };
                values.insert(*result, Value::F32(value.to_bits()));
            }
            IrInstr::F64Unary {
                result,
                value,
                operation,
            } => {
                let value = f64_value(values.get(value))?;
                let value = match operation {
                    super::FloatUnary::Abs => {
                        if value.is_nan() {
                            f64::from_bits(value.to_bits() & 0x7fff_ffff_ffff_ffff)
                        } else {
                            value.abs()
                        }
                    }
                    super::FloatUnary::Neg => {
                        if value.is_nan() {
                            f64::from_bits(value.to_bits() ^ 0x8000_0000_0000_0000)
                        } else {
                            -value
                        }
                    }
                    super::FloatUnary::Ceil => value.ceil(),
                    super::FloatUnary::Floor => value.floor(),
                    super::FloatUnary::Trunc => value.trunc(),
                    super::FloatUnary::Nearest => value.round_ties_even(),
                    super::FloatUnary::Sqrt => value.sqrt(),
                };
                values.insert(*result, Value::F64(value.to_bits()));
            }
            IrInstr::F32Compare {
                result,
                left,
                right,
                comparison,
            } => {
                let left = f32_value(values.get(left))?;
                let right = f32_value(values.get(right))?;
                values.insert(*result, Value::I32(compare_f32(left, right, *comparison)));
            }
            IrInstr::F64Compare {
                result,
                left,
                right,
                comparison,
            } => {
                let left = f64_value(values.get(left))?;
                let right = f64_value(values.get(right))?;
                values.insert(*result, Value::I32(compare_f64(left, right, *comparison)));
            }
            IrInstr::F32Add {
                result,
                left,
                right,
            }
            | IrInstr::F32Sub {
                result,
                left,
                right,
            }
            | IrInstr::F32Mul {
                result,
                left,
                right,
            }
            | IrInstr::F32Div {
                result,
                left,
                right,
            }
            | IrInstr::F32Min {
                result,
                left,
                right,
            }
            | IrInstr::F32Max {
                result,
                left,
                right,
            }
            | IrInstr::F32Copysign {
                result,
                left,
                right,
            } => {
                let left = f32_value(values.get(left))?;
                let right = f32_value(values.get(right))?;
                let value = match instruction {
                    IrInstr::F32Add { .. } => left + right,
                    IrInstr::F32Sub { .. } => left - right,
                    IrInstr::F32Mul { .. } => left * right,
                    IrInstr::F32Div { .. } => left / right,
                    IrInstr::F32Min { .. } => f32_min(left, right),
                    IrInstr::F32Max { .. } => f32_max(left, right),
                    IrInstr::F32Copysign { .. } => left.copysign(right),
                    _ => unreachable!(),
                };
                values.insert(*result, Value::F32(value.to_bits()));
            }
            IrInstr::F64Add {
                result,
                left,
                right,
            }
            | IrInstr::F64Sub {
                result,
                left,
                right,
            }
            | IrInstr::F64Mul {
                result,
                left,
                right,
            }
            | IrInstr::F64Div {
                result,
                left,
                right,
            }
            | IrInstr::F64Min {
                result,
                left,
                right,
            }
            | IrInstr::F64Max {
                result,
                left,
                right,
            }
            | IrInstr::F64Copysign {
                result,
                left,
                right,
            } => {
                let left = f64_value(values.get(left))?;
                let right = f64_value(values.get(right))?;
                let value = match instruction {
                    IrInstr::F64Add { .. } => left + right,
                    IrInstr::F64Sub { .. } => left - right,
                    IrInstr::F64Mul { .. } => left * right,
                    IrInstr::F64Div { .. } => left / right,
                    IrInstr::F64Min { .. } => f64_min(left, right),
                    IrInstr::F64Max { .. } => f64_max(left, right),
                    IrInstr::F64Copysign { .. } => left.copysign(right),
                    _ => unreachable!(),
                };
                values.insert(*result, Value::F64(value.to_bits()));
            }
        }
    }

    match &block.terminator {
        Terminator::Return(results) => results
            .iter()
            .map(|id| {
                values
                    .get(id)
                    .cloned()
                    .ok_or_else(|| Trap::HostFailure(format!("missing IR value {id:?}")))
            })
            .collect(),
        Terminator::Trap(trap) => Err(trap.clone()),
        Terminator::Unreachable => Err(Trap::Unreachable),
    }
}

const F32_CANONICAL_NAN: u32 = 0x7fc0_0000;
const F64_CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

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

fn f32_value(value: Option<&Value>) -> Result<f32, Trap> {
    match value {
        Some(Value::F32(value)) => Ok(f32::from_bits(*value)),
        _ => Err(Trap::HostFailure("expected IR f32 value".into())),
    }
}

fn div_i32_s(left: i32, right: i32) -> Result<i32, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    if left == i32::MIN && right == -1 {
        return Err(Trap::IntegerOverflow);
    }
    Ok(left / right)
}

fn div_i32_u(left: i32, right: i32) -> Result<i32, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(((left as u32) / (right as u32)) as i32)
}

fn rem_i32_s(left: i32, right: i32) -> Result<i32, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(left.wrapping_rem(right))
}

fn rem_i32_u(left: i32, right: i32) -> Result<i32, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(((left as u32) % (right as u32)) as i32)
}

fn div_i64_s(left: i64, right: i64) -> Result<i64, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    if left == i64::MIN && right == -1 {
        return Err(Trap::IntegerOverflow);
    }
    Ok(left / right)
}

fn div_i64_u(left: i64, right: i64) -> Result<i64, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(((left as u64) / (right as u64)) as i64)
}

fn rem_i64_s(left: i64, right: i64) -> Result<i64, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(left.wrapping_rem(right))
}

fn rem_i64_u(left: i64, right: i64) -> Result<i64, Trap> {
    if right == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(((left as u64) % (right as u64)) as i64)
}

fn f64_value(value: Option<&Value>) -> Result<f64, Trap> {
    match value {
        Some(Value::F64(value)) => Ok(f64::from_bits(*value)),
        _ => Err(Trap::HostFailure("expected IR f64 value".into())),
    }
}

fn trunc_f32_to_i32(value: f32, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if truncated < i32::MIN as f32 || truncated >= 2_147_483_648.0f32 {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I32(truncated as i32))
    } else if !(0.0..4_294_967_296.0f32).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I32(truncated as u32 as i32))
    }
}

fn trunc_f64_to_i32(value: f64, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if truncated < i32::MIN as f64 || truncated >= 2_147_483_648.0f64 {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I32(truncated as i32))
    } else if !(0.0..4_294_967_296.0f64).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I32(truncated as u32 as i32))
    }
}

fn trunc_f32_to_i64(value: f32, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if !(-9_223_372_036_854_775_808.0f32..9_223_372_036_854_775_808.0f32).contains(&truncated) {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I64(truncated as i64))
    } else if !(0.0..18_446_744_073_709_551_616.0f32).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I64(truncated as u64 as i64))
    }
}

fn trunc_f64_to_i64(value: f64, signed: bool) -> Result<Value, Trap> {
    if !value.is_finite() {
        return Err(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    if signed {
        if !(-9_223_372_036_854_775_808.0f64..9_223_372_036_854_775_808.0f64).contains(&truncated) {
            return Err(Trap::InvalidConversion);
        }
        Ok(Value::I64(truncated as i64))
    } else if !(0.0..18_446_744_073_709_551_616.0f64).contains(&truncated) {
        Err(Trap::InvalidConversion)
    } else {
        Ok(Value::I64(truncated as u64 as i64))
    }
}

fn i32_value(value: Option<&Value>) -> Result<i32, Trap> {
    match value {
        Some(Value::I32(value)) => Ok(*value),
        _ => Err(Trap::HostFailure("expected IR i32 value".into())),
    }
}

fn i64_value(value: Option<&Value>) -> Result<i64, Trap> {
    match value {
        Some(Value::I64(value)) => Ok(*value),
        _ => Err(Trap::HostFailure("expected IR i64 value".into())),
    }
}

fn compare_i32(left: i32, right: i32, comparison: IntComparison) -> i32 {
    let result = match comparison {
        IntComparison::Eq => left == right,
        IntComparison::Ne => left != right,
        IntComparison::LtS => left < right,
        IntComparison::LtU => (left as u32) < (right as u32),
        IntComparison::GtS => left > right,
        IntComparison::GtU => (left as u32) > (right as u32),
        IntComparison::LeS => left <= right,
        IntComparison::LeU => (left as u32) <= (right as u32),
        IntComparison::GeS => left >= right,
        IntComparison::GeU => (left as u32) >= (right as u32),
    };
    result as i32
}

fn compare_i64(left: i64, right: i64, comparison: IntComparison) -> i32 {
    let result = match comparison {
        IntComparison::Eq => left == right,
        IntComparison::Ne => left != right,
        IntComparison::LtS => left < right,
        IntComparison::LtU => (left as u64) < (right as u64),
        IntComparison::GtS => left > right,
        IntComparison::GtU => (left as u64) > (right as u64),
        IntComparison::LeS => left <= right,
        IntComparison::LeU => (left as u64) <= (right as u64),
        IntComparison::GeS => left >= right,
        IntComparison::GeU => (left as u64) >= (right as u64),
    };
    result as i32
}

fn compare_f32(left: f32, right: f32, comparison: FloatComparison) -> i32 {
    let result = match comparison {
        FloatComparison::Eq => left == right,
        FloatComparison::Ne => left != right,
        FloatComparison::Lt => left < right,
        FloatComparison::Gt => left > right,
        FloatComparison::Le => left <= right,
        FloatComparison::Ge => left >= right,
    };
    result as i32
}

fn compare_f64(left: f64, right: f64, comparison: FloatComparison) -> i32 {
    let result = match comparison {
        FloatComparison::Eq => left == right,
        FloatComparison::Ne => left != right,
        FloatComparison::Lt => left < right,
        FloatComparison::Gt => left > right,
        FloatComparison::Le => left <= right,
        FloatComparison::Ge => left >= right,
    };
    result as i32
}

fn f32_min(left: f32, right: f32) -> f32 {
    if left.is_nan() || right.is_nan() {
        f32::NAN
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

fn f32_max(left: f32, right: f32) -> f32 {
    if left.is_nan() || right.is_nan() {
        f32::NAN
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
        f64::NAN
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
        f64::NAN
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

fn unary_i32(value: i32, operation: IntUnary) -> i32 {
    match operation {
        IntUnary::Clz => value.leading_zeros() as i32,
        IntUnary::Ctz => value.trailing_zeros() as i32,
        IntUnary::Popcnt => value.count_ones() as i32,
    }
}

fn unary_i64(value: i64, operation: IntUnary) -> i64 {
    match operation {
        IntUnary::Clz => value.leading_zeros() as i64,
        IntUnary::Ctz => value.trailing_zeros() as i64,
        IntUnary::Popcnt => value.count_ones() as i64,
    }
}

fn default_value(value_type: ValueType) -> Value {
    match value_type {
        ValueType::I32 => Value::I32(0),
        ValueType::I64 => Value::I64(0),
        ValueType::F32 => Value::F32(0),
        ValueType::F64 => Value::F64(0),
        ValueType::V128 => Value::V128(0),
        ValueType::Ref(_) => Value::Ref(tpt_wasm_types::RefValue::Null(match value_type {
            ValueType::Ref(kind) => kind,
            _ => unreachable!(),
        })),
    }
}

fn execute_micro_at(
    module: &Module,
    function_index: usize,
    args: Vec<Value>,
) -> Result<Vec<Value>, Trap> {
    let mut store = Store::default();
    let mut func_addrs = Vec::with_capacity(module.functions.len());
    for (index, function) in module.functions.iter().enumerate() {
        let function_type = &module.types[function.type_index as usize];
        let instructions = decode_body(&function.body).expect("validated body should decode");
        let mut local_types = function_type.params.0.clone();
        for declaration in &function.locals {
            let count = usize::try_from(declaration.count).expect("test local count should fit");
            local_types.extend(std::iter::repeat_n(declaration.value_type, count));
        }
        let address = store
            .add_wasm_function(
                0,
                u32::try_from(index).expect("test function index should fit"),
                function_type.clone(),
                instructions,
                local_types,
            )
            .expect("test function should fit in store");
        func_addrs.push(address);
    }
    store
        .add_instance(StoreInstance {
            module_types: module.types.clone(),
            func_addrs,
            table_addrs: Vec::new(),
            memory_addrs: Vec::new(),
            global_addrs: Vec::new(),
        })
        .expect("test instance should fit in store");

    let function = &module.functions[function_index];
    let instructions = decode_body(&function.body).expect("validated body should decode");
    let result_arity = module.types[function.type_index as usize].results.0.len();
    let mut locals = args;
    for declaration in &function.locals {
        let count = usize::try_from(declaration.count).expect("test local count should fit");
        for _ in 0..count {
            locals.push(default_value(declaration.value_type));
        }
    }
    let mut machine = Machine::with_store(store);
    machine
        .push_frame(Frame::new(
            0,
            u32::try_from(function_index).expect("test function index should fit"),
            locals,
            instructions,
            result_arity,
        ))
        .expect("test frame should fit");
    match machine.run() {
        Step::Return(values) => Ok(values),
        Step::Trap(trap) => Err(trap),
        Step::Continue | Step::HostCall(_) => Err(Trap::HostFailure("Micro did not finish".into())),
    }
}

fn assert_differential(module: Module, args: Vec<Value>) -> Result<Vec<Value>, Trap> {
    assert_differential_at(module, 0, args)
}

fn assert_differential_at(
    module: Module,
    function_index: usize,
    args: Vec<Value>,
) -> Result<Vec<Value>, Trap> {
    let validated = validated(module.clone());
    let verified = lower_and_verify(&validated).expect("supported module should lower and verify");
    let ir_result = execute_ir_function(verified.module(), function_index, args.clone(), 0);
    assert_eq!(ir_result, execute_micro_at(&module, function_index, args));
    ir_result
}

#[test]
fn supported_numeric_subset_matches_micro() {
    let i32_result = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I32],
            vec![0x41, 20, 0x41, 22, 0x6a, 0x0b],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(i32_result, vec![Value::I32(42)]);

    let i64_result = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I64],
            vec![0x42, 50, 0x42, 8, 0x7d, 0x0b],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(i64_result, vec![Value::I64(42)]);

    let i32_mul = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I32],
            vec![0x41, 6, 0x41, 7, 0x6c, 0x0b],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(i32_mul, vec![Value::I32(42)]);

    let i64_mul = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I64],
            vec![0x42, 6, 0x42, 7, 0x7e, 0x0b],
        ),
        Vec::new(),
    )
    .unwrap();

    let unary = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::I32,
                ValueType::I32,
                ValueType::I32,
                ValueType::I64,
                ValueType::I64,
                ValueType::I64,
            ],
            vec![
                0x41, 1, 0x67, 0x41, 1, 0x68, 0x41, 1, 0x69, 0x42, 1, 0x79, 0x42, 1, 0x7a, 0x42, 1,
                0x7b, 0x0b,
            ],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        unary,
        vec![
            Value::I32(31),
            Value::I32(0),
            Value::I32(1),
            Value::I64(63),
            Value::I64(0),
            Value::I64(1),
        ]
    );

    assert_eq!(i64_mul, vec![Value::I64(42)]);

    let bitwise = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::I32,
                ValueType::I32,
                ValueType::I32,
                ValueType::I64,
                ValueType::I64,
                ValueType::I64,
            ],
            vec![
                0x41, 0b1010, 0x41, 0b0110, 0x71, 0x41, 0b1010, 0x41, 0b0110, 0x72, 0x41, 0b1010,
                0x41, 0b0110, 0x73, 0x42, 0b1010, 0x42, 0b0110, 0x83, 0x42, 0b1010, 0x42, 0b0110,
                0x84, 0x42, 0b1010, 0x42, 0b0110, 0x85, 0x0b,
            ],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        bitwise,
        vec![
            Value::I32(0b0010),
            Value::I32(0b1110),
            Value::I32(0b1100),
            Value::I64(0b0010),
            Value::I64(0b1110),
            Value::I64(0b1100),
        ]
    );

    let shifts = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::I32,
                ValueType::I32,
                ValueType::I32,
                ValueType::I64,
                ValueType::I64,
                ValueType::I64,
            ],
            vec![
                0x41, 16, 0x41, 2, 0x74, 0x41, 0x80, 0x7f, 0x41, 2, 0x75, 0x41, 0x80, 0x7f, 0x41,
                2, 0x76, 0x42, 16, 0x42, 2, 0x86, 0x42, 0x80, 0x7f, 0x42, 2, 0x87, 0x42, 0x80,
                0x7f, 0x42, 2, 0x88, 0x0b,
            ],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        shifts,
        vec![
            Value::I32(64),
            Value::I32(-32),
            Value::I32(1_073_741_792),
            Value::I64(64),
            Value::I64(-32),
            Value::I64(4_611_686_018_427_387_872),
        ]
    );

    let i32_comparisons = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I32; 11],
            vec![
                0x41, 0, 0x45, 0x41, 1, 0x41, 1, 0x46, 0x41, 1, 0x41, 2, 0x47, 0x41, 1, 0x41, 2,
                0x48, 0x41, 1, 0x41, 0x7f, 0x49, 0x41, 2, 0x41, 1, 0x4a, 0x41, 1, 0x41, 0x7f, 0x4b,
                0x41, 1, 0x41, 2, 0x4c, 0x41, 0x7f, 0x41, 1, 0x4d, 0x41, 2, 0x41, 1, 0x4e, 0x41, 1,
                0x41, 2, 0x4f, 0x0b,
            ],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        i32_comparisons,
        vec![
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(0),
            Value::I32(1),
            Value::I32(0),
            Value::I32(1),
            Value::I32(0),
        ]
    );

    let i64_comparisons = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I32; 11],
            vec![
                0x42, 0, 0x50, 0x42, 1, 0x42, 1, 0x51, 0x42, 1, 0x42, 2, 0x52, 0x42, 1, 0x42, 2,
                0x53, 0x42, 1, 0x42, 0x7f, 0x54, 0x42, 2, 0x42, 1, 0x55, 0x42, 1, 0x42, 0x7f, 0x56,
                0x42, 1, 0x42, 2, 0x57, 0x42, 0x7f, 0x42, 1, 0x58, 0x42, 2, 0x42, 1, 0x59, 0x42, 1,
                0x42, 2, 0x5a, 0x0b,
            ],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        i64_comparisons,
        vec![
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(1),
            Value::I32(0),
            Value::I32(1),
            Value::I32(0),
            Value::I32(1),
            Value::I32(0),
        ]
    );

    let mut rotation_body = Vec::new();
    for (opcode, left, right) in [(0x77, 1, 33), (0x78, i32::MIN, 33)] {
        rotation_body.extend([0x41]);
        rotation_body.extend(encode_i32(left));
        rotation_body.extend([0x41]);
        rotation_body.extend(encode_i32(right));
        rotation_body.push(opcode);
    }
    for (opcode, left, right) in [(0x89, 1i64, 65), (0x8a, i64::MIN, 65)] {
        rotation_body.extend([0x42]);
        rotation_body.extend(encode_i64(left));
        rotation_body.extend([0x42]);
        rotation_body.extend(encode_i64(right));
        rotation_body.push(opcode);
    }
    rotation_body.push(0x0b);
    let rotations = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::I32,
                ValueType::I32,
                ValueType::I64,
                ValueType::I64,
            ],
            rotation_body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        rotations,
        vec![
            Value::I32(2),
            Value::I32(1 << 30),
            Value::I64(2),
            Value::I64(1 << 62),
        ]
    );

    let mut float_unary_body = Vec::new();
    for opcode in 0x8b..=0x91 {
        float_unary_body.push(0x43);
        float_unary_body.extend(2.5f32.to_le_bytes());
        float_unary_body.push(opcode);
    }
    for opcode in 0x99..=0x9f {
        float_unary_body.push(0x44);
        float_unary_body.extend(2.5f64.to_le_bytes());
        float_unary_body.push(opcode);
    }
    float_unary_body.push(0x0b);
    let float_unary = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::F32; 7]
                .into_iter()
                .chain(std::iter::repeat_n(ValueType::F64, 7))
                .collect(),
            float_unary_body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        float_unary,
        vec![
            Value::F32(2.5f32.to_bits()),
            Value::F32((-2.5f32).to_bits()),
            Value::F32(3.0f32.to_bits()),
            Value::F32(2.0f32.to_bits()),
            Value::F32(2.0f32.to_bits()),
            Value::F32(2.0f32.to_bits()),
            Value::F32(2.5f32.sqrt().to_bits()),
            Value::F64(2.5f64.to_bits()),
            Value::F64((-2.5f64).to_bits()),
            Value::F64(3.0f64.to_bits()),
            Value::F64(2.0f64.to_bits()),
            Value::F64(2.0f64.to_bits()),
            Value::F64(2.0f64.to_bits()),
            Value::F64(2.5f64.sqrt().to_bits()),
        ]
    );

    let mut float_binary_body = Vec::new();
    for opcode in 0x96..=0x98 {
        float_binary_body.push(0x43);
        float_binary_body.extend(2.5f32.to_le_bytes());
        float_binary_body.push(0x43);
        float_binary_body.extend((-3.5f32).to_le_bytes());
        float_binary_body.push(opcode);
    }
    for opcode in 0xa4..=0xa6 {
        float_binary_body.push(0x44);
        float_binary_body.extend(2.5f64.to_le_bytes());
        float_binary_body.push(0x44);
        float_binary_body.extend((-3.5f64).to_le_bytes());
        float_binary_body.push(opcode);
    }
    float_binary_body.push(0x0b);
    let float_binary = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::F32; 3]
                .into_iter()
                .chain(std::iter::repeat_n(ValueType::F64, 3))
                .collect(),
            float_binary_body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        float_binary,
        vec![
            Value::F32((-3.5f32).to_bits()),
            Value::F32(2.5f32.to_bits()),
            Value::F32((-2.5f32).to_bits()),
            Value::F64((-3.5f64).to_bits()),
            Value::F64(2.5f64.to_bits()),
            Value::F64((-2.5f64).to_bits()),
        ]
    );

    let float_result = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::F32, ValueType::F64],
            vec![
                0x43, 0, 0, 0x80, 0x3f, 0x44, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f, 0x0b,
            ],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        float_result,
        vec![Value::F32(1.0f32.to_bits()), Value::F64(1.0f64.to_bits())]
    );

    let mut select_body = Vec::new();
    select_body.extend([0x41, 10, 0x41, 20, 0x41, 1, 0x1b]);
    select_body.extend([0x42, 30, 0x42, 40, 0x41, 0, 0x1b]);
    select_body.extend([0x43]);
    select_body.extend(1.5f32.to_le_bytes());
    select_body.push(0x43);
    select_body.extend(2.5f32.to_le_bytes());
    select_body.extend([0x41, 1, 0x1b]);
    select_body.push(0x44);
    select_body.extend(3.5f64.to_le_bytes());
    select_body.push(0x44);
    select_body.extend(4.5f64.to_le_bytes());
    select_body.extend([0x41, 0, 0x1b, 0x0b]);
    let selected = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::I32,
                ValueType::I64,
                ValueType::F32,
                ValueType::F64,
            ],
            select_body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        selected,
        vec![
            Value::I32(10),
            Value::I64(40),
            Value::F32(1.5f32.to_bits()),
            Value::F64(4.5f64.to_bits()),
        ]
    );

    let mut conversion_body = Vec::new();
    conversion_body.push(0x42);
    conversion_body.extend(encode_i64(0x0000_0001_0000_002a));
    conversion_body.push(0xa7);
    conversion_body.push(0x41);
    conversion_body.extend(encode_i32(-2));
    conversion_body.push(0xac);
    conversion_body.push(0x41);
    conversion_body.extend(encode_i32(-1));
    conversion_body.push(0xad);
    conversion_body.push(0x0b);
    let conversions = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I32, ValueType::I64, ValueType::I64],
            conversion_body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        conversions,
        vec![Value::I32(42), Value::I64(-2), Value::I64(4_294_967_295),]
    );

    let f32_bits = 0x7f80_1234u32;
    let f64_bits = 0x7ff0_1234_5678_9abcu64;
    let mut reinterpret_body = Vec::new();
    reinterpret_body.push(0x43);
    reinterpret_body.extend(f32_bits.to_le_bytes());
    reinterpret_body.push(0xbc);
    reinterpret_body.push(0x44);
    reinterpret_body.extend(f64_bits.to_le_bytes());
    reinterpret_body.push(0xbd);
    reinterpret_body.push(0x41);
    reinterpret_body.extend(encode_i32(f32_bits as i32));
    reinterpret_body.push(0xbe);
    reinterpret_body.push(0x42);
    reinterpret_body.extend(encode_i64(f64_bits as i64));
    reinterpret_body.push(0xbf);
    reinterpret_body.push(0x0b);
    let reinterpreted = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::I32,
                ValueType::I64,
                ValueType::F32,
                ValueType::F64,
            ],
            reinterpret_body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        reinterpreted,
        vec![
            Value::I32(f32_bits as i32),
            Value::I64(f64_bits as i64),
            Value::F32(f32_bits),
            Value::F64(f64_bits),
        ]
    );

    let mut division_body = Vec::new();
    for (opcode, left, right) in [(0x6d, -42, 6), (0x6e, -1, 2), (0x6f, -43, 6), (0x70, -1, 2)] {
        division_body.extend([0x41]);
        division_body.extend(encode_i32(left));
        division_body.extend([0x41]);
        division_body.extend(encode_i32(right));
        division_body.push(opcode);
    }
    division_body.extend([0x41, 0x80, 0x80, 0x80, 0x80, 0x78, 0x41, 0x7f, 0x6f]);
    for (opcode, left, right) in [
        (0x7f, -42i64, 6),
        (0x80, -1, 2),
        (0x81, -43, 6),
        (0x82, -1, 2),
    ] {
        division_body.extend([0x42]);
        division_body.extend(encode_i64(left));
        division_body.extend([0x42]);
        division_body.extend(encode_i64(right));
        division_body.push(opcode);
    }
    division_body.extend([0x42]);
    division_body.extend(encode_i64(i64::MIN));
    division_body.extend([0x42, 0x7f, 0x81, 0x0b]);
    let division_results = [
        ValueType::I32,
        ValueType::I32,
        ValueType::I32,
        ValueType::I32,
        ValueType::I32,
        ValueType::I64,
        ValueType::I64,
        ValueType::I64,
        ValueType::I64,
        ValueType::I64,
    ];
    let division = assert_differential(
        module(Vec::new(), division_results.to_vec(), division_body),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        division,
        vec![
            Value::I32(-7),
            Value::I32(((u32::MAX) / 2) as i32),
            Value::I32(-1),
            Value::I32(1),
            Value::I32(0),
            Value::I64(-7),
            Value::I64(((u64::MAX) / 2) as i64),
            Value::I64(-1),
            Value::I64(1),
            Value::I64(0),
        ]
    );

    let mut float_body = Vec::new();
    for opcode in [0x92, 0x93, 0x94, 0x95] {
        float_body.extend([0x43]);
        float_body.extend(2.0f32.to_le_bytes());
        float_body.extend([0x43]);
        float_body.extend(3.0f32.to_le_bytes());
        float_body.push(opcode);
    }
    for opcode in [0xa0, 0xa1, 0xa2, 0xa3] {
        float_body.extend([0x44]);
        float_body.extend(2.0f64.to_le_bytes());
        float_body.extend([0x44]);
        float_body.extend(3.0f64.to_le_bytes());
        float_body.push(opcode);
    }
    float_body.push(0x0b);
    let float_arithmetic = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
            ],
            float_body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        float_arithmetic,
        vec![
            Value::F32(5.0f32.to_bits()),
            Value::F32((-1.0f32).to_bits()),
            Value::F32(6.0f32.to_bits()),
            Value::F32((2.0f32 / 3.0f32).to_bits()),
            Value::F64(5.0f64.to_bits()),
            Value::F64((-1.0f64).to_bits()),
            Value::F64(6.0f64.to_bits()),
            Value::F64((2.0f64 / 3.0f64).to_bits()),
        ]
    );

    let mut float_comparison_body = Vec::new();
    for opcode in [0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x60] {
        float_comparison_body.extend([0x43]);
        float_comparison_body.extend(1.0f32.to_le_bytes());
        float_comparison_body.extend([0x43]);
        float_comparison_body.extend(2.0f32.to_le_bytes());
        float_comparison_body.push(opcode);
    }
    for opcode in [0x61, 0x62, 0x63, 0x64, 0x65, 0x66] {
        float_comparison_body.extend([0x44]);
        float_comparison_body.extend(1.0f64.to_le_bytes());
        float_comparison_body.extend([0x44]);
        float_comparison_body.extend(2.0f64.to_le_bytes());
        float_comparison_body.push(opcode);
    }
    float_comparison_body.extend([0x43]);
    float_comparison_body.extend(f32::NAN.to_bits().to_le_bytes());
    float_comparison_body.extend([0x43]);
    float_comparison_body.extend(1.0f32.to_le_bytes());
    float_comparison_body.extend([0x5c]);
    float_comparison_body.extend([0x44]);
    float_comparison_body.extend(f64::NAN.to_bits().to_le_bytes());
    float_comparison_body.extend([0x44]);
    float_comparison_body.extend(1.0f64.to_le_bytes());
    float_comparison_body.extend([0x62, 0x0b]);
    let float_comparisons = assert_differential(
        module(Vec::new(), vec![ValueType::I32; 14], float_comparison_body),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        float_comparisons,
        vec![
            Value::I32(0),
            Value::I32(1),
            Value::I32(1),
            Value::I32(0),
            Value::I32(1),
            Value::I32(0),
            Value::I32(0),
            Value::I32(1),
            Value::I32(1),
            Value::I32(0),
            Value::I32(1),
            Value::I32(0),
            Value::I32(1),
            Value::I32(1),
        ]
    );
}

#[test]
fn non_trapping_float_conversions_match_micro() {
    let mut body = Vec::new();
    body.push(0x41);
    body.extend(encode_i32(-1));
    body.push(0xb2);
    body.push(0x41);
    body.extend(encode_i32(-1));
    body.push(0xb3);
    body.push(0x42);
    body.extend(encode_i64(-1));
    body.push(0xb4);
    body.push(0x42);
    body.extend(encode_i64(-1));
    body.push(0xb5);
    body.push(0x44);
    body.extend(f64::NAN.to_bits().to_le_bytes());
    body.push(0xb6);
    body.push(0x41);
    body.extend(encode_i32(-1));
    body.push(0xb7);
    body.push(0x41);
    body.extend(encode_i32(-1));
    body.push(0xb8);
    body.push(0x42);
    body.extend(encode_i64(-1));
    body.push(0xb9);
    body.push(0x42);
    body.extend(encode_i64(-1));
    body.push(0xba);
    body.push(0x43);
    body.extend(f32::NAN.to_bits().to_le_bytes());
    body.push(0xbb);
    body.push(0x0b);

    let results = assert_differential(
        module(
            Vec::new(),
            vec![
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F32,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
                ValueType::F64,
            ],
            body,
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        results,
        vec![
            Value::F32((-1.0f32).to_bits()),
            Value::F32((u32::MAX as f32).to_bits()),
            Value::F32((-1.0f32).to_bits()),
            Value::F32((u64::MAX as f32).to_bits()),
            Value::F32(F32_CANONICAL_NAN),
            Value::F64((-1.0f64).to_bits()),
            Value::F64((u32::MAX as f64).to_bits()),
            Value::F64((-1.0f64).to_bits()),
            Value::F64((u64::MAX as f64).to_bits()),
            Value::F64(F64_CANONICAL_NAN),
        ]
    );
}

#[test]
fn integer_division_and_remainder_traps_match_micro() {
    for (body, result_type, trap) in [
        (
            vec![0x41, 1, 0x41, 0, 0x6d, 0x0b],
            ValueType::I32,
            Trap::IntegerDivisionByZero,
        ),
        (
            {
                let mut body = vec![0x41];
                body.extend(encode_i32(i32::MIN));
                body.extend([0x41, 0x7f, 0x6d, 0x0b]);
                body
            },
            ValueType::I32,
            Trap::IntegerOverflow,
        ),
        (
            vec![0x42, 1, 0x42, 0, 0x7f, 0x0b],
            ValueType::I64,
            Trap::IntegerDivisionByZero,
        ),
        (
            {
                let mut body = vec![0x42];
                body.extend(encode_i64(i64::MIN));
                body.extend([0x42, 0x7f, 0x7f, 0x0b]);
                body
            },
            ValueType::I64,
            Trap::IntegerOverflow,
        ),
    ] {
        assert_eq!(
            assert_differential(module(Vec::new(), vec![result_type], body), Vec::new()),
            Err(trap)
        );
    }
}

#[test]
fn local_get_set_and_tee_match_micro() {
    let result = assert_differential(
        module_with_locals(
            Vec::new(),
            vec![ValueType::I32],
            vec![LocalDecl {
                count: 1,
                value_type: ValueType::I32,
            }],
            vec![0x41, 7, 0x21, 0, 0x41, 8, 0x22, 0, 0x1a, 0x20, 0, 0x0b],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(result, vec![Value::I32(8)]);
}

fn direct_call_module() -> Module {
    let signature = function_type(vec![ValueType::I32, ValueType::I32], vec![ValueType::I32]);
    Module {
        types: vec![
            signature.clone(),
            function_type(Vec::new(), vec![ValueType::I32]),
        ],
        functions: vec![
            Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x20, 0, 0x20, 1, 0x6b, 0x0b],
            },
            Function {
                type_index: 1,
                locals: Vec::new(),
                body: vec![0x41, 20, 0x41, 21, 0x10, 0, 0x0b],
            },
        ],
        ..Module::default()
    }
}

#[test]
fn direct_calls_match_micro_and_preserve_argument_order() {
    assert_eq!(
        assert_differential_at(direct_call_module(), 1, Vec::new()).unwrap(),
        vec![Value::I32(-1)]
    );
}

#[test]
fn verifier_rejects_forged_call_targets_and_result_arity() {
    let mut ir = lower_module(&validated(direct_call_module())).unwrap();
    let call_index = ir.functions[1].blocks[0]
        .instrs
        .iter()
        .position(|instruction| matches!(instruction, IrInstr::Call { .. }))
        .unwrap();
    let IrInstr::Call { function, .. } = &mut ir.functions[1].blocks[0].instrs[call_index] else {
        unreachable!()
    };
    *function = 9;
    assert_eq!(
        verify_module(&ir),
        Err(VerificationError::UnknownFunction(9))
    );

    let IrInstr::Call {
        function, results, ..
    } = &mut ir.functions[1].blocks[0].instrs[call_index]
    else {
        unreachable!()
    };
    *function = 0;
    results.clear();
    assert_eq!(
        verify_module(&ir),
        Err(VerificationError::CallResultArity {
            function: 0,
            expected: 1,
            actual: 0,
        })
    );
}

#[test]
fn drop_nop_return_and_unreachable_are_lowered_explicitly() {
    let dropped = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I32],
            vec![0x01, 0x41, 9, 0x1a, 0x41, 7, 0x0b],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(dropped, vec![Value::I32(7)]);

    let returned = assert_differential(
        module(
            Vec::new(),
            vec![ValueType::I32],
            vec![0x41, 11, 0x0f, 0x41, 99, 0x0b],
        ),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(returned, vec![Value::I32(11)]);

    assert_eq!(
        assert_differential(module(Vec::new(), Vec::new(), vec![0x00, 0x0b]), Vec::new(),),
        Err(Trap::Unreachable)
    );
}

#[test]
fn float_truncation_matches_micro_and_traps_on_invalid_values() {
    let valid_cases = [
        (0xa8, 0x43, ValueType::I32, Value::I32(3)),
        (0xa9, 0x43, ValueType::I32, Value::I32(3)),
        (0xaa, 0x44, ValueType::I32, Value::I32(3)),
        (0xab, 0x44, ValueType::I32, Value::I32(3)),
        (0xae, 0x43, ValueType::I64, Value::I64(3)),
        (0xaf, 0x43, ValueType::I64, Value::I64(3)),
        (0xb0, 0x44, ValueType::I64, Value::I64(3)),
        (0xb1, 0x44, ValueType::I64, Value::I64(3)),
    ];
    for (opcode, source, result_type, expected) in valid_cases {
        let mut body = vec![source];
        if source == 0x43 {
            body.extend(3.9f32.to_bits().to_le_bytes());
        } else {
            body.extend(3.9f64.to_bits().to_le_bytes());
        }
        body.extend([opcode, 0x0b]);
        let result =
            assert_differential(module(Vec::new(), vec![result_type], body), Vec::new()).unwrap();
        assert_eq!(result, vec![expected]);
    }

    let trap_cases = [
        (0xa8, 0x43, ValueType::I32),
        (0xa9, 0x43, ValueType::I32),
        (0xaa, 0x44, ValueType::I32),
        (0xab, 0x44, ValueType::I32),
        (0xae, 0x43, ValueType::I64),
        (0xaf, 0x43, ValueType::I64),
        (0xb0, 0x44, ValueType::I64),
        (0xb1, 0x44, ValueType::I64),
    ];
    for (opcode, source, result_type) in trap_cases {
        let mut body = vec![source];
        if source == 0x43 {
            body.extend(f32::NAN.to_bits().to_le_bytes());
        } else {
            body.extend(f64::NAN.to_bits().to_le_bytes());
        }
        body.extend([opcode, 0x0b]);
        assert_eq!(
            assert_differential(module(Vec::new(), vec![result_type], body), Vec::new()),
            Err(Trap::InvalidConversion)
        );
    }

    let out_of_range = module(
        Vec::new(),
        vec![ValueType::I32],
        vec![0x44, 0, 0, 0, 0, 0, 0, 0xf0, 0x41, 0xab, 0x0b],
    );
    assert_eq!(
        assert_differential(out_of_range, Vec::new()),
        Err(Trap::InvalidConversion)
    );
}

#[test]
fn unsupported_module_state_and_instructions_are_rejected() {
    let mut with_memory = module(Vec::new(), Vec::new(), vec![0x0b]);
    with_memory.memories.push(tpt_wasm_format::Memory {
        memory_type: tpt_wasm_types::MemoryType {
            limits: tpt_wasm_types::Limits { min: 1, max: None },
            memory64: false,
        },
    });
    assert_eq!(
        lower_module(&validated(with_memory)),
        Err(LoweringError::UnsupportedFeature("memories"))
    );

    let mut with_export = module(Vec::new(), Vec::new(), vec![0x0b]);
    with_export.exports.push(Export {
        name: "run".into(),
        desc: ExportDesc::Function(0),
    });
    assert_eq!(
        lower_module(&validated(with_export)),
        Err(LoweringError::UnsupportedFeature("exports"))
    );

    let block = module(
        Vec::new(),
        vec![ValueType::I32],
        vec![0x02, 0x7f, 0x41, 1, 0x0b, 0x0b],
    );
    assert_eq!(
        lower_module(&validated(block)),
        Err(LoweringError::UnsupportedInstruction(0x02))
    );
}

#[test]
fn verifier_rejects_bad_result_arity_and_use_before_definition() {
    {
        let validated = validated(module(
            Vec::new(),
            vec![ValueType::I32],
            vec![0x41, 1, 0x0b],
        ));
        let mut ir = lower_module(&validated).unwrap();
        ir.functions[0].blocks[0].terminator = Terminator::Return(Vec::new());
        assert_eq!(
            verify_module(&ir),
            Err(VerificationError::ReturnArity {
                function: 0,
                expected: 1,
                actual: 0,
            })
        );

        ir.functions[0].blocks[0].instrs[0] = IrInstr::I32Add {
            result: ValueId(0),
            left: ValueId(0),
            right: ValueId(0),
        };
        assert_eq!(
            verify_module(&ir),
            Err(VerificationError::UseBeforeDefinition(ValueId(0)))
        );
    }

    let validated = validated(module(
        Vec::new(),
        vec![ValueType::I32],
        vec![0x41, 1, 0x0b],
    ));
    let mut ir = lower_module(&validated).unwrap();
    let mut extra = ir.functions[0].blocks[0].clone();
    extra.id = BlockId(1);
    ir.functions[0].blocks.push(extra);
    assert_eq!(
        verify_module(&ir),
        Err(VerificationError::UnsupportedBlockCount {
            function: 0,
            actual: 2,
        })
    );
}

#[test]
fn verifier_rejects_malformed_integer_conversion_result_type() {
    let validated = validated(module(
        Vec::new(),
        vec![ValueType::I32],
        vec![0x42, 1, 0xa7, 0x0b],
    ));
    let mut ir = lower_module(&validated).unwrap();
    let result = match &ir.functions[0].blocks[0].instrs[1] {
        IrInstr::IntConvert { result, .. } => *result,
        _ => panic!("conversion result should be present"),
    };
    ir.functions[0]
        .values
        .iter_mut()
        .find(|value| value.id == result)
        .expect("conversion result should be declared")
        .value_type = ValueType::I64;
    assert_eq!(
        verify_module(&ir),
        Err(VerificationError::TypeMismatch {
            function: 0,
            value: result,
            expected: ValueType::I32,
            actual: ValueType::I64,
        })
    );
}
#[test]
fn verifier_rejects_malformed_reinterpret_result_type() {
    let validated = validated(module(
        Vec::new(),
        vec![ValueType::I32],
        vec![0x43, 0x34, 0x12, 0x80, 0x7f, 0xbc, 0x0b],
    ));
    let mut ir = lower_module(&validated).unwrap();
    let result = match &ir.functions[0].blocks[0].instrs[1] {
        IrInstr::Reinterpret { result, .. } => *result,
        _ => panic!("reinterpret result should be present"),
    };
    ir.functions[0]
        .values
        .iter_mut()
        .find(|value| value.id == result)
        .expect("reinterpret result should be declared")
        .value_type = ValueType::F32;
    assert_eq!(
        verify_module(&ir),
        Err(VerificationError::TypeMismatch {
            function: 0,
            value: result,
            expected: ValueType::I32,
            actual: ValueType::F32,
        })
    );
}

#[test]
fn verifier_rejects_malformed_float_conversion_result_type() {
    let validated = validated(module(
        Vec::new(),
        vec![ValueType::F32],
        vec![0x41, 0x7f, 0xb2, 0x0b],
    ));
    let mut ir = lower_module(&validated).unwrap();
    let result = match &ir.functions[0].blocks[0].instrs[1] {
        IrInstr::FloatConvert { result, .. } => *result,
        _ => panic!("float conversion result should be present"),
    };
    ir.functions[0]
        .values
        .iter_mut()
        .find(|value| value.id == result)
        .expect("float conversion result should be declared")
        .value_type = ValueType::I32;
    assert_eq!(
        verify_module(&ir),
        Err(VerificationError::TypeMismatch {
            function: 0,
            value: result,
            expected: ValueType::F32,
            actual: ValueType::I32,
        })
    );
}

#[test]
fn verified_module_keeps_its_payload_immutable() {
    let validated = validated(module(
        Vec::new(),
        vec![ValueType::I32],
        vec![0x41, 42, 0x0b],
    ));
    let verified = lower_and_verify(&validated).unwrap();
    let function = &verified.module().functions[0];
    assert_eq!(function.blocks[0].id, function.entry);
    assert_eq!(verified.certificate(), &verified.certificate().clone());
}
