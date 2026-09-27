// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Constant evaluation with Wasm-exact semantics.
//!
//! This is a *third* implementation of the arithmetic rules, alongside Micro's
//! and the baseline backend's. That is a liability, and it is managed the only
//! way a third implementation can be: every function here is checked against the
//! baseline executor over a corpus of constants, and the float routines
//! deliberately mirror the backend's line for line, including its choice of
//! canonical NaN.
//!
//! Floats are the reason this module cannot be casual. Wasm permits a
//! non-deterministic NaN payload, and this project has fixed on the canonical
//! quiet NaN so two backends can be compared bit-for-bit. An optimizer that
//! folded `0.0 / 0.0` to a host NaN, or preserved a signalling payload through an
//! arithmetic operation, would produce a *different* module from the one Micro
//! runs, and the difference would surface only on the handful of inputs that
//! produce NaN -- exactly the kind of bug a differential test finds months later.

use tpt_wasm_ir::{
    FloatComparison, FloatConversion, FloatTrunc, FloatUnary, IntComparison, IntConversion,
    IntUnary, IrInstr, ValueId,
};
use tpt_wasm_types::{Trap, ValueType};

/// A statically known value.
///
/// Floats are held as raw bits rather than as `f32`/`f64`. A NaN's payload is part
/// of its value in Wasm, and holding the bits means no conversion can quietly
/// quiet it or drop its sign on the way through this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Const {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

impl Const {
    /// The Wasm value type of this constant.
    pub fn value_type(self) -> ValueType {
        match self {
            Self::I32(_) => ValueType::I32,
            Self::I64(_) => ValueType::I64,
            Self::F32(_) => ValueType::F32,
            Self::F64(_) => ValueType::F64,
        }
    }

    /// The `i32`, if this is one.
    pub fn as_i32(self) -> Option<i32> {
        match self {
            Self::I32(value) => Some(value),
            _ => None,
        }
    }

    /// The `i64`, if this is one.
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::I64(value) => Some(value),
            _ => None,
        }
    }
}

/// The canonical quiet NaN both backends produce.
///
/// Matching the backend's choice is what lets a folded constant be compared to
/// the unfolded module's result on raw bits rather than with an IEEE equality
/// that would treat every NaN as equal to every other NaN.
const F32_CANONICAL_NAN: u32 = 0x7fc0_0000;
const F64_CANONICAL_NAN: u64 = 0x7ff8_0000_0000_0000;

/// Canonicalize any NaN result, preserving every other bit pattern exactly.
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

/// `min`: NaN propagates, and `min(-0, +0)` is `-0`.
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

/// `max`: NaN propagates, and `max(-0, +0)` is `+0`.
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

/// What evaluating an instruction against known constants produced.
///
/// Not `Copy`: it carries a [`Trap`], which holds a `String` and so is not
/// `Copy` either. A folder returns one per instruction, so cloning would cost
/// an allocation on the trap path for no benefit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Folded {
    /// The operation is a value.
    Value(Const),
    /// The operation provably traps, with this trap.
    Traps(Trap),
    /// Not enough is known, or the operation is not one this module folds.
    Unknown,
}

/// The constant behind an operand, if that operand is known.
///
/// `operands` is indexed by `ValueId`, so it is a dense table over value numbers
/// rather than a map. The element type is `Option<Const>` rather than a sentinel
/// constant precisely because every bit pattern is a legal `i32`: a sentinel
/// would be indistinguishable from a real zero, and folding `0 + 1` to `0`
/// because a sentinel happened to equal zero is the kind of bug that passes a
/// smoke test and fails a spec suite.
fn operand(value: ValueId, operands: &[Option<Const>]) -> Option<Const> {
    operands.get(value.0 as usize).copied().flatten()
}

fn int32_pair(left: ValueId, right: ValueId, operands: &[Option<Const>]) -> Option<(i32, i32)> {
    match (operand(left, operands), operand(right, operands)) {
        (Some(Const::I32(l)), Some(Const::I32(r))) => Some((l, r)),
        _ => None,
    }
}

