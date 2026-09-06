// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

use tpt_wasm_types::{Trap, Value};
use crate::store::Store;

/// The complete state of the abstract machine.
#[derive(Debug)]
pub struct Machine {
    pub store: Store,
    pub operand_stack: Vec<Value>,
    pub frames: Vec<Frame>,
    pub control_stack: Vec<ControlFrame>,
}

/// An activation frame (function call frame).
#[derive(Debug)]
pub struct Frame {
    pub instance_idx: u32,
    pub func_idx: u32,
    pub locals: Vec<Value>,
    pub pc: usize,
    pub instrs: Vec<crate::instr::Instr>,
    pub return_arity: usize,
}

/// A control frame (block/loop/if scope).
#[derive(Debug)]
pub struct ControlFrame {
    pub kind: ControlKind,
    pub block_type_arity: usize,
    pub stack_height: usize,
    pub pc_else: Option<usize>,
    pub pc_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Block,
    Loop,
    If,
    Function,
}

/// The result of a single execution step.
#[derive(Debug)]
pub enum Step {
    Continue,
    Return(Vec<Value>),
    Trap(Trap),
    HostCall(HostCall),
}

/// A pending host function call.
#[derive(Debug)]
pub struct HostCall {
    pub func_name: String,
    pub args: Vec<Value>,
    pub expected_results: usize,
}

/// Execute a single step of the WebAssembly machine.
pub fn step(machine: &mut Machine) -> Step {
    // TODO(M2): implement full step semantics per WebAssembly Core spec §4
    let _ = machine;
    Step::Trap(Trap::HostFailure("interpreter not yet implemented".into()))
}
