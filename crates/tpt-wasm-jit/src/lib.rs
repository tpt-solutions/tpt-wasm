// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A JIT compiler for a subset of the TPT IR, generating machine code for x86-64, AArch64, and
//! RISC-V 64.
//!
//! ## What is compiled
//!
//! A function over `i32`, `i64`, `f32`, or `f64` values, returning one of them, whose body is made
//! of:
//!
//! - constants; reads and writes of parameters and declared locals (`local.get`, `local.set`,
//!   `local.tee`);
//! - the integer operations `add`, `sub`, `mul`, `and`, `or`, `xor`, the shifts `shl`, `shr_s`,
//!   and `shr_u` (counts are masked to the operand width, as in Wasm), division and remainder
//!   (signed and unsigned, with Wasm's traps), the comparisons, `eqz`, `select`, the integer
//!   width conversions (`wrap`, `extend`), and the sign extensions `extend8_s`, `extend16_s`,
//!   and `extend32_s`;
//! - the float operations `add`, `sub`, `mul`, `div`, `min`, `max`, `copysign`, `sqrt`, the
//!   rounding functions `ceil`, `floor`, `trunc`, and `nearest` (on x86-64 these need SSE4.1, and
//!   the target name records it), the comparisons, `neg`, and `abs`;
//! - the conversions between integers and floats, between the two float widths, and the bit
//!   reinterpretations; the float-to-integer truncations, which trap on a NaN, an infinity, or an
//!   out-of-range value;
//! - linear memory: loads and stores of all the integer widths and of both float widths, each
//!   checked against the memory's length;
//! - direct calls to other functions of the same module, when they are compiled with
//!   [`compile_module`], and indirect calls through the module's table, when that table is defined
//!   here and not exported, so that nothing outside the module can write it;
//! - control flow: unconditional and conditional branches, including loops, and a return.
//!
//! Each value lives in a stack slot, so each IR instruction is a load, at most one operation,
//! and a store. Anything else is refused with [`JitError::Unsupported`] rather than compiled
//! approximately: rotations, bit counting, calls through imported or exported tables, `memory.grow`, and a frame larger than the
//! encoder's limit are left to the interpreter or the baseline. A call to a function that is not
//! compiled refuses the caller too, so [`compile_module`] leaves that caller out as well.
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

use tpt_wasm_ir::{IrFunction, IrInstr, IrModule};
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
/// The status for a call that would go deeper than the call-depth limit.
pub(crate) const STATUS_CALL_DEPTH: u32 = 4;
/// The status for an indirect call whose index is past the end of the table.
pub(crate) const STATUS_TABLE_OUT_OF_BOUNDS: u32 = 5;
/// The status for an indirect call through a null slot.
pub(crate) const STATUS_NULL_REFERENCE: u32 = 6;
/// The status for an indirect call whose slot holds a function of another signature.
pub(crate) const STATUS_INDIRECT_TYPE_MISMATCH: u32 = 7;
/// The status for a float-to-integer truncation of a NaN, an infinity, or a value out of range.
pub(crate) const STATUS_INVALID_CONVERSION: u32 = 8;
/// The status for a global access past the end of the globals the caller provided. The code checks
/// this itself, so a module called without its globals traps instead of reading past them.
pub(crate) const STATUS_GLOBAL_OUT_OF_BOUNDS: u32 = 9;
/// The status for the `unreachable` instruction, which traps.
pub(crate) const STATUS_UNREACHABLE: u32 = 10;

