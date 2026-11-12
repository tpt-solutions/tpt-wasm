// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A JIT compiler for a subset of the TPT IR, generating machine code for x86-64, AArch64, and
//! RISC-V 64.
//!
//! ## What is compiled
//!
//! A function over integers, `i32` or `i64`, returning one of them, whose body is made of:
//!
//! - constants; reads and writes of parameters and declared locals (`local.get`, `local.set`,
//!   `local.tee`);
//! - the integer operations `add`, `sub`, `mul`, `and`, `or`, `xor`, the shifts `shl`, `shr_s`,
//!   and `shr_u` (counts are masked to the operand width, as in Wasm), division and remainder
//!   (signed and unsigned, with Wasm's traps), the comparisons, `eqz`, `select`, the integer
//!   width conversions (`wrap`, `extend`), and the sign extensions `extend8_s`, `extend16_s`,
//!   and `extend32_s`;
//! - linear memory: loads and stores of all the integer widths, each checked against the
//!   memory's length;
//! - control flow: unconditional and conditional branches, including loops, and a return.
//!
//! Each value lives in a stack slot, so each IR instruction is a load, at most one operation,
//! and a store. Anything else is refused with [`JitError::Unsupported`] rather than compiled
//! approximately: floats, rotations, bit counting, calls, memory growth and size, and a frame
//! larger than the encoder's limit are left to the interpreter or the baseline.
//!
//! ## Calling convention
//!
//! Every compiled function has the one signature `(args: *const i64, ctx: *mut Context) -> u64`
//! on the host's C convention. Each argument is an eight-byte slot in `args`, and the context
//! gives the memory base and length and receives the trap status. The return value is the result
//! register, so an `i64` result is the whole of it, and an `i32` result is its low half.
//!
//! A trap stores a status in the context and returns. The status is read back after the call,
//! and it names the trap Micro reports for the same input.
//!
//! ## Safety boundary
//!
//! The crate denies `unsafe` code except in the `exec` module, which maps executable memory and
//! calls into it. Everything else, including the encoders, is safe code and is tested on every
//! target. Generated code exists only where [`target_id`] names a supported target.

#![deny(unsafe_code)]

use std::fmt;

use tpt_wasm_ir::IrFunction;
use tpt_wasm_types::{Trap, ValueType};

mod aarch64;
mod riscv64;
mod x86;

#[allow(unsafe_code)]
mod exec;

/// The status a compiled function leaves in the context when it divides by zero.
pub(crate) const STATUS_DIVIDE_BY_ZERO: u32 = 1;
/// The status for the minimum value divided by -1, the one signed division whose result does not
/// fit.
pub(crate) const STATUS_OVERFLOW: u32 = 2;
/// The status for a load or store outside the memory.
pub(crate) const STATUS_MEMORY_OUT_OF_BOUNDS: u32 = 3;

/// What a compiled function reads and writes besides its arguments.
///
/// The layout is part of the calling convention, and the generated code reads and writes it at
/// fixed offsets: the memory base at 0, the memory length at 8, and the trap status at 16.
#[repr(C)]
pub(crate) struct Context {
    pub(crate) memory: *mut u8,
    pub(crate) memory_len: u64,
    pub(crate) status: u64,
}

/// Why a function was not compiled, or could not be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitError {
    /// The function uses something this compiler does not generate code for.
    Unsupported(String),
    /// A call passed a different number of arguments than the function declares.
    ArityMismatch { expected: usize, actual: usize },
    /// Executable memory could not be obtained or protected.
    ExecutableMemory(String),
    /// The target has no code generator.
    UnsupportedTarget,
    /// The compiled code trapped, with the same trap Micro reports for the same input.
    Trap(Trap),
}

impl fmt::Display for JitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JitError::Unsupported(what) => write!(f, "not compiled: {what}"),
            JitError::ArityMismatch { expected, actual } => {
                write!(f, "expected {expected} arguments, got {actual}")
            }
            JitError::ExecutableMemory(message) => write!(f, "executable memory: {message}"),
            JitError::UnsupportedTarget => write!(f, "no code generator for this target"),
            JitError::Trap(trap) => write!(f, "trap: {trap}"),
        }
    }
}