fn int64_pair(left: ValueId, right: ValueId, operands: &[Option<Const>]) -> Option<(i64, i64)> {
    match (operand(left, operands), operand(right, operands)) {
        (Some(Const::I64(l)), Some(Const::I64(r))) => Some((l, r)),
        _ => None,
    }
}

/// Fold a two-operand `i32` operation.
fn fold_i32(
    left: ValueId,
    right: ValueId,
    operands: &[Option<Const>],
    apply: fn(i32, i32) -> i32,
) -> Folded {
    match int32_pair(left, right, operands) {
        Some((l, r)) => Folded::Value(Const::I32(apply(l, r))),
        None => Folded::Unknown,
    }
}

fn fold_i64(
    left: ValueId,
    right: ValueId,
    operands: &[Option<Const>],
    apply: fn(i64, i64) -> i64,
) -> Folded {
    match int64_pair(left, right, operands) {
        Some((l, r)) => Folded::Value(Const::I64(apply(l, r))),
        None => Folded::Unknown,
    }
}

fn compare_i32(left: i32, right: i32, comparison: IntComparison) -> bool {
    match comparison {
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
    }
}

fn compare_i64(left: i64, right: i64, comparison: IntComparison) -> bool {
    match comparison {
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
    }
}

fn compare_f32(left: f32, right: f32, comparison: FloatComparison) -> bool {
    match comparison {
        FloatComparison::Eq => left == right,
        FloatComparison::Ne => left != right,
        FloatComparison::Lt => left < right,
        FloatComparison::Gt => left > right,
        FloatComparison::Le => left <= right,
        FloatComparison::Ge => left >= right,
    }
}