/// What a compiled function reads and writes besides its arguments.
///
/// The layout is part of the calling convention, and the generated code reads and writes it at
/// fixed offsets: the memory base at 0, the memory length at 8, the trap status at 16, the call
/// table at 24, the active-frame count at 32, its limit at 40, the table's entries, types and length
/// at 48, 56 and 64, and the globals and their length at 72 and 80.
#[repr(C)]
pub(crate) struct Context {
    pub(crate) memory: *mut u8,
    pub(crate) memory_len: u64,
    pub(crate) status: u64,
    /// The entry points of the module's compiled functions, by function index. A compiled caller
    /// only calls an entry of a function that is compiled, so the table is never read past a null.
    pub(crate) functions: *const usize,
    /// The frames active now, counted as Micro counts them: the entry's frame included.
    pub(crate) depth: u64,
    /// The most frames that may be active at once. A call that would exceed it traps.
    pub(crate) depth_limit: u64,
    /// The module's table, by slot: the entry point of the function in each slot, zero for a null
    /// slot. Fixed for the life of the module, since nothing in the IR writes a table.
    pub(crate) table_entries: *const usize,
    /// The canonical signature id of each slot's function, or [`NULL_TYPE`] for a null slot.
    pub(crate) table_types: *const u64,
    /// The number of slots in the table.
    pub(crate) table_len: u64,
    /// The module's globals, by index, as raw 8-byte values: an `i32` in the low four bytes, a float
    /// as its bits. Null when the caller gave none.
    pub(crate) globals: *mut u64,
    /// The number of globals the caller provided. Compiled code checks an index against it before
    /// it touches `globals`.
    pub(crate) globals_len: u64,
}

impl Context {
    /// A context for a function that makes no calls, which needs no call table.
    fn without_calls(memory: *mut u8, memory_len: u64) -> Self {
        Self {
            memory,
            memory_len,
            status: 0,
            functions: std::ptr::null(),
            depth: 0,
            depth_limit: 0,
            table_entries: std::ptr::null(),
            table_types: std::ptr::null(),
            table_len: 0,
            globals: std::ptr::null_mut(),
            globals_len: 0,
        }
    }
}

/// The type id of a null table slot. No signature has this id, so a null slot never matches.
pub(crate) const NULL_TYPE: u64 = u64::MAX;

/// What a function's calls may name. A call is compiled only to a function listed as callable, and
/// an indirect call only when the table is private and every slot it can select is callable.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallEnv {
    /// Whether each function of the module, by index, is compiled and may be called.
    pub(crate) callable: Vec<bool>,
    /// The canonical signature id of each of the module's types, by type index: the index of the
    /// first type that equals it.
    pub(crate) type_ids: Vec<u64>,
    /// Whether an indirect call may be compiled: the module's table is private to it, and every
    /// slot names a callable function or is null.
    pub(crate) indirect: bool,
    /// Whether the code may read and write globals: the caller provides them as an array.
    pub(crate) globals: bool,
}

/// The result of a call in the IR: its one result, or none for a call to a function that returns nothing.
pub(crate) fn call_result(
    results: &[tpt_wasm_ir::ValueId],
) -> Result<Option<tpt_wasm_ir::ValueId>, JitError> {
    match results {
        [] => Ok(None),
        [result] => Ok(Some(*result)),
        _ => Err(JitError::Unsupported(
            "a call with more than one result".into(),
        )),
    }
}

/// The result of a call, given the return value and the status the code left in the context.
fn finish(bits: u64, status: u64) -> Result<u64, JitError> {
    match status as u32 {
        0 => Ok(bits),
        STATUS_DIVIDE_BY_ZERO => Err(JitError::Trap(Trap::IntegerDivisionByZero)),
        STATUS_OVERFLOW => Err(JitError::Trap(Trap::IntegerOverflow)),
        STATUS_MEMORY_OUT_OF_BOUNDS => Err(JitError::Trap(Trap::MemoryOutOfBounds)),
        STATUS_CALL_DEPTH => Err(JitError::Trap(Trap::CallDepthExceeded)),
        STATUS_TABLE_OUT_OF_BOUNDS => Err(JitError::Trap(Trap::TableOutOfBounds)),
        STATUS_NULL_REFERENCE => Err(JitError::Trap(Trap::NullReference)),
        STATUS_INDIRECT_TYPE_MISMATCH => Err(JitError::Trap(Trap::IndirectCallTypeMismatch)),
        STATUS_INVALID_CONVERSION => Err(JitError::Trap(Trap::InvalidConversion)),
        STATUS_UNREACHABLE => Err(JitError::Trap(Trap::Unreachable)),
        STATUS_GLOBAL_OUT_OF_BOUNDS => Err(JitError::Unsupported(
            "a global the call did not provide".into(),
        )),
        other => Err(JitError::Unsupported(format!("an unknown status {other}"))),
    }
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
    /// Parts of a compiled module are inconsistent with each other, so they cannot be installed.
    Invalid(String),
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
            JitError::Invalid(what) => write!(f, "invalid compiled module: {what}"),
            JitError::Trap(trap) => write!(f, "trap: {trap}"),
        }
    }
}

