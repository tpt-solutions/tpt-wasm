// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Constant expressions: the instructions an initializer or a segment offset may use, evaluated to a
//! value.
//!
//! An expression is a sequence of constants, reads of globals, and the `i32` and `i64` integer operations
//! `add`, `sub` and `mul`, ended by `end`. The value is the one value left on the stack. Evaluation takes
//! the value of each global as a callback, so the same evaluator serves the lowering (where a global may
//! have no value yet) and the runtime (where every global has one).

use tpt_wasm_types::Value;

/// Why a constant expression could not be evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstExprError {
    /// The expression does not decode, or does not leave exactly one value.
    Malformed,
    /// The expression uses an instruction a constant expression may not contain.
    Unsupported(u8),
    /// The expression reads a global the callback has no value for.
    UnknownGlobal(u32),
}

/// Evaluate a constant expression, reading each global through `global`.
pub fn eval_const(
    bytes: &[u8],
    global: &dyn Fn(u32) -> Option<Value>,
) -> Result<Value, ConstExprError> {
    let mut reader = Reader { bytes, at: 0 };
    let mut stack: Vec<Value> = Vec::new();
    loop {
        match reader.byte()? {
            0x0b => break,
            0x41 => stack.push(Value::I32(reader.signed()? as i32)),
            0x42 => stack.push(Value::I64(reader.signed()?)),
            0x43 => stack.push(Value::F32(u32::from_le_bytes(reader.array()?))),
            0x44 => stack.push(Value::F64(u64::from_le_bytes(reader.array()?))),
            0x23 => {
                let index = reader.unsigned()? as u32;
                stack.push(global(index).ok_or(ConstExprError::UnknownGlobal(index))?);
            }
            opcode @ (0x6a..=0x6c) => {
                let right = pop_i32(&mut stack)?;
                let left = pop_i32(&mut stack)?;
                stack.push(Value::I32(match opcode {
                    0x6a => left.wrapping_add(right),
                    0x6b => left.wrapping_sub(right),
                    _ => left.wrapping_mul(right),
                }));
            }
            opcode @ (0x7c..=0x7e) => {
                let right = pop_i64(&mut stack)?;
                let left = pop_i64(&mut stack)?;
                stack.push(Value::I64(match opcode {
                    0x7c => left.wrapping_add(right),
                    0x7d => left.wrapping_sub(right),
                    _ => left.wrapping_mul(right),
                }));
            }
            other => return Err(ConstExprError::Unsupported(other)),
        }
    }
    if reader.at != bytes.len() {
        return Err(ConstExprError::Malformed);
    }
    match stack.as_slice() {
        [value] => Ok(value.clone()),
        _ => Err(ConstExprError::Malformed),
    }
}

/// Whether the expression reads any global, which makes its value depend on the module's imports.
pub fn reads_globals(bytes: &[u8]) -> bool {
    let mut reader = Reader { bytes, at: 0 };
    loop {
        let Ok(opcode) = reader.byte() else {
            return false;
        };
        let skipped = match opcode {
            0x0b => return false,
            0x23 => return true,
            0x41 | 0x42 => reader.signed().map(|_| ()),
            0x43 => reader.array::<4>().map(|_| ()),
            0x44 => reader.array::<8>().map(|_| ()),
            0x6a..=0x6c | 0x7c..=0x7e => Ok(()),
            _ => return false,
        };
        if skipped.is_err() {
            return false;
        }
    }
}

fn pop_i32(stack: &mut Vec<Value>) -> Result<i32, ConstExprError> {
    match stack.pop() {
        Some(Value::I32(value)) => Ok(value),
        _ => Err(ConstExprError::Malformed),
    }
}

fn pop_i64(stack: &mut Vec<Value>) -> Result<i64, ConstExprError> {
    match stack.pop() {
        Some(Value::I64(value)) => Ok(value),
        _ => Err(ConstExprError::Malformed),
    }
}

/// A cursor over the expression's bytes. Every read is bounded, so a truncated expression is malformed
/// rather than a panic.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, ConstExprError> {
        let byte = *self.bytes.get(self.at).ok_or(ConstExprError::Malformed)?;
        self.at += 1;
        Ok(byte)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ConstExprError> {
        let end = self.at.checked_add(N).ok_or(ConstExprError::Malformed)?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or(ConstExprError::Malformed)?;
        self.at = end;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }

    fn unsigned(&mut self) -> Result<u64, ConstExprError> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            if shift >= 64 {
                return Err(ConstExprError::Malformed);
            }
            result |= u64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
    }

    fn signed(&mut self) -> Result<i64, ConstExprError> {
        let mut result = 0i64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            if shift >= 64 {
                return Err(ConstExprError::Malformed);
            }
            result |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result);
            }
        }
    }
}
