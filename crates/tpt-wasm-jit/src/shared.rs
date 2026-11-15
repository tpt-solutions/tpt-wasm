// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Items the native backends share: the calling convention of the host, the check that a function is
//! inside the compiled subset, and the names of IR instructions and terminators for error messages.

use tpt_wasm_ir::IrInstr;
use tpt_wasm_types::Trap;

use crate::mir::{FloatUnaryOp, MirFunction, MirOp};
use crate::{
    JitError, STATUS_CALL_DEPTH, STATUS_DIVIDE_BY_ZERO, STATUS_INDIRECT_TYPE_MISMATCH,
    STATUS_INVALID_CONVERSION, STATUS_MEMORY_OUT_OF_BOUNDS, STATUS_NULL_REFERENCE, STATUS_OVERFLOW,
    STATUS_TABLE_OUT_OF_BOUNDS, STATUS_UNREACHABLE,
};

/// The calling convention whose argument registers the code reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Abi {
    /// System V AMD64: `rdi` and `rsi`, as used on Unix.
    SysV,
    /// Windows x64: `rcx` and `rdx`.
    Win64,
}

impl Abi {
    /// The convention of the target this crate was built for.
    pub(crate) fn host() -> Abi {
        if cfg!(windows) {
            Abi::Win64
        } else {
            Abi::SysV
        }
    }

    /// The register that carries the pointer to the arguments.
    pub(crate) fn args_register(self) -> u8 {
        match self {
            Abi::SysV => 7,  // rdi
            Abi::Win64 => 1, // rcx
        }
    }

    /// The register that carries the pointer to the context.
    pub(crate) fn context_register(self) -> u8 {
        match self {
            Abi::SysV => 6,  // rsi
            Abi::Win64 => 2, // rdx
        }
    }
}

/// The variant name of an instruction, for error messages.
pub(crate) fn instruction_name(instruction: &IrInstr) -> String {
    let text = format!("{instruction:?}");
    text.split([' ', '{']).next().unwrap_or_default().to_owned()
}

/// What the machine-IR encoders may assume about the module around a function.
pub(crate) struct Environment<'a> {
    /// Whether the caller reserves memory for `memory.grow`.
    pub(crate) memory_grow: bool,
    /// Whether each function of the module, by index, is compiled and may be called.
    pub(crate) callable: &'a [bool],
    /// Whether an indirect call may be compiled: the table is private to the module, and every
    /// slot it can select is callable.
    pub(crate) indirect: bool,
    /// The canonical signature id of each module type, by type index.
    pub(crate) type_ids: &'a [u64],
    /// Whether the code may read and write globals: the caller provides them as an array.
    pub(crate) globals: bool,
    /// Whether the code may call the host: the caller provides a host, and the trampoline for it.
    pub(crate) host: bool,
    /// Whether the host has SSE4.1, which the x86-64 rounding operations need. The AArch64 encoder
    /// rounds with its own instructions and passes `true`.
    pub(crate) sse41: bool,
}

/// Refuse a machine function that names something the environment does not provide: a grow without
/// reserved memory, a call the module does not compile, an indirect call the table cannot resolve, a
/// global without an array, a host call without a host, or an operation the host cannot run.
pub(crate) fn check_environment(function: &MirFunction, env: &Environment) -> Result<(), JitError> {
    let ops = || function.blocks.iter().flat_map(|block| block.ops.iter());
    if !env.memory_grow && ops().any(|op| matches!(op, MirOp::MemoryGrow { .. })) {
        return Err(JitError::Unsupported(
            "a memory.grow without reserved memory".into(),
        ));
    }
    for op in ops() {
        match op {
            MirOp::Call { function, .. } => {
                if !env
                    .callable
                    .get(*function as usize)
                    .copied()
                    .unwrap_or(false)
                {
                    return Err(JitError::Unsupported(
                        "a call to a function that is not compiled".into(),
                    ));
                }
            }
            MirOp::CallIndirect { type_index, .. } => {
                if !env.indirect {
                    return Err(JitError::Unsupported(
                        "an indirect call the native tier cannot resolve".into(),
                    ));
                }
                // The signature must be a module type whose id the code can compare against.
                if env
                    .type_ids
                    .get(*type_index as usize)
                    .and_then(|id| i32::try_from(*id).ok())
                    .is_none()
                {
                    return Err(JitError::Unsupported(
                        "an indirect call to an unknown type".into(),
                    ));
                }
            }
            MirOp::GlobalGet { index, .. } | MirOp::GlobalSet { index, .. } => {
                if !env.globals {
                    return Err(JitError::Unsupported(
                        "a global access without a globals array".into(),
                    ));
                }
                // The entry's byte offset, `8 * index`, must fit a 32-bit displacement.
                if i32::try_from(*index)
                    .ok()
                    .and_then(|index| index.checked_mul(8))
                    .is_none()
                {
                    return Err(JitError::Unsupported(
                        "a global index past the encodable range".into(),
                    ));
                }
            }
            MirOp::FloatUnary {
                op:
                    FloatUnaryOp::Ceil
                    | FloatUnaryOp::Floor
                    | FloatUnaryOp::Trunc
                    | FloatUnaryOp::Nearest,
                ..
            } if !env.sse41 => {
                return Err(JitError::Unsupported(
                    "a float rounding without SSE4.1".into(),
                ));
            }
            MirOp::FloatToInt { .. } if !env.sse41 => {
                return Err(JitError::Unsupported(
                    "a float truncation without SSE4.1".into(),
                ));
            }
            MirOp::CallHost { .. } if !env.host => {
                return Err(JitError::Unsupported("a host call without a host".into()));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The most words any call in the function passes in either direction, which sets how many entries
/// each of its two call areas needs.
pub(crate) fn widest_call(function: &MirFunction) -> u32 {
    function
        .blocks
        .iter()
        .flat_map(|block| block.ops.iter())
        .filter_map(|op| match op {
            // A callee with two or more results writes them over its argument block, so the block holds
            // whichever is more.
            MirOp::Call { args, dst, .. } | MirOp::CallIndirect { args, dst, .. } => {
                Some(args.len().max(dst.len()) as u32)
            }
            MirOp::CallHost { args, dsts, .. } => Some(args.len().max(dsts.len()) as u32),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// The status a trap is reported with, which the context carries back to the caller. A trap the native
/// code cannot raise (the step budget, which only the interpreter counts) has none.
pub(crate) fn trap_status(trap: &Trap) -> Option<u32> {
    Some(match trap {
        Trap::IntegerDivisionByZero => STATUS_DIVIDE_BY_ZERO,
        Trap::IntegerOverflow => STATUS_OVERFLOW,
        Trap::MemoryOutOfBounds => STATUS_MEMORY_OUT_OF_BOUNDS,
        Trap::CallDepthExceeded => STATUS_CALL_DEPTH,
        Trap::TableOutOfBounds => STATUS_TABLE_OUT_OF_BOUNDS,
        Trap::NullReference => STATUS_NULL_REFERENCE,
        Trap::IndirectCallTypeMismatch => STATUS_INDIRECT_TYPE_MISMATCH,
        Trap::InvalidConversion => STATUS_INVALID_CONVERSION,
        Trap::Unreachable => STATUS_UNREACHABLE,
        _ => return None,
    })
}