fn compare_f64(left: f64, right: f64, comparison: FloatComparison) -> bool {
    match comparison {
        FloatComparison::Eq => left == right,
        FloatComparison::Ne => left != right,
        FloatComparison::Lt => left < right,
        FloatComparison::Gt => left > right,
        FloatComparison::Le => left <= right,
        FloatComparison::Ge => left >= right,
    }
}
/// Evaluate an instruction whose operands are all known.
///
/// Returning [`Folded::Traps`] rather than only values is what makes folding
/// *trap-preserving* instead of trap-eliding. A folder that evaluates `1 / 0` to
/// "nothing to do here" is merely conservative; one that drops the instruction as
/// dead has deleted an observable trap. Returning the trap lets the caller
/// rewrite the access into the trap it always was.
pub fn evaluate(instruction: &IrInstr, operands: &[Option<Const>]) -> Folded {
    use IrInstr::*;
    match instruction {
        // Division and remainder are the only integer arithmetic that traps, so
        // they are handled explicitly rather than falling through to a
        // total-operation default that would have to special-case them anyway.
        I32DivS { left, right, .. } => {
            let (l, r) = match int32_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            if l == i32::MIN && r == -1 {
                return Folded::Traps(Trap::IntegerOverflow);
            }
            Folded::Value(Const::I32(l.wrapping_div(r)))
        }
        I32DivU { left, right, .. } => {
            let (l, r) = match int32_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            Folded::Value(Const::I32(((l as u32) / (r as u32)) as i32))
        }
        I32RemS { left, right, .. } => {
            let (l, r) = match int32_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            Folded::Value(Const::I32(l.wrapping_rem(r)))
        }
        I32RemU { left, right, .. } => {
            let (l, r) = match int32_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            Folded::Value(Const::I32(((l as u32) % (r as u32)) as i32))
        }
        I64DivS { left, right, .. } => {
            let (l, r) = match int64_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            if l == i64::MIN && r == -1 {
                return Folded::Traps(Trap::IntegerOverflow);
            }
            Folded::Value(Const::I64(l.wrapping_div(r)))
        }
        I64DivU { left, right, .. } => {
            let (l, r) = match int64_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            Folded::Value(Const::I64(((l as u64) / (r as u64)) as i64))
        }
        I64RemS { left, right, .. } => {
            let (l, r) = match int64_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            Folded::Value(Const::I64(l.wrapping_rem(r)))
        }
        I64RemU { left, right, .. } => {
            let (l, r) = match int64_pair(*left, *right, operands) {
                Some(pair) => pair,
                None => return Folded::Unknown,
            };
            if r == 0 {
                return Folded::Traps(Trap::IntegerDivisionByZero);
            }
            Folded::Value(Const::I64(((l as u64) % (r as u64)) as i64))
        }
        FloatTrunc {
            value, operation, ..
        } => match operand(*value, operands) {
            Some(Const::F32(bits)) => float_trunc_f32(*operation, bits),
            Some(Const::F64(bits)) => float_trunc_f64(*operation, bits),
            _ => Folded::Unknown,
        },
        I32Add { left, right, .. } => fold_i32(*left, *right, operands, i32::wrapping_add),
        I32Sub { left, right, .. } => fold_i32(*left, *right, operands, i32::wrapping_sub),
        I32Mul { left, right, .. } => fold_i32(*left, *right, operands, i32::wrapping_mul),
        I32And { left, right, .. } => fold_i32(*left, *right, operands, |a, b| a & b),
        I32Or { left, right, .. } => fold_i32(*left, *right, operands, |a, b| a | b),
        I32Xor { left, right, .. } => fold_i32(*left, *right, operands, |a, b| a ^ b),
        I32Shl { left, right, .. } => {
            fold_i32(*left, *right, operands, |a, b| a.wrapping_shl(b as u32))
        }
        I32ShrS { left, right, .. } => {
            fold_i32(*left, *right, operands, |a, b| a.wrapping_shr(b as u32))
        }
        I32ShrU { left, right, .. } => fold_i32(*left, *right, operands, |a, b| {
            ((a as u32).wrapping_shr(b as u32)) as i32
        }),
        I32Rotl { left, right, .. } => fold_i32(*left, *right, operands, |a, b| {
            a.rotate_left((b as u32) & 31)
        }),
        I32Rotr { left, right, .. } => fold_i32(*left, *right, operands, |a, b| {
            a.rotate_right((b as u32) & 31)
        }),
        I64Add { left, right, .. } => fold_i64(*left, *right, operands, i64::wrapping_add),
        I64Sub { left, right, .. } => fold_i64(*left, *right, operands, i64::wrapping_sub),
        I64Mul { left, right, .. } => fold_i64(*left, *right, operands, i64::wrapping_mul),
        I64And { left, right, .. } => fold_i64(*left, *right, operands, |a, b| a & b),
        I64Or { left, right, .. } => fold_i64(*left, *right, operands, |a, b| a | b),
        I64Xor { left, right, .. } => fold_i64(*left, *right, operands, |a, b| a ^ b),
        I64Shl { left, right, .. } => {
            fold_i64(*left, *right, operands, |a, b| a.wrapping_shl(b as u32))
        }
        I64ShrS { left, right, .. } => {
            fold_i64(*left, *right, operands, |a, b| a.wrapping_shr(b as u32))
        }
        I64ShrU { left, right, .. } => fold_i64(*left, *right, operands, |a, b| {
            ((a as u64).wrapping_shr(b as u32)) as i64
        }),
        I64Rotl { left, right, .. } => fold_i64(*left, *right, operands, |a, b| {
            a.rotate_left((b as u32) & 63)
        }),
        I64Rotr { left, right, .. } => fold_i64(*left, *right, operands, |a, b| {
            a.rotate_right((b as u32) & 63)
        }),
        _ => evaluate_folded(instruction, operands),
    }
}
/// The float, comparison, and conversion operations.
///
/// Split from [`evaluate`] purely for length. The two halves share the same
/// contract: a wrong answer here is a miscompiled module, and the cross-check
/// test against the baseline executor is what keeps them honest.
fn evaluate_folded(instruction: &IrInstr, operands: &[Option<Const>]) -> Folded {
    use IrInstr::*;
    match instruction {
        I32Eqz { value, .. } => match operand(*value, operands) {
            Some(Const::I32(v)) => Folded::Value(Const::I32(i32::from(v == 0))),
            _ => Folded::Unknown,
        },
        I64Eqz { value, .. } => match operand(*value, operands) {
            Some(Const::I64(v)) => Folded::Value(Const::I32(i32::from(v == 0))),
            _ => Folded::Unknown,
        },
        I32Unary {
            value, operation, ..
        } => match operand(*value, operands) {
            Some(Const::I32(v)) => Folded::Value(Const::I32(match operation {
                IntUnary::Clz => v.leading_zeros() as i32,
                IntUnary::Ctz => v.trailing_zeros() as i32,
                IntUnary::Popcnt => v.count_ones() as i32,
            })),
            _ => Folded::Unknown,
        },
        I64Unary {
            value, operation, ..
        } => match operand(*value, operands) {
            Some(Const::I64(v)) => Folded::Value(Const::I64(match operation {
                IntUnary::Clz => v.leading_zeros() as i64,
                IntUnary::Ctz => v.trailing_zeros() as i64,
                IntUnary::Popcnt => v.count_ones() as i64,
            })),
            _ => Folded::Unknown,
        },
        I32Compare {
            left,
            right,
            comparison,
            ..
        } => match int32_pair(*left, *right, operands) {
            Some((l, r)) => Folded::Value(Const::I32(i32::from(compare_i32(l, r, *comparison)))),
            None => Folded::Unknown,
        },
        I64Compare {
            left,
            right,
            comparison,
            ..
        } => match int64_pair(*left, *right, operands) {
            Some((l, r)) => Folded::Value(Const::I32(i32::from(compare_i64(l, r, *comparison)))),
            None => Folded::Unknown,
        },
        F32Compare {
            left,
            right,
            comparison,
            ..
        } => match (operand(*left, operands), operand(*right, operands)) {
            (Some(Const::F32(l)), Some(Const::F32(r))) => Folded::Value(Const::I32(i32::from(
                compare_f32(f32::from_bits(l), f32::from_bits(r), *comparison),
            ))),
            _ => Folded::Unknown,
        },
        F64Compare {
            left,
            right,
            comparison,
            ..
        } => match (operand(*left, operands), operand(*right, operands)) {
            (Some(Const::F64(l)), Some(Const::F64(r))) => Folded::Value(Const::I32(i32::from(
                compare_f64(f64::from_bits(l), f64::from_bits(r), *comparison),
            ))),
            _ => Folded::Unknown,
        },
        Select {
            condition,
            left,
            right,
            ..
        } => {
            // A select computes only the arm it chooses, and neither arm can trap
            // or have an effect here, so picking a side is sound. The condition
            // is Wasm's `i32`: any non-zero value selects the first arm.
            match (
                operand(*condition, operands),
                operand(*left, operands),
                operand(*right, operands),
            ) {
                (Some(Const::I32(c)), Some(l), Some(r)) => {
                    Folded::Value(if c != 0 { l } else { r })
                }
                _ => Folded::Unknown,
            }
        }
        F32Add { left, right, .. } => fold_f32(*left, *right, operands, |a, b| a + b),
        F32Sub { left, right, .. } => fold_f32(*left, *right, operands, |a, b| a - b),
        F32Mul { left, right, .. } => fold_f32(*left, *right, operands, |a, b| a * b),
        F32Div { left, right, .. } => fold_f32(*left, *right, operands, |a, b| a / b),
        F32Min { left, right, .. } => fold_f32(*left, *right, operands, f32_min),
        F32Max { left, right, .. } => fold_f32(*left, *right, operands, f32_max),
        // `copysign` is a bit operation and must not canonicalize a NaN payload;
        // every other `f32` operation must.
        F32Copysign { left, right, .. } => {
            match (operand(*left, operands), operand(*right, operands)) {
                (Some(Const::F32(l)), Some(Const::F32(r))) => Folded::Value(Const::F32(
                    f32::copysign(f32::from_bits(l), f32::from_bits(r)).to_bits(),
                )),
                _ => Folded::Unknown,
            }
        }
        F64Add { left, right, .. } => fold_f64(*left, *right, operands, |a, b| a + b),
        F64Sub { left, right, .. } => fold_f64(*left, *right, operands, |a, b| a - b),
        F64Mul { left, right, .. } => fold_f64(*left, *right, operands, |a, b| a * b),
        F64Div { left, right, .. } => fold_f64(*left, *right, operands, |a, b| a / b),
        F64Min { left, right, .. } => fold_f64(*left, *right, operands, f64_min),
        F64Max { left, right, .. } => fold_f64(*left, *right, operands, f64_max),
        F64Copysign { left, right, .. } => {
            match (operand(*left, operands), operand(*right, operands)) {
                (Some(Const::F64(l)), Some(Const::F64(r))) => Folded::Value(Const::F64(
                    f64::copysign(f64::from_bits(l), f64::from_bits(r)).to_bits(),
                )),
                _ => Folded::Unknown,
            }
        }
        _ => evaluate_conversions(instruction, operands),
    }
}