impl std::error::Error for JitError {}

/// A function compiled to machine code, ready to call.
pub struct CompiledFunction {
    params: Vec<ValueType>,
    result: Option<ValueType>,
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
        if self.result != Some(expected) {
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
        let mut context = Context::without_calls(memory, memory_len);
        let bits = self.code.call(args.as_ptr(), &mut context);
        finish(bits, context.status)
    }

    /// The generated machine code, for inspection and tests.
    pub fn code_bytes(&self) -> &[u8] {
        self.code.bytes()
    }

    /// The parameter types, in order.
    pub fn params(&self) -> &[ValueType] {
        &self.params
    }

    /// The result type, or `None` for a function that returns nothing.
    pub fn result(&self) -> Option<ValueType> {
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
        result: Option<ValueType>,
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

/// Whether this host has SSE4.1, which the x86-64 rounding functions need (`roundss` and
/// `roundsd`). Always false off x86-64.
fn host_has_sse41() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("sse4.1")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// The name of the target that generated code runs on, or `"unsupported"` where there is no code
/// generator. An artifact records this, and loading it on another target is refused.
///
/// On x86-64 the name carries `+sse4.1` when the host has SSE4.1. Code generated there may use
/// `roundss` and `roundsd`, which an older x86-64 CPU does not have, so an artifact from such a host
/// is refused on a host without it. The rule is conservative: an artifact that uses no rounding
/// still records the feature.
pub fn target_id() -> &'static str {
    let sse41 = host_has_sse41();
    if cfg!(all(target_arch = "x86_64", windows)) {
        if sse41 {
            "x86_64-windows+sse4.1"
        } else {
            "x86_64-windows"
        }
    } else if cfg!(all(target_arch = "x86_64", unix)) {
        if sse41 {
            "x86_64-unix+sse4.1"
        } else {
            "x86_64-unix"
        }
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

/// Generate the machine code for this host's architecture. `env` says which functions a call in
/// `function` may name, and whether an indirect call may be compiled; anything else is refused.
fn encode_for_host(function: &IrFunction, env: &CallEnv) -> Result<Vec<u8>, JitError> {
    if cfg!(target_arch = "aarch64") {
        aarch64::encode(function, env)
    } else if cfg!(target_arch = "riscv64") {
        riscv64::encode(function, env)
    } else {
        x86::encode(function, x86::Abi::host(), env, host_has_sse41())
    }
}

/// Compile one IR function to machine code.
///
/// The function must be a verified function over integers that fits the subset described in the
/// crate documentation.
pub fn compile(function: &IrFunction) -> Result<CompiledFunction, JitError> {
    let params = function.function_type.params.0.clone();
    let result = function.function_type.results.0.first().copied();
    // Encoding is pure and runs everywhere, so a function outside the subset is refused the same
    // way on every target. Only installing the code needs the target. With no callable functions,
    // a call in this function is refused: a call needs the module it belongs to.
    let code = encode_for_host(function, &CallEnv::default())?;
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

/// The native stack, in bytes, that a chain of nested calls may use. A thread's stack is one or
/// two megabytes, and the baseline's recursion and the runtime's own frames share it, so the
/// budget leaves at least half of the main thread's stack for them.
const NATIVE_CALL_STACK_BUDGET: usize = 512 * 1024;

/// Whether `function` contains a call, direct or indirect, and so needs the call table and the
/// depth check.
fn makes_calls(function: &IrFunction) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instrs)
        .any(|instruction| {
            matches!(
                instruction,
                IrInstr::Call { .. } | IrInstr::CallIndirect { .. }
            )
        })
}

/// An upper bound on the native stack one activation of `function` takes: a slot for each value,
/// the words a call reserves for its arguments, and a fixed header and shadow space.
fn frame_estimate(function: &IrFunction) -> usize {
    let argument_words: usize = function
        .blocks
        .iter()
        .flat_map(|block| &block.instrs)
        .map(|instruction| match instruction {
            IrInstr::Call { arguments, .. } | IrInstr::CallIndirect { arguments, .. } => {
                arguments.len().max(1)
            }
            _ => 0,
        })
        .sum();
    8 * (function.values.len() + argument_words + 4) + 96
}

/// The bytes of native stack one activation of `function` is estimated to take, from the IR alone.
///
/// The estimate is what the call-stack budget is checked against. [`CompiledModule::measured_stack_bytes`]
/// reads the same quantity from the code that was generated, so the two can be compared.
pub fn stack_estimate(function: &IrFunction) -> usize {
    frame_estimate(function)
}

/// The native stack one activation takes, read from the prologue of generated code for the host.
///
/// Each prologue reserves the frame and saves the frame pointer and return address, so the
/// activation takes that 16 bytes of saved registers plus the frame. On x86-64 the return address
/// is pushed by the call and the frame pointer by the prologue, which is the same 16 bytes.
fn measured_frame_bytes(code: &[u8]) -> Option<usize> {
    const SAVED: usize = 16;
    if cfg!(target_arch = "aarch64") {
        // stp; mov x29, sp; sub sp, sp, #size: the size is the third word's immediate.
        let word = u32::from_le_bytes(code.get(8..12)?.try_into().ok()?);
        Some(SAVED + ((word >> 10) & 0xFFF) as usize)
    } else if cfg!(target_arch = "riscv64") {
        // addi sp, sp, -16; sd s0; sd ra; addi s0, sp, 16; addi sp, sp, -size: the fifth word.
        let word = u32::from_le_bytes(code.get(16..20)?.try_into().ok()?);
        let immediate = (word as i32) >> 20;
        Some(SAVED + usize::try_from(-immediate).ok()?)
    } else {
        // push rbp; mov rbp, rsp; sub rsp, imm32: the immediate is at byte 7.
        let size = i32::from_le_bytes(code.get(7..11)?.try_into().ok()?);
        Some(SAVED + usize::try_from(size).ok()?)
    }
}

/// The functions of one module, compiled together so that a compiled function can call another.
///
/// Each function is compiled to its own code, and a call reaches its callee through the table of
/// entry points in the context, so no code is patched after it is installed. A function that
/// cannot be compiled, or that calls one that cannot, is left out; the caller of this type runs
/// those on another engine.
pub struct CompiledModule {
    functions: Vec<Option<CompiledFunction>>,
    /// The entry points by function index, zero where a function is not compiled. The table is
    /// boxed, so its address stays valid while the module lives.
    entries: Box<[usize]>,
    /// The module's table by slot: each slot's entry point and signature id. Empty when the
    /// module has no private table.
    table_entries: Box<[usize]>,
    table_types: Box<[u64]>,
    /// The function each slot names, `None` for a null slot. Kept beside the addresses so the module
    /// can be exported as parts and rebuilt from them.
    table_functions: Box<[Option<u32>]>,
    /// Whether the module's indirect calls are compiled, which fixes what the table must hold.
    indirect: bool,
    /// Whether the module has globals. Its functions need them supplied, through `call_with_globals`.
    globals: bool,
    max_call_depth: u64,
}

/// One compiled function in the form an artifact stores: its signature and its machine code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionParts {
    /// The parameter types, in order.
    pub params: Vec<ValueType>,
    /// The result type, or `None` for a function that returns nothing.
    pub result: Option<ValueType>,
    /// The machine code, which is installed exactly as it is. See [`CompiledModule::from_parts`].
    pub code: Vec<u8>,
}

/// One slot of a module's table, in the form an artifact stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSlotParts {
    /// The function index the slot names, or `None` for a null slot.
    pub function: Option<u32>,
    /// The canonical signature id of the slot's function, or the null id for a null slot.
    pub type_id: u64,
}

