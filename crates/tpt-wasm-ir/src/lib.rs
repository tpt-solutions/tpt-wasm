// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! TPT intermediate representation.
//!
//! The IR is deliberately independent of the Micro Interpreter.
//!
//! Wasm → ValidatedWasm → TPT-Wasm IR → { baseline, optimizing, verification }
//!
//! Design properties (targets):
//!   - SSA-ready
//!   - Explicit control flow graph
//!   - Typed values
//!   - Explicit memory operations
//!   - Explicit traps
//!   - Explicit calls
//!   - Explicit references
//!   - Explicit host boundaries
//!   - No machine registers in initial IR
//!
//! Status: M6 — not yet implemented.

use tpt_wasm_types::ValueType;

/// A function in the TPT IR.
#[derive(Debug)]
pub struct IrFunction {
    pub params: Vec<ValueType>,
    pub returns: Vec<ValueType>,
    pub blocks: Vec<BasicBlock>,
}

/// A basic block in the control flow graph.
#[derive(Debug)]
pub struct BasicBlock {
    pub id: BlockId,
    pub instrs: Vec<IrInstr>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId(pub u32);

/// IR instruction placeholder — will be expanded in M6.
#[derive(Debug)]
pub enum IrInstr {
    // TODO(M6): full typed IR instruction set
}

/// Basic block terminator.
#[derive(Debug)]
pub enum Terminator {
    Branch(BlockId),
    CondBranch { cond: ValueId, then_: BlockId, else_: BlockId },
    Return(Vec<ValueId>),
    Trap(tpt_wasm_types::Trap),
    Unreachable,
}