impl std::error::Error for JitError {}

/// A function compiled to machine code, ready to call.
pub struct CompiledFunction {
    params: Vec<ValueType>,
    result: ValueType,
    code: exec::ExecutableCode,
}

impl CompiledFunction {
    /// The number of arguments the function takes.
    pub fn arity(&self) -> usize {
        self.params.len()
    }

    /// Run the function on `i32` arguments and return its `i32` result, with no linear memory: a
    /// memory access traps.
    ///
    /// Refused unless every parameter and the result are `i32`.
    pub fn call(&self, args: &[i32]) -> Result<i32, JitError> {
        self.require_i32_signature()?;
        self.call_with_memory_bits(&widen(args), std::ptr::null_mut(), 0)
            .map(|bits| bits as u32 as i32)
    }

    /// Run the function on `i32` arguments with `memory` as its linear memory, and return its
    /// `i32` result.
    ///
    /// The function reads and writes `memory` in place, so a store made by the call is visible to
    /// the caller afterwards, as it is in Micro.
    pub fn call_with_memory(&self, args: &[i32], memory: &mut [u8]) -> Result<i32, JitError> {
        self.require_i32_signature()?;
        self.call_with_memory_bits(&widen(args), memory.as_mut_ptr(), memory.len() as u64)
            .map(|bits| bits as u32 as i32)
    }

    /// Run the function on integer arguments, given as `i64` (an `i32` parameter takes the low
    /// 32 bits), and return its `i64` result.
    ///
    /// Refused unless the result is `i64`.
    pub fn call_i64(&self, args: &[i64]) -> Result<i64, JitError> {
        self.require_result(ValueType::I64)?;
        self.call_with_memory_bits(args, std::ptr::null_mut(), 0)
            .map(|bits| bits as i64)
    }

    /// As [`call_i64`](Self::call_i64), with `memory` as the linear memory.
    pub fn call_i64_with_memory(&self, args: &[i64], memory: &mut [u8]) -> Result<i64, JitError> {
        self.require_result(ValueType::I64)?;
        self.call_with_memory_bits(args, memory.as_mut_ptr(), memory.len() as u64)
            .map(|bits| bits as i64)
    }

    /// Run the function on raw argument slots and return its raw result slot, for a caller
    /// that already holds values of mixed types. An `i32` parameter takes the low 32 bits;
    /// an `i32` result is in the low 32 bits of the returned value.
    ///
    /// `memory`, when given, is the linear memory the function reads and writes.
    pub fn call_raw(&self, args: &[i64], memory: Option<&mut [u8]>) -> Result<u64, JitError> {
        match memory {
            Some(memory) => {
                self.call_with_memory_bits(args, memory.as_mut_ptr(), memory.len() as u64)
            }
            None => self.call_with_memory_bits(args, std::ptr::null_mut(), 0),
        }
    }

    /// Refuse unless every parameter and the result are `i32`, which the `i32` calls assume.
    fn require_i32_signature(&self) -> Result<(), JitError> {
        self.require_result(ValueType::I32)?;
        if let Some(parameter) = self.params.iter().find(|p| **p != ValueType::I32) {
            return Err(JitError::Unsupported(format!(
                "a {parameter:?} parameter; use the i64 calls"
            )));
        }
        Ok(())
    }

    fn require_result(&self, expected: ValueType) -> Result<(), JitError> {
        if self.result != expected {
            return Err(JitError::Unsupported(format!(
                "the function returns {:?}, not {expected:?}",
                self.result
            )));
        }
        Ok(())
    }