fn fold_f32(
    left: ValueId,
    right: ValueId,
    operands: &[Option<Const>],
    apply: fn(f32, f32) -> f32,
) -> Folded {
    match (operand(left, operands), operand(right, operands)) {
        (Some(Const::F32(l)), Some(Const::F32(r))) => Folded::Value(Const::F32(f32_result(apply(
            f32::from_bits(l),
            f32::from_bits(r),
        )))),
        _ => Folded::Unknown,
    }
}

fn fold_f64(
    left: ValueId,
    right: ValueId,
    operands: &[Option<Const>],
    apply: fn(f64, f64) -> f64,
) -> Folded {
    match (operand(left, operands), operand(right, operands)) {
        (Some(Const::F64(l)), Some(Const::F64(r))) => Folded::Value(Const::F64(f64_result(apply(
            f64::from_bits(l),
            f64::from_bits(r),
        )))),
        _ => Folded::Unknown,
    }
}
/// The width, sign, and raw-bit conversions, plus the float unary operations.
///
/// Every conversion here is *non-trapping*: those live in
/// [`float_trunc`] with the other operation that can fail. Keeping the two
/// apart is what makes the trap boundary obvious at the call site.
fn evaluate_conversions(instruction: &IrInstr, operands: &[Option<Const>]) -> Folded {
    use IrInstr::*;
    match instruction {
        IntConvert {
            value, operation, ..
        } => match (operation, operand(*value, operands)) {
            (IntConversion::I32WrapI64, Some(Const::I64(v))) => Folded::Value(Const::I32(v as i32)),
            (IntConversion::I64ExtendI32S, Some(Const::I32(v))) => {
                Folded::Value(Const::I64(i64::from(v)))
            }
            (IntConversion::I64ExtendI32U, Some(Const::I32(v))) => {
                Folded::Value(Const::I64(i64::from(v as u32)))
            }
            _ => Folded::Unknown,
        },
        SignExtend {
            value, operation, ..
        } => match (operation, operand(*value, operands)) {
            (tpt_wasm_ir::SignExtend::I32Extend8S, Some(Const::I32(v))) => {
                Folded::Value(Const::I32(v as i8 as i32))
            }
            (tpt_wasm_ir::SignExtend::I32Extend16S, Some(Const::I32(v))) => {
                Folded::Value(Const::I32(v as i16 as i32))
            }
            (tpt_wasm_ir::SignExtend::I64Extend8S, Some(Const::I64(v))) => {
                Folded::Value(Const::I64(v as i8 as i64))
            }
            (tpt_wasm_ir::SignExtend::I64Extend16S, Some(Const::I64(v))) => {
                Folded::Value(Const::I64(v as i16 as i64))
            }
            (tpt_wasm_ir::SignExtend::I64Extend32S, Some(Const::I64(v))) => {
                Folded::Value(Const::I64(v as i32 as i64))
            }
            _ => Folded::Unknown,
        },
        // Reinterpretation moves bits and does nothing else. In particular it
        // must NOT canonicalize: `i32.reinterpret_f32` on a signalling NaN has
        // to return the payload Micro returns, which is the whole point of the
        // operation.
        Reinterpret {
            value, operation, ..
        } => match (operation, operand(*value, operands)) {
            (tpt_wasm_ir::Reinterpret::I32FromF32, Some(Const::F32(bits))) => {
                Folded::Value(Const::I32(bits as i32))
            }
            (tpt_wasm_ir::Reinterpret::I64FromF64, Some(Const::F64(bits))) => {
                Folded::Value(Const::I64(bits as i64))
            }
            (tpt_wasm_ir::Reinterpret::F32FromI32, Some(Const::I32(v))) => {
                Folded::Value(Const::F32(v as u32))
            }
            (tpt_wasm_ir::Reinterpret::F64FromI64, Some(Const::I64(v))) => {
                Folded::Value(Const::F64(v as u64))
            }
            _ => Folded::Unknown,
        },
        FloatConvert {
            value, operation, ..
        } => match (operation, operand(*value, operands)) {
            (FloatConversion::F32FromI32S, Some(Const::I32(v))) => {
                Folded::Value(Const::F32(f32_result(v as f32)))
            }
            (FloatConversion::F32FromI32U, Some(Const::I32(v))) => {
                Folded::Value(Const::F32(f32_result(v as u32 as f32)))
            }
            (FloatConversion::F32FromI64S, Some(Const::I64(v))) => {
                Folded::Value(Const::F32(f32_result(v as f32)))
            }
            (FloatConversion::F32FromI64U, Some(Const::I64(v))) => {
                Folded::Value(Const::F32(f32_result(v as u64 as f32)))
            }
            (FloatConversion::F32FromF64, Some(Const::F64(bits))) => {
                Folded::Value(Const::F32(f32_result(f64::from_bits(bits) as f32)))
            }
            (FloatConversion::F64FromI32S, Some(Const::I32(v))) => {
                Folded::Value(Const::F64(f64_result(v as f64)))
            }
            (FloatConversion::F64FromI32U, Some(Const::I32(v))) => {
                Folded::Value(Const::F64(f64_result(v as u32 as f64)))
            }
            (FloatConversion::F64FromI64S, Some(Const::I64(v))) => {
                Folded::Value(Const::F64(f64_result(v as f64)))
            }
            (FloatConversion::F64FromI64U, Some(Const::I64(v))) => {
                Folded::Value(Const::F64(f64_result(v as u64 as f64)))
            }
            (FloatConversion::F64FromF32, Some(Const::F32(bits))) => {
                Folded::Value(Const::F64(f64_result(f32::from_bits(bits) as f64)))
            }
            _ => Folded::Unknown,
        },
        F32Unary {
            value, operation, ..
        } => match operand(*value, operands) {
            Some(Const::F32(bits)) => {
                let raw = f32::from_bits(bits);
                Folded::Value(Const::F32(match operation {
                    // `abs` and `neg` only move the sign bit, so they preserve the
                    // payload of a NaN and must not canonicalize.
                    FloatUnary::Abs => raw.to_bits() & 0x7fff_ffff,
                    FloatUnary::Neg => raw.to_bits() ^ 0x8000_0000,
                    FloatUnary::Ceil => f32_result(raw.ceil()),
                    FloatUnary::Floor => f32_result(raw.floor()),
                    FloatUnary::Trunc => f32_result(raw.trunc()),
                    FloatUnary::Nearest => f32_result(raw.round_ties_even()),
                    FloatUnary::Sqrt => f32_result(raw.sqrt()),
                }))
            }
            _ => Folded::Unknown,
        },
        F64Unary {
            value, operation, ..
        } => match operand(*value, operands) {
            Some(Const::F64(bits)) => {
                let raw = f64::from_bits(bits);
                Folded::Value(Const::F64(match operation {
                    FloatUnary::Abs => raw.to_bits() & 0x7fff_ffff_ffff_ffff,
                    FloatUnary::Neg => raw.to_bits() ^ 0x8000_0000_0000_0000,
                    FloatUnary::Ceil => f64_result(raw.ceil()),
                    FloatUnary::Floor => f64_result(raw.floor()),
                    FloatUnary::Trunc => f64_result(raw.trunc()),
                    FloatUnary::Nearest => f64_result(raw.round_ties_even()),
                    FloatUnary::Sqrt => f64_result(raw.sqrt()),
                }))
            }
            _ => Folded::Unknown,
        },
        _ => Folded::Unknown,
    }
}
/// A trapping float-to-integer conversion from an `f32`.
///
/// Every bound is evaluated in `f32`, not in a widened `f64`. That is not a
/// stylistic choice: the backend does the same, and the two differ at the edges.
/// `2147483648.0f64` is exactly `2^31`, but the nearest `f32` above
/// `i32::MAX as f32` is `2147483648.0f32`, which is out of range either way --
/// while `i64` bounds genuinely do not fit an `f32`, and a widened `f64` range
/// test would accept a value the backend rejects. Matching the source precision
/// is what makes a folded conversion agree with the unfolded one at the
/// boundary, which is precisely where a folding bug hides.
fn float_trunc_f32(operation: FloatTrunc, bits: u32) -> Folded {
    let value = f32::from_bits(bits);
    if !value.is_finite() {
        return Folded::Traps(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    let invalid = Folded::Traps(Trap::InvalidConversion);
    match operation {
        FloatTrunc::I32FromF32S => {
            if truncated < i32::MIN as f32 || truncated >= 2147483648.0f32 {
                return invalid;
            }
            Folded::Value(Const::I32(truncated as i32))
        }
        FloatTrunc::I32FromF32U => {
            if !(0.0f32..4294967296.0f32).contains(&truncated) {
                return invalid;
            }
            Folded::Value(Const::I32(truncated as u32 as i32))
        }
        FloatTrunc::I32FromF64S | FloatTrunc::I32FromF64U => Folded::Unknown,
        FloatTrunc::I64FromF32S => {
            if !(-9223372036854775808.0f32..9223372036854775808.0f32).contains(&truncated) {
                return invalid;
            }
            Folded::Value(Const::I64(truncated as i64))
        }
        FloatTrunc::I64FromF32U => {
            if !(0.0f32..18446744073709551616.0f32).contains(&truncated) {
                return invalid;
            }
            Folded::Value(Const::I64(truncated as u64 as i64))
        }
        FloatTrunc::I64FromF64S | FloatTrunc::I64FromF64U => Folded::Unknown,
    }
}

/// A trapping float-to-integer conversion from an `f64`.
fn float_trunc_f64(operation: FloatTrunc, bits: u64) -> Folded {
    let value = f64::from_bits(bits);
    if !value.is_finite() {
        return Folded::Traps(Trap::InvalidConversion);
    }
    let truncated = value.trunc();
    let invalid = Folded::Traps(Trap::InvalidConversion);
    match operation {
        FloatTrunc::I32FromF64S => {
            if truncated < i32::MIN as f64 || truncated >= 2147483648.0f64 {
                return invalid;
            }
            Folded::Value(Const::I32(truncated as i32))
        }
        FloatTrunc::I32FromF64U => {
            if !(0.0f64..4294967296.0f64).contains(&truncated) {
                return invalid;
            }
            Folded::Value(Const::I32(truncated as u32 as i32))
        }
        FloatTrunc::I64FromF64S => {
            if !(-9223372036854775808.0f64..9223372036854775808.0f64).contains(&truncated) {
                return invalid;
            }
            Folded::Value(Const::I64(truncated as i64))
        }
        FloatTrunc::I64FromF64U => {
            if !(0.0f64..18446744073709551616.0f64).contains(&truncated) {
                return invalid;
            }
            Folded::Value(Const::I64(truncated as u64 as i64))
        }
        FloatTrunc::I32FromF32S
        | FloatTrunc::I32FromF32U
        | FloatTrunc::I64FromF32S
        | FloatTrunc::I64FromF32U => Folded::Unknown,
    }
}