/// A compiled module in the form an artifact stores. [`CompiledModule::parts`] produces it and
/// [`CompiledModule::from_parts`] rebuilds the module from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleParts {
    /// Each function by index: its code, or `None` where it is not compiled.
    pub functions: Vec<Option<FunctionParts>>,
    /// The module's table, slot by slot. Empty when the module has no table to call through.
    pub table: Vec<TableSlotParts>,
    /// Whether indirect calls are compiled, and so every slot names a compiled function or is null.
    pub indirect: bool,
    /// The call-depth limit the calls were compiled against.
    pub max_call_depth: u64,
}

impl CompiledModule {
    /// The module in the form an artifact stores.
    pub fn parts(&self) -> ModuleParts {
        ModuleParts {
            functions: self
                .functions
                .iter()
                .map(|function| {
                    function.as_ref().map(|function| FunctionParts {
                        params: function.params.clone(),
                        result: function.result,
                        code: function.code.bytes().to_vec(),
                    })
                })
                .collect(),
            table: self
                .table_functions
                .iter()
                .zip(self.table_types.iter())
                .map(|(function, type_id)| TableSlotParts {
                    function: *function,
                    type_id: *type_id,
                })
                .collect(),
            indirect: self.indirect,
            max_call_depth: self.max_call_depth,
        }
    }