    /// Run the code and return the raw result register, or the trap it reported.
    fn call_with_memory_bits(
        &self,
        args: &[i64],
        memory: *mut u8,
        memory_len: u64,
    ) -> Result<u64, JitError> {
        if args.len() != self.params.len() {
            return Err(JitError::ArityMismatch {
                expected: self.params.len(),
                actual: args.len(),
            });
        }
        let mut context = Context {
            memory,
            memory_len,
            status: 0,
        };
        let bits = self.code.call(args.as_ptr(), &mut context);
        match context.status as u32 {
            0 => Ok(bits),
            STATUS_DIVIDE_BY_ZERO => Err(JitError::Trap(Trap::IntegerDivisionByZero)),
            STATUS_OVERFLOW => Err(JitError::Trap(Trap::IntegerOverflow)),
            STATUS_MEMORY_OUT_OF_BOUNDS => Err(JitError::Trap(Trap::MemoryOutOfBounds)),
            other => Err(JitError::Unsupported(format!("an unknown status {other}"))),
        }
    }

    /// The generated machine code, for inspection and tests.
    pub fn code_bytes(&self) -> &[u8] {
        self.code.bytes()
    }

    /// The parameter types, in order.
    pub fn params(&self) -> &[ValueType] {
        &self.params
    }

    /// The result type.
    pub fn result(&self) -> ValueType {
        self.result
    }

    /// Install machine code that was generated earlier, for a function with this signature.
    ///
    /// This is the trust boundary of ahead-of-time artifacts. The bytes are not verified: they
    /// are executed exactly as given. The caller must have established where they came from and
    /// that they were generated for this target (see [`target_id`]), because loading code is the
    /// same as running it.
    pub fn from_machine_code(
        code: &[u8],
        params: Vec<ValueType>,
        result: ValueType,
    ) -> Result<Self, JitError> {
        Ok(Self {
            params,
            result,
            code: exec::ExecutableCode::new(code)?,
        })
    }
}

/// Widen `i32` arguments to the eight-byte slots the code reads.
fn widen(args: &[i32]) -> Vec<i64> {
    args.iter().map(|value| i64::from(*value)).collect()
}

impl fmt::Debug for CompiledFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledFunction")
            .field("params", &self.params)
            .field("result", &self.result)
            .field("code_len", &self.code.bytes().len())
            .finish()
    }
}

/// The name of the target that generated code runs on, or `"unsupported"` where there is no code
/// generator. An artifact records this, and loading it on another target is refused.
pub fn target_id() -> &'static str {
    if cfg!(all(target_arch = "x86_64", windows)) {
        "x86_64-windows"
    } else if cfg!(all(target_arch = "x86_64", unix)) {
        "x86_64-unix"
    } else if cfg!(all(target_arch = "aarch64", unix)) {
        "aarch64-unix"
    } else if cfg!(all(target_arch = "riscv64", unix)) {
        "riscv64-unix"
    } else {
        "unsupported"
    }
}

/// Whether this host can install and run generated code.
const HOST_CAN_RUN_CODE: bool = cfg!(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
));

/// Generate the machine code for this host's architecture.
fn encode_for_host(function: &IrFunction) -> Result<Vec<u8>, JitError> {
    if cfg!(target_arch = "aarch64") {
        aarch64::encode(function)
    } else if cfg!(target_arch = "riscv64") {
        riscv64::encode(function)
    } else {
        x86::encode(function, x86::Abi::host())
    }
}

/// Compile one IR function to machine code.
///
/// The function must be a verified function over integers that fits the subset described in the
/// crate documentation.
pub fn compile(function: &IrFunction) -> Result<CompiledFunction, JitError> {
    let params = function.function_type.params.0.clone();
    let result = function
        .function_type
        .results
        .0
        .first()
        .copied()
        .ok_or_else(|| JitError::Unsupported("a function with no result".into()))?;
    // Encoding is pure and runs everywhere, so a function outside the subset is refused the same
    // way on every target. Only installing the code needs the target.
    let code = encode_for_host(function)?;
    if !HOST_CAN_RUN_CODE {
        return Err(JitError::UnsupportedTarget);
    }
    let executable = exec::ExecutableCode::new(&code)?;
    Ok(CompiledFunction {
        params,
        result,
        code: executable,
    })
}