    /// Rebuild a module from its parts: install each function's code, then fill the table from the
    /// installed entries.
    ///
    /// This is the trust boundary of ahead-of-time modules. The code is installed as given, so the
    /// parts must come from a trusted store. What is checked here is that the parts are consistent:
    /// a table slot names a compiled function, or is null with the null id, and an indirect call
    /// needs every slot to name a compiled function.
    pub fn from_parts(parts: ModuleParts) -> Result<Self, JitError> {
        if !HOST_CAN_RUN_CODE {
            return Err(JitError::UnsupportedTarget);
        }
        let count = parts.functions.len();
        let mut functions: Vec<Option<CompiledFunction>> = Vec::with_capacity(count);
        let mut entries = vec![0usize; count];
        for (index, function) in parts.functions.into_iter().enumerate() {
            match function {
                Some(function) => {
                    let code = exec::ExecutableCode::new(&function.code)?;
                    entries[index] = code.address();
                    functions.push(Some(CompiledFunction {
                        params: function.params,
                        result: function.result,
                        code,
                    }));
                }
                None => functions.push(None),
            }
        }
        let mut table_entries = vec![0usize; parts.table.len()];
        let mut table_types = vec![NULL_TYPE; parts.table.len()];
        let mut table_functions = vec![None; parts.table.len()];
        for (slot, entry) in parts.table.iter().enumerate() {
            match entry.function {
                None => {
                    if entry.type_id != NULL_TYPE {
                        return Err(JitError::Invalid(format!(
                            "table slot {slot} is null but has a signature"
                        )));
                    }
                }
                Some(function) => {
                    let compiled = functions
                        .get(function as usize)
                        .is_some_and(Option::is_some);
                    if !compiled {
                        if parts.indirect {
                            return Err(JitError::Invalid(format!(
                                "table slot {slot} names function {function}, which is not compiled"
                            )));
                        }
                    } else {
                        table_entries[slot] = entries[function as usize];
                    }
                    table_types[slot] = entry.type_id;
                    table_functions[slot] = Some(function);
                }
            }
        }
        if parts.indirect && parts.table.is_empty() {
            return Err(JitError::Invalid(
                "indirect calls are compiled but the module has no table".into(),
            ));
        }
        Ok(CompiledModule {
            functions,
            entries: entries.into_boxed_slice(),
            table_entries: table_entries.into_boxed_slice(),
            table_types: table_types.into_boxed_slice(),
            table_functions: table_functions.into_boxed_slice(),
            indirect: parts.indirect,
            globals: false,
            max_call_depth: parts.max_call_depth,
        })
    }

    /// Whether the function at `index` (its position among the module's functions) is compiled.
    pub fn is_compiled(&self, index: usize) -> bool {
        self.functions.get(index).is_some_and(Option::is_some)
    }

    /// The native stack one activation of the compiled function at `index` takes, read from its
    /// generated prologue, or `None` when it is not compiled.
    pub fn measured_stack_bytes(&self, index: usize) -> Option<usize> {
        let function = self.functions.get(index)?.as_ref()?;
        measured_frame_bytes(function.code.bytes())
    }

    /// Run the compiled function at `index` on raw argument slots. `memory`, when given, is the
    /// linear memory the function and its callees read and write.
    ///
    /// Calls count against the module's call-depth limit, as Micro counts them, and a call past it
    /// traps with [`Trap::CallDepthExceeded`].
    pub fn call(
        &self,
        index: usize,
        args: &[i64],
        memory: Option<&mut [u8]>,
    ) -> Result<u64, JitError> {
        if self.globals {
            return Err(JitError::Unsupported(
                "a module with globals; use call_with_globals".into(),
            ));
        }
        self.call_inner(index, args, memory, &mut [])
    }

    /// As [`call`](Self::call), with `globals` as the module's globals by index, as raw 8-byte values:
    /// an `i32` in the low four bytes, a float as its bits. The code reads a global when it reads it
    /// and writes it when it sets it, so after the call `globals` holds what the call left, including
    /// after a trap.
    pub fn call_with_globals(
        &self,
        index: usize,
        args: &[i64],
        memory: Option<&mut [u8]>,
        globals: &mut [u64],
    ) -> Result<u64, JitError> {
        self.call_inner(index, args, memory, globals)
    }

    fn call_inner(
        &self,
        index: usize,
        args: &[i64],
        memory: Option<&mut [u8]>,
        globals: &mut [u64],
    ) -> Result<u64, JitError> {
        let function = self
            .functions
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| JitError::Unsupported("a function without native code".into()))?;
        if args.len() != function.params.len() {
            return Err(JitError::ArityMismatch {
                expected: function.params.len(),
                actual: args.len(),
            });
        }
        let (memory, memory_len) = match memory {
            Some(memory) => (memory.as_mut_ptr(), memory.len() as u64),
            None => (std::ptr::null_mut(), 0),
        };
        let mut context = Context {
            memory,
            memory_len,
            status: 0,
            functions: self.entries.as_ptr(),
            // The entry's own frame is active when the call starts.
            depth: 1,
            depth_limit: self.max_call_depth,
            table_entries: self.table_entries.as_ptr(),
            table_types: self.table_types.as_ptr(),
            table_len: self.table_types.len() as u64,
            globals: globals.as_mut_ptr(),
            globals_len: globals.len() as u64,
        };
        let bits = function.code.call(args.as_ptr(), &mut context);
        finish(bits, context.status)
    }
}

/// Compile the functions of a module, with the calls between them.
///
/// A function is compiled when its own code is supported and every function it calls is compiled.
/// Those two conditions depend on each other across recursion, so the set is found as a greatest
/// fixed point: start with every function assumed compiled, and drop any that fails until nothing
/// changes. Mutually recursive functions that are otherwise supported stay compiled together.
///
/// `max_call_depth` is the most frames that may be active at once, as in the runtime's limits.
pub fn compile_module(
    module: &IrModule,
    max_call_depth: u64,
    table_private: bool,
) -> CompiledModule {
    let functions = &module.functions;
    let mut callable = vec![HOST_CAN_RUN_CODE; functions.len()];

    // The table: the one table of an MVP module, when the module defines it. Its slots are the
    // declared size, with each active element written at its offset and every other slot null.
    let table = module
        .tables
        .first()
        .filter(|table| table.import.is_none())
        .filter(|table| table.min <= 1 << 20);
    let slots: Vec<Option<u32>> = match table {
        Some(table) => {
            let mut slots = vec![None; table.min as usize];
            for (offset, element) in table.elements.iter().enumerate() {
                if let Some(slot) = slots.get_mut(table.offset as usize + offset) {
                    *slot = *element;
                }
            }
            slots
        }
        None => Vec::new(),
    };
    // A slot's signature id is the id of the first module type equal to its function's type. A
    // function whose type is not among the module's types matches no call.
    let type_ids: Vec<u64> = module
        .types
        .iter()
        .map(|kind| {
            module
                .types
                .iter()
                .position(|other| other == kind)
                .unwrap_or(0) as u64
        })
        .collect();
    // An indirect call is compiled only when no other instance can write the table, and every
    // slot it can select is a function compiled in this module.
    let env_for = |callable: &[bool]| CallEnv {
        callable: callable.to_vec(),
        type_ids: type_ids.clone(),
        globals: !module.globals.is_empty(),
        indirect: table.is_some()
            && table_private
            && slots.iter().all(|slot| match slot {
                None => true,
                Some(element) => callable.get(*element as usize).copied().unwrap_or(false),
            }),
    };
    // Nested calls use the native stack, a frame per active call, so a chain of them is as deep as
    // the call-depth limit. When the deepest such chain could outgrow the stack budget, the
    // functions that call are left out, and the baseline runs them. A leaf adds one frame, so the
    // leaves stay compiled.
    let largest_caller = functions
        .iter()
        .filter(|function| makes_calls(function))
        .map(frame_estimate)
        .max()
        .unwrap_or(0);
    let largest_frame = functions.iter().map(frame_estimate).max().unwrap_or(0);
    let chain = (max_call_depth as usize)
        .saturating_mul(largest_caller)
        .saturating_add(largest_frame);
    if chain > NATIVE_CALL_STACK_BUDGET {
        for (index, function) in functions.iter().enumerate() {
            if makes_calls(function) {
                callable[index] = false;
            }
        }
    }
    loop {
        let mut changed = false;
        for (index, function) in functions.iter().enumerate() {
            if callable[index] && encode_for_host(function, &env_for(&callable)).is_err() {
                callable[index] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut compiled: Vec<Option<CompiledFunction>> = Vec::with_capacity(functions.len());
    let mut entries = vec![0usize; functions.len()];
    let mut complete = true;
    for (index, function) in functions.iter().enumerate() {
        if !callable[index] {
            compiled.push(None);
            continue;
        }
        // Installation can fail where the encoder succeeded. A caller of this function was
        // compiled to call it, so a missing callee leaves the whole module without native code.
        let installed = encode_for_host(function, &env_for(&callable))
            .ok()
            .and_then(|code| exec::ExecutableCode::new(&code).ok());
        match installed {
            Some(code) => {
                entries[index] = code.address();
                compiled.push(Some(CompiledFunction {
                    params: function.function_type.params.0.clone(),
                    result: function.function_type.results.0.first().copied(),
                    code,
                }));
            }
            None => {
                complete = false;
                compiled.push(None);
            }
        }
    }
    if !complete {
        return CompiledModule {
            functions: functions.iter().map(|_| None).collect(),
            entries: vec![0usize; functions.len()].into_boxed_slice(),
            table_entries: Box::new([]),
            table_types: Box::new([]),
            table_functions: Box::new([]),
            indirect: false,
            globals: !module.globals.is_empty(),
            max_call_depth,
        };
    }
    // Each slot's entry is the installed entry of its function, and its signature id. A null slot
    // has neither, and the call sequence tests for null before it uses either.
    let mut table_entries = vec![0usize; slots.len()];
    let mut table_types = vec![NULL_TYPE; slots.len()];
    for (slot, element) in slots.iter().enumerate() {
        if let Some(element) = element {
            let element = *element as usize;
            table_entries[slot] = entries.get(element).copied().unwrap_or(0);
            table_types[slot] = functions.get(element).map_or(MISMATCHED_TYPE, |function| {
                module
                    .types
                    .iter()
                    .position(|kind| *kind == function.function_type)
                    .map_or(MISMATCHED_TYPE, |id| id as u64)
            });
        }
    }
    let table_functions: Vec<Option<u32>> = slots.to_vec();
    CompiledModule {
        functions: compiled,
        entries: entries.into_boxed_slice(),
        table_entries: table_entries.into_boxed_slice(),
        table_types: table_types.into_boxed_slice(),
        table_functions: table_functions.into_boxed_slice(),
        indirect: env_for(&callable).indirect,
        globals: !module.globals.is_empty(),
        max_call_depth,
    }
}

/// The signature id of a slot whose function's type is not among the module's types. It is not an
/// id any call names, so such a slot never matches.
const MISMATCHED_TYPE: u64 = u64::MAX - 1;
