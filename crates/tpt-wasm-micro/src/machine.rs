// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

use crate::execution::{DeterministicRng, ExecutionConfig};
use crate::instr::decode_body;
use crate::store::Store;
use tpt_wasm_types::{RefValue, Trap, Value, ValueType};

/// The complete state of the abstract machine.
#[derive(Debug)]
pub struct Machine {
    pub store: Store,
    pub operand_stack: Vec<Value>,
    pub frames: Vec<Frame>,
    pub control_stack: Vec<ControlFrame>,
    pub steps: u64,
    pub pending_host_results: Option<usize>,
    pub execution: ExecutionConfig,
    rng: DeterministicRng,
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
    pub control_base: usize,
    pub control_targets: Vec<ControlTarget>,
}

impl Frame {
    pub fn new(
        instance_idx: u32,
        func_idx: u32,
        locals: Vec<Value>,
        instrs: Vec<crate::instr::Instr>,
        return_arity: usize,
    ) -> Self {
        let control_targets = build_control_targets(&instrs);
        Self {
            instance_idx,
            func_idx,
            locals,
            pc: 0,
            instrs,
            return_arity,
            control_base: 0,
            control_targets,
        }
    }

    /// Prepare a frame from a function body that has already passed validation.
    pub fn from_body(
        instance_idx: u32,
        func_idx: u32,
        locals: Vec<Value>,
        body: &[u8],
        return_arity: usize,
    ) -> Result<Self, crate::instr::InstrDecodeError> {
        Ok(Self::new(
            instance_idx,
            func_idx,
            locals,
            decode_body(body)?,
            return_arity,
        ))
    }
}

fn build_control_targets(instrs: &[crate::instr::Instr]) -> Vec<ControlTarget> {
    let mut targets = vec![
        ControlTarget {
            else_pc: None,
            end_pc: 0
        };
        instrs.len()
    ];
    let mut stack = Vec::new();
    for (pc, instr) in instrs.iter().enumerate() {
        match instr {
            crate::instr::Instr::Block(_)
            | crate::instr::Instr::Loop(_)
            | crate::instr::Instr::If(_) => stack.push(pc),
            crate::instr::Instr::Else => {
                if let Some(start) = stack.last().copied() {
                    targets[start].else_pc = Some(pc);
                }
            }
            crate::instr::Instr::End => {
                if let Some(start) = stack.pop() {
                    targets[start].end_pc = pc;
                }
            }
            _ => {}
        }
    }
    targets
}

/// A control frame (block/loop/if scope).
#[derive(Debug, Clone)]
pub struct ControlFrame {
    pub kind: ControlKind,
    pub block_type_arity: usize,
    pub stack_height: usize,
    pub pc_else: Option<usize>,
    pub pc_end: usize,
    pub pc_start: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlTarget {
    pub else_pc: Option<usize>,
    pub end_pc: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Block,
    Loop,
    If,
    Function,
}

/// The result of a single execution step.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    Return(Vec<Value>),
    Trap(Trap),
    HostCall(HostCall),
}

/// A pending host function call.
#[derive(Debug, PartialEq, Eq)]
pub struct HostCall {
    pub func_name: String,
    pub args: Vec<Value>,
    pub expected_results: usize,
}

/// Execute a single step of the WebAssembly machine.
pub fn step(machine: &mut Machine) -> Step {
    if let Some(limit) = machine.store.limits.max_execution_steps {
        if machine.steps >= limit {
            return Step::Trap(Trap::StepsExhausted);
        }
    }
    let Some(frame) = machine.frames.last_mut() else {
        return Step::Trap(Trap::HostFailure("no active frame".into()));
    };
    let Some(instruction) = frame.instrs.get(frame.pc).cloned() else {
        return Step::Trap(Trap::HostFailure("program counter out of bounds".into()));
    };
    machine.steps = machine.steps.saturating_add(1);
    machine.frames.last_mut().expect("frame remains present").pc += 1;
    execute_instruction(machine, instruction)
}

fn execute_instruction(machine: &mut Machine, instruction: crate::instr::Instr) -> Step {
    match instruction {
        crate::instr::Instr::Unreachable => Step::Trap(Trap::Unreachable),
        crate::instr::Instr::Nop => Step::Continue,
        crate::instr::Instr::Block(block_type) => {
            enter_block(machine, ControlKind::Block, block_type)
        }
        crate::instr::Instr::Loop(block_type) => {
            enter_block(machine, ControlKind::Loop, block_type)
        }
        crate::instr::Instr::If(block_type) => enter_if(machine, block_type),
        crate::instr::Instr::Else => skip_else(machine),
        crate::instr::Instr::Br(depth) => branch(machine, depth),
        crate::instr::Instr::BrIf(depth) => branch_if(machine, depth),
        crate::instr::Instr::BrTable(table) => branch_table(machine, table),
        crate::instr::Instr::Call(index) => call_function(machine, index),
        crate::instr::Instr::CallIndirect(type_index, table_index) => {
            call_indirect(machine, type_index, table_index)
        }
        crate::instr::Instr::Return => return_frame(machine),
        crate::instr::Instr::Drop => match pop_value(machine) {
            Ok(_) => Step::Continue,
            Err(step) => step,
        },
        crate::instr::Instr::Select => select_value(machine),
        crate::instr::Instr::RefNull(kind) => push_value(machine, Value::Ref(RefValue::Null(kind))),
        crate::instr::Instr::RefIsNull => ref_is_null(machine),
        crate::instr::Instr::RefFunc(index) => {
            let addresses = machine
                .frames
                .last()
                .and_then(|frame| machine.store.instance(frame.instance_idx).ok())
                .map(|instance| instance.func_addrs.clone())
                .unwrap_or_default();
            match instance_address(machine, &addresses, index, "function") {
                Ok(address) => push_value(machine, Value::Ref(RefValue::FuncRef(address))),
                Err(step) => step,
            }
        }
        crate::instr::Instr::I32Const(value) => push_value(machine, Value::I32(value)),
        crate::instr::Instr::I64Const(value) => push_value(machine, Value::I64(value)),
        crate::instr::Instr::F32Const(value) => push_value(machine, Value::F32(value)),
        crate::instr::Instr::F64Const(value) => push_value(machine, Value::F64(value)),
        crate::instr::Instr::LocalGet(index) => local_get(machine, index),
        crate::instr::Instr::LocalSet(index) => local_set(machine, index),
        crate::instr::Instr::LocalTee(index) => local_tee(machine, index),
        crate::instr::Instr::GlobalGet(index) => global_get(machine, index),
        crate::instr::Instr::GlobalSet(index) => global_set(machine, index),
        crate::instr::Instr::I32Load(arg) => load_i32(machine, arg),
        crate::instr::Instr::I64Load(arg) => load_i64(machine, arg),
        crate::instr::Instr::F32Load(arg) => load_f32(machine, arg),
        crate::instr::Instr::F64Load(arg) => load_f64(machine, arg),
        crate::instr::Instr::I32Load8s(arg) => load_i32_small(machine, arg, 1, true),
        crate::instr::Instr::I32Load8u(arg) => load_i32_small(machine, arg, 1, false),
        crate::instr::Instr::I32Load16s(arg) => load_i32_small(machine, arg, 2, true),
        crate::instr::Instr::I32Load16u(arg) => load_i32_small(machine, arg, 2, false),
        crate::instr::Instr::I64Load8s(arg) => load_i64_small(machine, arg, 1, true),
        crate::instr::Instr::I64Load8u(arg) => load_i64_small(machine, arg, 1, false),
        crate::instr::Instr::I64Load16s(arg) => load_i64_small(machine, arg, 2, true),
        crate::instr::Instr::I64Load16u(arg) => load_i64_small(machine, arg, 2, false),
        crate::instr::Instr::I64Load32s(arg) => load_i64_small(machine, arg, 4, true),
        crate::instr::Instr::I64Load32u(arg) => load_i64_small(machine, arg, 4, false),
        crate::instr::Instr::I32Store(arg) => store_i32(machine, arg, 4),
        crate::instr::Instr::I64Store(arg) => store_i64(machine, arg, 8),
        crate::instr::Instr::F32Store(arg) => store_f32(machine, arg),
        crate::instr::Instr::F64Store(arg) => store_f64(machine, arg),
        crate::instr::Instr::I32Store8(arg) => store_i32(machine, arg, 1),
        crate::instr::Instr::I32Store16(arg) => store_i32(machine, arg, 2),
        crate::instr::Instr::I64Store8(arg) => store_i64(machine, arg, 1),
        crate::instr::Instr::I64Store16(arg) => store_i64(machine, arg, 2),
        crate::instr::Instr::I64Store32(arg) => store_i64(machine, arg, 4),
        crate::instr::Instr::MemorySize(index) => memory_size(machine, index),
        crate::instr::Instr::MemoryGrow(index) => memory_grow(machine, index),
        crate::instr::Instr::I32Eqz => unary_i32(machine, |value| (value == 0) as i32),
        crate::instr::Instr::I32Eq => compare_i32(machine, |left, right| left == right),
        crate::instr::Instr::I32Ne => compare_i32(machine, |left, right| left != right),
        crate::instr::Instr::I32LtS => compare_i32(machine, |left, right| left < right),
        crate::instr::Instr::I32LtU => {
            compare_i32(machine, |left, right| (left as u32) < (right as u32))
        }
        crate::instr::Instr::I32GtS => compare_i32(machine, |left, right| left > right),
        crate::instr::Instr::I32GtU => {
            compare_i32(machine, |left, right| (left as u32) > (right as u32))
        }
        crate::instr::Instr::I32LeS => compare_i32(machine, |left, right| left <= right),
        crate::instr::Instr::I32LeU => {
            compare_i32(machine, |left, right| (left as u32) <= (right as u32))
        }
        crate::instr::Instr::I32GeS => compare_i32(machine, |left, right| left >= right),
        crate::instr::Instr::I32GeU => {
            compare_i32(machine, |left, right| (left as u32) >= (right as u32))
        }
        crate::instr::Instr::I32Clz => unary_i32(machine, |value| value.leading_zeros() as i32),
        crate::instr::Instr::I32Ctz => unary_i32(machine, |value| value.trailing_zeros() as i32),
        crate::instr::Instr::I32Popcnt => unary_i32(machine, |value| value.count_ones() as i32),
        crate::instr::Instr::I32Add => binary_i32(machine, |left, right| left.wrapping_add(right)),
        crate::instr::Instr::I32Sub => binary_i32(machine, |left, right| left.wrapping_sub(right)),
        crate::instr::Instr::I32Mul => binary_i32(machine, |left, right| left.wrapping_mul(right)),
        crate::instr::Instr::I32DivS => div_i32_s(machine),
        crate::instr::Instr::I32DivU => div_i32_u(machine),
        crate::instr::Instr::I32RemS => rem_i32_s(machine),
        crate::instr::Instr::I32RemU => rem_i32_u(machine),
        crate::instr::Instr::I32And => binary_i32(machine, |left, right| left & right),
        crate::instr::Instr::I32Or => binary_i32(machine, |left, right| left | right),
        crate::instr::Instr::I32Xor => binary_i32(machine, |left, right| left ^ right),
        crate::instr::Instr::I32Shl => {
            binary_i32(machine, |left, right| left.wrapping_shl(right as u32))
        }
        crate::instr::Instr::I32ShrS => {
            binary_i32(machine, |left, right| left.wrapping_shr(right as u32))
        }
        crate::instr::Instr::I32ShrU => binary_i32(machine, |left, right| {
            ((left as u32).wrapping_shr(right as u32)) as i32
        }),
        crate::instr::Instr::I32Rotl => {
            binary_i32(machine, |left, right| left.rotate_left((right as u32) & 31))
        }
        crate::instr::Instr::I32Rotr => binary_i32(machine, |left, right| {
            left.rotate_right((right as u32) & 31)
        }),
        crate::instr::Instr::I64Eqz => unary_i64_to_i32(machine, |value| (value == 0) as i32),
        crate::instr::Instr::I64Eq => compare_i64(machine, |left, right| left == right),
        crate::instr::Instr::I64Ne => compare_i64(machine, |left, right| left != right),
        crate::instr::Instr::I64LtS => compare_i64(machine, |left, right| left < right),
        crate::instr::Instr::I64LtU => {
            compare_i64(machine, |left, right| (left as u64) < (right as u64))
        }
        crate::instr::Instr::I64GtS => compare_i64(machine, |left, right| left > right),
        crate::instr::Instr::I64GtU => {
            compare_i64(machine, |left, right| (left as u64) > (right as u64))
        }
        crate::instr::Instr::I64LeS => compare_i64(machine, |left, right| left <= right),
        crate::instr::Instr::I64LeU => {
            compare_i64(machine, |left, right| (left as u64) <= (right as u64))
        }
        crate::instr::Instr::I64GeS => compare_i64(machine, |left, right| left >= right),
        crate::instr::Instr::I64GeU => {
            compare_i64(machine, |left, right| (left as u64) >= (right as u64))
        }
        crate::instr::Instr::I64Clz => unary_i64(machine, |value| value.leading_zeros() as i64),
        crate::instr::Instr::I64Ctz => unary_i64(machine, |value| value.trailing_zeros() as i64),
        crate::instr::Instr::I64Popcnt => unary_i64(machine, |value| value.count_ones() as i64),
        crate::instr::Instr::I64Add => binary_i64(machine, |left, right| left.wrapping_add(right)),
        crate::instr::Instr::I64Sub => binary_i64(machine, |left, right| left.wrapping_sub(right)),
        crate::instr::Instr::I64Mul => binary_i64(machine, |left, right| left.wrapping_mul(right)),
        crate::instr::Instr::I64DivS => div_i64_s(machine),
        crate::instr::Instr::I64DivU => div_i64_u(machine),
        crate::instr::Instr::I64RemS => rem_i64_s(machine),
        crate::instr::Instr::I64RemU => rem_i64_u(machine),
        crate::instr::Instr::I64And => binary_i64(machine, |left, right| left & right),
        crate::instr::Instr::I64Or => binary_i64(machine, |left, right| left | right),
        crate::instr::Instr::I64Xor => binary_i64(machine, |left, right| left ^ right),
        crate::instr::Instr::I64Shl => {
            binary_i64(machine, |left, right| left.wrapping_shl(right as u32))
        }
        crate::instr::Instr::I64ShrS => {
            binary_i64(machine, |left, right| left.wrapping_shr(right as u32))
        }
        crate::instr::Instr::I64ShrU => binary_i64(machine, |left, right| {
            ((left as u64).wrapping_shr(right as u32)) as i64
        }),
        crate::instr::Instr::I64Rotl => {
            binary_i64(machine, |left, right| left.rotate_left((right as u32) & 63))
        }
        crate::instr::Instr::I64Rotr => binary_i64(machine, |left, right| {
            left.rotate_right((right as u32) & 63)
        }),
        crate::instr::Instr::F32Eq => compare_f32(machine, |left, right| left == right),
        crate::instr::Instr::F32Ne => compare_f32(machine, |left, right| left != right),
        crate::instr::Instr::F32Lt => compare_f32(machine, |left, right| left < right),
        crate::instr::Instr::F32Gt => compare_f32(machine, |left, right| left > right),
        crate::instr::Instr::F32Le => compare_f32(machine, |left, right| left <= right),
        crate::instr::Instr::F32Ge => compare_f32(machine, |left, right| left >= right),
        crate::instr::Instr::F32Abs => unary_f32_preserving_nan(machine, |value| {
            f32::from_bits(value.to_bits() & 0x7fff_ffff)
        }),
        crate::instr::Instr::F32Neg => unary_f32_preserving_nan(machine, |value| {
            f32::from_bits(value.to_bits() ^ 0x8000_0000)
        }),
        crate::instr::Instr::F32Ceil => unary_f32(machine, f32::ceil),
        crate::instr::Instr::F32Floor => unary_f32(machine, f32::floor),
        crate::instr::Instr::F32Trunc => unary_f32(machine, f32::trunc),
        crate::instr::Instr::F32Nearest => unary_f32(machine, f32::round_ties_even),
        crate::instr::Instr::F32Sqrt => unary_f32(machine, f32::sqrt),
        crate::instr::Instr::F32Add => binary_f32(machine, |left, right| left + right),
        crate::instr::Instr::F32Sub => binary_f32(machine, |left, right| left - right),
        crate::instr::Instr::F32Mul => binary_f32(machine, |left, right| left * right),
        crate::instr::Instr::F32Div => binary_f32(machine, |left, right| left / right),
        crate::instr::Instr::F32Min => binary_f32(machine, f32_min),
        crate::instr::Instr::F32Max => binary_f32(machine, f32_max),
        crate::instr::Instr::F32Copysign => binary_f32_preserving_nan(machine, f32::copysign),
        crate::instr::Instr::F64Eq => compare_f64(machine, |left, right| left == right),
        crate::instr::Instr::F64Ne => compare_f64(machine, |left, right| left != right),
        crate::instr::Instr::F64Lt => compare_f64(machine, |left, right| left < right),
        crate::instr::Instr::F64Gt => compare_f64(machine, |left, right| left > right),
        crate::instr::Instr::F64Le => compare_f64(machine, |left, right| left <= right),
        crate::instr::Instr::F64Ge => compare_f64(machine, |left, right| left >= right),
        crate::instr::Instr::F64Abs => unary_f64_preserving_nan(machine, |value| {
            f64::from_bits(value.to_bits() & 0x7fff_ffff_ffff_ffff)
        }),
        crate::instr::Instr::F64Neg => unary_f64_preserving_nan(machine, |value| {
            f64::from_bits(value.to_bits() ^ 0x8000_0000_0000_0000)
        }),
        crate::instr::Instr::F64Ceil => unary_f64(machine, f64::ceil),
        crate::instr::Instr::F64Floor => unary_f64(machine, f64::floor),
        crate::instr::Instr::F64Trunc => unary_f64(machine, f64::trunc),
        crate::instr::Instr::F64Nearest => unary_f64(machine, f64::round_ties_even),
        crate::instr::Instr::F64Sqrt => unary_f64(machine, f64::sqrt),
        crate::instr::Instr::F64Add => binary_f64(machine, |left, right| left + right),
        crate::instr::Instr::F64Sub => binary_f64(machine, |left, right| left - right),
        crate::instr::Instr::F64Mul => binary_f64(machine, |left, right| left * right),
        crate::instr::Instr::F64Div => binary_f64(machine, |left, right| left / right),
        crate::instr::Instr::F64Min => binary_f64(machine, f64_min),
        crate::instr::Instr::F64Max => binary_f64(machine, f64_max),
        crate::instr::Instr::F64Copysign => binary_f64_preserving_nan(machine, f64::copysign),
        crate::instr::Instr::I32WrapI64 => convert_i64_to_i32(machine),
        crate::instr::Instr::I32TruncF32S => trunc_f32_to_i32(machine, true),
        crate::instr::Instr::I32TruncF32U => trunc_f32_to_i32(machine, false),
        crate::instr::Instr::I32TruncF64S => trunc_f64_to_i32(machine, true),
        crate::instr::Instr::I32TruncF64U => trunc_f64_to_i32(machine, false),
        crate::instr::Instr::I64ExtendI32S => convert_i32_to_i64(machine, true),
        crate::instr::Instr::I64ExtendI32U => convert_i32_to_i64(machine, false),
        crate::instr::Instr::I64TruncF32S => trunc_f32_to_i64(machine, true),
        crate::instr::Instr::I64TruncF32U => trunc_f32_to_i64(machine, false),
        crate::instr::Instr::I64TruncF64S => trunc_f64_to_i64(machine, true),
        crate::instr::Instr::I64TruncF64U => trunc_f64_to_i64(machine, false),
        crate::instr::Instr::F32ConvertI32S => convert_i32_to_f32(machine, true),
        crate::instr::Instr::F32ConvertI32U => convert_i32_to_f32(machine, false),
        crate::instr::Instr::F32ConvertI64S => convert_i64_to_f32(machine, true),
        crate::instr::Instr::F32ConvertI64U => convert_i64_to_f32(machine, false),
        crate::instr::Instr::F32DemoteF64 => convert_f64_to_f32(machine),
        crate::instr::Instr::F64ConvertI32S => convert_i32_to_f64(machine, true),
        crate::instr::Instr::F64ConvertI32U => convert_i32_to_f64(machine, false),
        crate::instr::Instr::F64ConvertI64S => convert_i64_to_f64(machine, true),
        crate::instr::Instr::F64ConvertI64U => convert_i64_to_f64(machine, false),
        crate::instr::Instr::F64PromoteF32 => convert_f32_to_f64(machine),
        crate::instr::Instr::I32ReinterpretF32 => reinterpret_f32_to_i32(machine),
        crate::instr::Instr::I64ReinterpretF64 => reinterpret_f64_to_i64(machine),
        crate::instr::Instr::F32ReinterpretI32 => reinterpret_i32_to_f32(machine),
        crate::instr::Instr::F64ReinterpretI64 => reinterpret_i64_to_f64(machine),
        crate::instr::Instr::I32Extend8S => unary_i32(machine, |value| (value as i8) as i32),
        crate::instr::Instr::I32Extend16S => unary_i32(machine, |value| (value as i16) as i32),
        crate::instr::Instr::I64Extend8S => unary_i64(machine, |value| (value as i8) as i64),
        crate::instr::Instr::I64Extend16S => unary_i64(machine, |value| (value as i16) as i64),
        crate::instr::Instr::I64Extend32S => unary_i64(machine, |value| (value as i32) as i64),
        crate::instr::Instr::End => end_control(machine),
    }
}

fn end_control(machine: &mut Machine) -> Step {
    match machine.control_stack.last() {
        Some(control) if control.kind != ControlKind::Function => {
            machine.control_stack.pop();
            Step::Continue
        }
        Some(_) | None => return_frame(machine),
    }
}

fn block_arity(block_type: &crate::instr::BlockType) -> usize {
    match block_type {
        crate::instr::BlockType::Empty => 0,
        crate::instr::BlockType::Value(_) => 1,
        crate::instr::BlockType::FunctionType(_) => 0,
    }
}

fn enter_block(
    machine: &mut Machine,
    kind: ControlKind,
    block_type: crate::instr::BlockType,
) -> Step {
    let start = machine
        .frames
        .last()
        .map_or(0, |frame| frame.pc.saturating_sub(1));
    let Some(target) = machine
        .frames
        .last()
        .and_then(|frame| frame.control_targets.get(start))
        .copied()
    else {
        return Step::Trap(Trap::HostFailure("missing control target".into()));
    };
    machine.control_stack.push(ControlFrame {
        kind,
        block_type_arity: block_arity(&block_type),
        stack_height: machine.operand_stack.len(),
        pc_else: target.else_pc,
        pc_end: target.end_pc,
        pc_start: start,
    });
    Step::Continue
}

fn enter_if(machine: &mut Machine, block_type: crate::instr::BlockType) -> Step {
    let condition = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let start = machine
        .frames
        .last()
        .map_or(0, |frame| frame.pc.saturating_sub(1));
    let Some(target) = machine
        .frames
        .last()
        .and_then(|frame| frame.control_targets.get(start))
        .copied()
    else {
        return Step::Trap(Trap::HostFailure("missing if target".into()));
    };
    if condition != 0 {
        machine.control_stack.push(ControlFrame {
            kind: ControlKind::If,
            block_type_arity: block_arity(&block_type),
            stack_height: machine.operand_stack.len(),
            pc_else: target.else_pc,
            pc_end: target.end_pc,
            pc_start: start,
        });
    } else if target.else_pc.is_some() {
        machine.control_stack.push(ControlFrame {
            kind: ControlKind::If,
            block_type_arity: block_arity(&block_type),
            stack_height: machine.operand_stack.len(),
            pc_else: target.else_pc,
            pc_end: target.end_pc,
            pc_start: start,
        });
        if let Some(frame) = machine.frames.last_mut() {
            frame.pc = target.else_pc.unwrap_or(target.end_pc).saturating_add(1);
        }
    } else if let Some(frame) = machine.frames.last_mut() {
        frame.pc = target.end_pc.saturating_add(1);
    }
    Step::Continue
}

fn skip_else(machine: &mut Machine) -> Step {
    let Some(control) = machine.control_stack.last().cloned() else {
        return Step::Trap(Trap::HostFailure("else outside if".into()));
    };
    if control.kind != ControlKind::If {
        return Step::Trap(Trap::HostFailure("else does not close if".into()));
    }
    if let Some(frame) = machine.frames.last_mut() {
        frame.pc = control.pc_end;
    }
    Step::Continue
}

fn branch(machine: &mut Machine, depth: u32) -> Step {
    let depth_index = match usize::try_from(depth)
        .ok()
        .and_then(|depth| depth.checked_add(1))
    {
        Some(index) => index,
        None => return Step::Trap(Trap::HostFailure("invalid branch depth".into())),
    };
    let Some(target_index) = machine.control_stack.len().checked_sub(depth_index) else {
        return Step::Trap(Trap::HostFailure("invalid branch depth".into()));
    };
    let Some(control) = machine.control_stack.get(target_index).cloned() else {
        return Step::Trap(Trap::HostFailure("invalid branch depth".into()));
    };
    if control.kind == ControlKind::Function {
        return return_frame(machine);
    }
    // A branch to a loop re-enters at its *start*, so it carries the loop's
    // parameter arity, not its result arity. The current block-type
    // representation has no independent param list, so that arity is always
    // zero; a branch to a block or `if` carries its result arity instead,
    // since that branch targets the construct's *end*.
    let arity = if control.kind == ControlKind::Loop {
        0
    } else {
        control.block_type_arity
    };
    let Some(start) = machine.operand_stack.len().checked_sub(arity) else {
        return Step::Trap(Trap::HostFailure("branch result underflow".into()));
    };
    let results = machine.operand_stack.split_off(start);
    machine.operand_stack.truncate(control.stack_height);
    machine.operand_stack.extend(results);
    if control.kind == ControlKind::Loop {
        machine.control_stack.truncate(target_index + 1);
        if let Some(frame) = machine.frames.last_mut() {
            frame.pc = control.pc_start.saturating_add(1);
        }
    } else {
        machine.control_stack.truncate(target_index);
        if let Some(frame) = machine.frames.last_mut() {
            frame.pc = control.pc_end.saturating_add(1);
        }
    }
    Step::Continue
}

fn branch_if(machine: &mut Machine, depth: u32) -> Step {
    let condition = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if condition == 0 {
        Step::Continue
    } else {
        branch(machine, depth)
    }
}

fn branch_table(machine: &mut Machine, table: crate::instr::BrTable) -> Step {
    let selector = match pop_typed_i32(machine) {
        Ok(value) => value as u32,
        Err(step) => return step,
    };
    let depth = table
        .targets
        .get(selector as usize)
        .copied()
        .unwrap_or(table.default);
    branch(machine, depth)
}

fn default_value(value_type: ValueType) -> Value {
    match value_type {
        ValueType::I32 => Value::I32(0),
        ValueType::I64 => Value::I64(0),
        ValueType::F32 => Value::F32(0),
        ValueType::F64 => Value::F64(0),
        ValueType::V128 => Value::V128(0),
        ValueType::Ref(kind) => Value::Ref(RefValue::Null(kind)),
    }
}

fn pop_arguments(machine: &mut Machine, types: &[ValueType]) -> Result<Vec<Value>, Step> {
    let mut arguments = Vec::with_capacity(types.len());
    for expected in types.iter().rev() {
        let value = pop_value(machine)?;
        let actual = match &value {
            Value::I32(_) => ValueType::I32,
            Value::I64(_) => ValueType::I64,
            Value::F32(_) => ValueType::F32,
            Value::F64(_) => ValueType::F64,
            Value::V128(_) => ValueType::V128,
            Value::Ref(value) => ValueType::Ref(match value {
                RefValue::Null(kind) => *kind,
                RefValue::FuncRef(_) => tpt_wasm_types::RefType::FuncRef,
                RefValue::ExternRef(_) => tpt_wasm_types::RefType::ExternRef,
            }),
        };
        if actual != *expected {
            return Err(Step::Trap(Trap::HostFailure(
                "function argument type mismatch".into(),
            )));
        }
        arguments.push(value);
    }
    arguments.reverse();
    Ok(arguments)
}

fn call_function(machine: &mut Machine, index: u32) -> Step {
    let instance_idx = match machine.frames.last() {
        Some(frame) => frame.instance_idx,
        None => return Step::Trap(Trap::HostFailure("no active frame".into())),
    };
    let address = match machine.store.instance(instance_idx) {
        Ok(instance) => match instance.func_addrs.get(index as usize) {
            Some(address) => *address,
            None => return Step::Trap(Trap::HostFailure("unknown function index".into())),
        },
        Err(_) => return Step::Trap(Trap::HostFailure("unknown instance".into())),
    };
    call_address(machine, address)
}

fn call_indirect(machine: &mut Machine, type_index: u32, table_index: u32) -> Step {
    let instance_idx = match machine.frames.last() {
        Some(frame) => frame.instance_idx,
        None => return Step::Trap(Trap::HostFailure("no active frame".into())),
    };
    let (table_address, expected) = match machine.store.instance(instance_idx) {
        Ok(instance) => {
            let table_address = match instance.table_addrs.get(table_index as usize) {
                Some(address) => *address,
                None => return Step::Trap(Trap::TableOutOfBounds),
            };
            let expected = match instance.module_types.get(type_index as usize) {
                Some(function_type) => function_type.clone(),
                None => return Step::Trap(Trap::HostFailure("unknown function type".into())),
            };
            (table_address, expected)
        }
        Err(_) => return Step::Trap(Trap::HostFailure("unknown instance".into())),
    };
    let index_value = match pop_typed_i32(machine) {
        Ok(value) => value as u32,
        Err(step) => return step,
    };
    let reference = match machine
        .store
        .table(table_address)
        .and_then(|table| table.get(index_value))
    {
        Ok(reference) => reference,
        Err(_) => return Step::Trap(Trap::TableOutOfBounds),
    };
    let address = match reference {
        Some(RefValue::FuncRef(address)) => address,
        _ => return Step::Trap(Trap::NullReference),
    };
    let actual = match machine.store.function(address) {
        Ok(crate::store::FuncInstance::Wasm(function)) => function.func_type.clone(),
        Ok(crate::store::FuncInstance::Host(function)) => function.func_type.clone(),
        Err(_) => return Step::Trap(Trap::HostFailure("unknown function address".into())),
    };
    if actual != expected {
        return Step::Trap(Trap::IndirectCallTypeMismatch);
    }
    call_address(machine, address)
}

fn call_address(machine: &mut Machine, address: u32) -> Step {
    let function = match machine.store.function(address) {
        Ok(function) => function,
        Err(_) => return Step::Trap(Trap::HostFailure("unknown function address".into())),
    };
    match function {
        crate::store::FuncInstance::Wasm(function) => {
            let (param_types, local_types, instance_idx, func_idx, instrs, result_count) = (
                function.func_type.params.0.clone(),
                function.local_types.clone(),
                function.instance_idx,
                function.func_idx,
                function.instrs.clone(),
                function.func_type.results.0.len(),
            );
            let arguments = match pop_arguments(machine, &param_types) {
                Ok(arguments) => arguments,
                Err(step) => return step,
            };
            let mut locals = arguments;
            locals.extend(
                local_types
                    .iter()
                    .skip(param_types.len())
                    .copied()
                    .map(default_value),
            );
            let frame = Frame::new(instance_idx, func_idx, locals, instrs, result_count);
            match machine.push_frame(frame) {
                Ok(()) => Step::Continue,
                Err(trap) => Step::Trap(trap),
            }
        }
        crate::store::FuncInstance::Host(function) => {
            let (param_types, name, result_count) = (
                function.func_type.params.0.clone(),
                function.name.clone(),
                function.func_type.results.0.len(),
            );
            let arguments = match pop_arguments(machine, &param_types) {
                Ok(arguments) => arguments,
                Err(step) => return step,
            };
            machine.pending_host_results = Some(result_count);
            Step::HostCall(HostCall {
                func_name: name,
                args: arguments,
                expected_results: result_count,
            })
        }
    }
}

fn push_value(machine: &mut Machine, value: Value) -> Step {
    if machine.operand_stack.len() >= machine.store.limits.max_stack_depth {
        return Step::Trap(Trap::StackOverflow);
    }
    machine.operand_stack.push(value);
    Step::Continue
}

fn pop_value(machine: &mut Machine) -> Result<Value, Step> {
    machine
        .operand_stack
        .pop()
        .ok_or_else(|| Step::Trap(Trap::HostFailure("operand stack underflow".into())))
}

fn local_get(machine: &mut Machine, index: u32) -> Step {
    let value = machine
        .frames
        .last()
        .and_then(|frame| frame.locals.get(index as usize))
        .cloned();
    match value {
        Some(value) => push_value(machine, value),
        None => Step::Trap(Trap::HostFailure(format!(
            "local index {index} out of bounds"
        ))),
    }
}

fn local_set(machine: &mut Machine, index: u32) -> Step {
    let value = match pop_value(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let Some(frame) = machine.frames.last_mut() else {
        return Step::Trap(Trap::HostFailure("no active frame".into()));
    };
    let Some(local) = frame.locals.get_mut(index as usize) else {
        return Step::Trap(Trap::HostFailure(format!(
            "local index {index} out of bounds"
        )));
    };
    *local = value;
    Step::Continue
}

fn local_tee(machine: &mut Machine, index: u32) -> Step {
    let Some(value) = machine.operand_stack.last().cloned() else {
        return Step::Trap(Trap::HostFailure("operand stack underflow".into()));
    };
    let Some(frame) = machine.frames.last_mut() else {
        return Step::Trap(Trap::HostFailure("no active frame".into()));
    };
    let Some(local) = frame.locals.get_mut(index as usize) else {
        return Step::Trap(Trap::HostFailure(format!(
            "local index {index} out of bounds"
        )));
    };
    *local = value;
    Step::Continue
}

fn instance_address(
    machine: &Machine,
    addresses: &[u32],
    index: u32,
    kind: &str,
) -> Result<u32, Step> {
    if machine.frames.last().is_none() {
        return Err(Step::Trap(Trap::HostFailure("no active frame".into())));
    }
    addresses
        .get(index as usize)
        .copied()
        .ok_or_else(|| Step::Trap(Trap::HostFailure(format!("unknown {kind} index {index}"))))
}

fn global_get(machine: &mut Machine, index: u32) -> Step {
    let address = match instance_address(machine, &global_addresses(machine), index, "global") {
        Ok(address) => address,
        Err(step) => return step,
    };
    let value = machine
        .store
        .global(address)
        .ok()
        .map(|global| global.value.clone());
    match value {
        Some(value) => push_value(machine, value),
        None => Step::Trap(Trap::HostFailure(format!(
            "global address {address} not found"
        ))),
    }
}

fn global_addresses(machine: &Machine) -> Vec<u32> {
    machine
        .frames
        .last()
        .and_then(|frame| machine.store.instance(frame.instance_idx).ok())
        .map(|instance| instance.global_addrs.clone())
        .unwrap_or_default()
}

fn global_set(machine: &mut Machine, index: u32) -> Step {
    let address = match instance_address(machine, &global_addresses(machine), index, "global") {
        Ok(address) => address,
        Err(step) => return step,
    };
    let value = match pop_value(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    match machine.store.set_global(address, value) {
        Ok(()) => Step::Continue,
        Err(error) => Step::Trap(Trap::HostFailure(error.to_string())),
    }
}
fn memory_addresses(machine: &Machine) -> Vec<u32> {
    machine
        .frames
        .last()
        .and_then(|frame| machine.store.instance(frame.instance_idx).ok())
        .map(|instance| instance.memory_addrs.clone())
        .unwrap_or_default()
}

fn memory_address(machine: &mut Machine, arg: crate::instr::MemArg) -> Result<u64, Step> {
    let base = pop_typed_i32(machine)?;
    let base = u64::from(base as u32);
    base.checked_add(arg.offset)
        .ok_or(Step::Trap(Trap::MemoryOutOfBounds))
}

fn read_memory(
    machine: &mut Machine,
    arg: crate::instr::MemArg,
    length: usize,
) -> Result<Vec<u8>, Step> {
    let offset = memory_address(machine, arg)?;
    let address = instance_address(
        machine,
        &memory_addresses(machine),
        arg.memory_index,
        "memory",
    )?;
    let memory = machine
        .store
        .memory(address)
        .map_err(|_| Step::Trap(Trap::MemoryOutOfBounds))?;
    memory
        .read(offset, length)
        .map(<[u8]>::to_vec)
        .map_err(|_| Step::Trap(Trap::MemoryOutOfBounds))
}

fn write_memory(machine: &mut Machine, arg: crate::instr::MemArg, bytes: &[u8]) -> Step {
    let offset = match memory_address(machine, arg) {
        Ok(address) => address,
        Err(step) => return step,
    };
    let address = match instance_address(
        machine,
        &memory_addresses(machine),
        arg.memory_index,
        "memory",
    ) {
        Ok(address) => address,
        Err(step) => return step,
    };
    let memory = match machine.store.memory_mut(address) {
        Ok(memory) => memory,
        Err(_) => return Step::Trap(Trap::MemoryOutOfBounds),
    };
    match memory.write(offset, bytes) {
        Ok(()) => Step::Continue,
        Err(_) => Step::Trap(Trap::MemoryOutOfBounds),
    }
}

fn load_i32(machine: &mut Machine, arg: crate::instr::MemArg) -> Step {
    let bytes = match read_memory(machine, arg, 4) {
        Ok(bytes) => bytes,
        Err(step) => return step,
    };
    push_value(
        machine,
        Value::I32(i32::from_le_bytes(bytes.try_into().unwrap_or([0; 4]))),
    )
}

fn load_i64(machine: &mut Machine, arg: crate::instr::MemArg) -> Step {
    let bytes = match read_memory(machine, arg, 8) {
        Ok(bytes) => bytes,
        Err(step) => return step,
    };
    push_value(
        machine,
        Value::I64(i64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))),
    )
}

fn load_f32(machine: &mut Machine, arg: crate::instr::MemArg) -> Step {
    let bytes = match read_memory(machine, arg, 4) {
        Ok(bytes) => bytes,
        Err(step) => return step,
    };
    push_value(
        machine,
        Value::F32(u32::from_le_bytes(bytes.try_into().unwrap_or([0; 4]))),
    )
}

fn load_f64(machine: &mut Machine, arg: crate::instr::MemArg) -> Step {
    let bytes = match read_memory(machine, arg, 8) {
        Ok(bytes) => bytes,
        Err(step) => return step,
    };
    push_value(
        machine,
        Value::F64(u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))),
    )
}

fn load_i32_small(
    machine: &mut Machine,
    arg: crate::instr::MemArg,
    width: usize,
    signed: bool,
) -> Step {
    let bytes = match read_memory(machine, arg, width) {
        Ok(bytes) => bytes,
        Err(step) => return step,
    };
    let mut value = 0u32;
    for (shift, byte) in bytes.into_iter().enumerate() {
        value |= u32::from(byte) << (shift * 8);
    }
    let value = if signed && width < 4 {
        let shift = 32 - width * 8;
        ((value << shift) as i32 >> shift) as u32
    } else {
        value
    };
    push_value(machine, Value::I32(value as i32))
}

fn load_i64_small(
    machine: &mut Machine,
    arg: crate::instr::MemArg,
    width: usize,
    signed: bool,
) -> Step {
    let bytes = match read_memory(machine, arg, width) {
        Ok(bytes) => bytes,
        Err(step) => return step,
    };
    let mut value = 0u64;
    for (shift, byte) in bytes.into_iter().enumerate() {
        value |= u64::from(byte) << (shift * 8);
    }
    let value = if signed && width < 8 {
        let shift = 64 - width * 8;
        ((value << shift) as i64 >> shift) as u64
    } else {
        value
    };
    push_value(machine, Value::I64(value as i64))
}

fn store_i32(machine: &mut Machine, arg: crate::instr::MemArg, width: usize) -> Step {
    let value = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let bytes = value.to_le_bytes();
    write_memory(machine, arg, &bytes[..width])
}

fn store_i64(machine: &mut Machine, arg: crate::instr::MemArg, width: usize) -> Step {
    let value = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let bytes = value.to_le_bytes();
    write_memory(machine, arg, &bytes[..width])
}

fn store_f32(machine: &mut Machine, arg: crate::instr::MemArg) -> Step {
    let value = match pop_value(machine) {
        Ok(Value::F32(value)) => value,
        Ok(_) => return Step::Trap(Trap::HostFailure("expected f32 operand".into())),
        Err(step) => return step,
    };
    write_memory(machine, arg, &value.to_le_bytes())
}

fn store_f64(machine: &mut Machine, arg: crate::instr::MemArg) -> Step {
    let value = match pop_value(machine) {
        Ok(Value::F64(value)) => value,
        Ok(_) => return Step::Trap(Trap::HostFailure("expected f64 operand".into())),
        Err(step) => return step,
    };
    write_memory(machine, arg, &value.to_le_bytes())
}

fn memory_size(machine: &mut Machine, index: u32) -> Step {
    let address = match instance_address(machine, &memory_addresses(machine), index, "memory") {
        Ok(address) => address,
        Err(step) => return step,
    };
    match machine.store.memory(address) {
        Ok(memory) => push_value(machine, Value::I32(memory.pages() as i32)),
        Err(_) => Step::Trap(Trap::MemoryOutOfBounds),
    }
}

fn memory_grow(machine: &mut Machine, index: u32) -> Step {
    let delta = match pop_typed_i32(machine) {
        Ok(value) => value as u32,
        Err(step) => return step,
    };
    let limit = machine.store.limits.max_memory_pages;
    let address = match instance_address(machine, &memory_addresses(machine), index, "memory") {
        Ok(address) => address,
        Err(step) => return step,
    };
    let memory = match machine.store.memory_mut(address) {
        Ok(memory) => memory,
        Err(_) => return Step::Trap(Trap::MemoryOutOfBounds),
    };
    match memory.grow(u64::from(delta), limit) {
        Ok(old_pages) => push_value(machine, Value::I32(old_pages as i32)),
        Err(_) => push_value(machine, Value::I32(-1)),
    }
}

fn pop_typed_i32(machine: &mut Machine) -> Result<i32, Step> {
    match pop_value(machine)? {
        Value::I32(value) => Ok(value),
        _ => Err(Step::Trap(Trap::HostFailure("expected i32 operand".into()))),
    }
}

fn pop_typed_i64(machine: &mut Machine) -> Result<i64, Step> {
    match pop_value(machine)? {
        Value::I64(value) => Ok(value),
        _ => Err(Step::Trap(Trap::HostFailure("expected i64 operand".into()))),
    }
}

fn pop_typed_f32(machine: &mut Machine) -> Result<f32, Step> {
    match pop_value(machine)? {
        Value::F32(value) => Ok(f32::from_bits(value)),
        _ => Err(Step::Trap(Trap::HostFailure("expected f32 operand".into()))),
    }
}

fn pop_typed_f64(machine: &mut Machine) -> Result<f64, Step> {
    match pop_value(machine)? {
        Value::F64(value) => Ok(f64::from_bits(value)),
        _ => Err(Step::Trap(Trap::HostFailure("expected f64 operand".into()))),
    }
}

fn runtime_value_type(value: &Value) -> ValueType {
    match value {
        Value::I32(_) => ValueType::I32,
        Value::I64(_) => ValueType::I64,
        Value::F32(_) => ValueType::F32,
        Value::F64(_) => ValueType::F64,
        Value::V128(_) => ValueType::V128,
        Value::Ref(value) => ValueType::Ref(match value {
            RefValue::Null(kind) => *kind,
            RefValue::FuncRef(_) => tpt_wasm_types::RefType::FuncRef,
            RefValue::ExternRef(_) => tpt_wasm_types::RefType::ExternRef,
        }),
    }
}

fn select_value(machine: &mut Machine) -> Step {
    let condition = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let right = match pop_value(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_value(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if runtime_value_type(&left) != runtime_value_type(&right) {
        return Step::Trap(Trap::HostFailure(
            "select operands have different types".into(),
        ));
    }
    push_value(machine, if condition != 0 { left } else { right })
}

fn ref_is_null(machine: &mut Machine) -> Step {
    match pop_value(machine) {
        Ok(Value::Ref(RefValue::Null(_))) => push_value(machine, Value::I32(1)),
        Ok(Value::Ref(_)) => push_value(machine, Value::I32(0)),
        Ok(_) => Step::Trap(Trap::HostFailure("expected reference operand".into())),
        Err(step) => step,
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

fn unary_f32(machine: &mut Machine, operation: impl FnOnce(f32) -> f32) -> Step {
    let value = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F32(f32_result(operation(value))))
}

fn unary_f32_preserving_nan(machine: &mut Machine, operation: impl FnOnce(f32) -> f32) -> Step {
    let value = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F32(operation(value).to_bits()))
}

fn unary_f64(machine: &mut Machine, operation: impl FnOnce(f64) -> f64) -> Step {
    let value = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F64(f64_result(operation(value))))
}

fn unary_f64_preserving_nan(machine: &mut Machine, operation: impl FnOnce(f64) -> f64) -> Step {
    let value = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F64(operation(value).to_bits()))
}

fn binary_f32(machine: &mut Machine, operation: impl FnOnce(f32, f32) -> f32) -> Step {
    let right = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F32(f32_result(operation(left, right))))
}

fn binary_f32_preserving_nan(
    machine: &mut Machine,
    operation: impl FnOnce(f32, f32) -> f32,
) -> Step {
    let right = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F32(operation(left, right).to_bits()))
}

fn binary_f64(machine: &mut Machine, operation: impl FnOnce(f64, f64) -> f64) -> Step {
    let right = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F64(f64_result(operation(left, right))))
}

fn binary_f64_preserving_nan(
    machine: &mut Machine,
    operation: impl FnOnce(f64, f64) -> f64,
) -> Step {
    let right = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F64(operation(left, right).to_bits()))
}

fn compare_f32(machine: &mut Machine, operation: impl FnOnce(f32, f32) -> bool) -> Step {
    let right = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(operation(left, right) as i32))
}

fn compare_f64(machine: &mut Machine, operation: impl FnOnce(f64, f64) -> bool) -> Step {
    let right = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(operation(left, right) as i32))
}

fn invalid_conversion() -> Step {
    Step::Trap(Trap::InvalidConversion)
}

fn trunc_f32_to_i32(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if !value.is_finite() {
        return invalid_conversion();
    }
    let truncated = value.trunc();
    if signed {
        if truncated < i32::MIN as f32 || truncated >= 2147483648.0f32 {
            return invalid_conversion();
        }
        push_value(machine, Value::I32(truncated as i32))
    } else if !(0.0..4294967296.0f32).contains(&truncated) {
        invalid_conversion()
    } else {
        push_value(machine, Value::I32(truncated as u32 as i32))
    }
}

fn trunc_f64_to_i32(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if !value.is_finite() {
        return invalid_conversion();
    }
    let truncated = value.trunc();
    if signed {
        if truncated < i32::MIN as f64 || truncated >= 2147483648.0f64 {
            return invalid_conversion();
        }
        push_value(machine, Value::I32(truncated as i32))
    } else if !(0.0..4294967296.0f64).contains(&truncated) {
        invalid_conversion()
    } else {
        push_value(machine, Value::I32(truncated as u32 as i32))
    }
}

fn trunc_f32_to_i64(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if !value.is_finite() {
        return invalid_conversion();
    }
    let truncated = value.trunc();
    if signed {
        if !(-9223372036854775808.0f32..9223372036854775808.0f32).contains(&truncated) {
            return invalid_conversion();
        }
        push_value(machine, Value::I64(truncated as i64))
    } else if !(0.0..18446744073709551616.0f32).contains(&truncated) {
        invalid_conversion()
    } else {
        push_value(machine, Value::I64(truncated as u64 as i64))
    }
}

fn trunc_f64_to_i64(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if !value.is_finite() {
        return invalid_conversion();
    }
    let truncated = value.trunc();
    if signed {
        if !(-9223372036854775808.0f64..9223372036854775808.0f64).contains(&truncated) {
            return invalid_conversion();
        }
        push_value(machine, Value::I64(truncated as i64))
    } else if !(0.0..18446744073709551616.0f64).contains(&truncated) {
        invalid_conversion()
    } else {
        push_value(machine, Value::I64(truncated as u64 as i64))
    }
}

fn convert_i64_to_i32(machine: &mut Machine) -> Step {
    let value = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(value as i32))
}

fn convert_i32_to_i64(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let value = if signed {
        i64::from(value)
    } else {
        i64::from(value as u32)
    };
    push_value(machine, Value::I64(value))
}

fn convert_i32_to_f32(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let value = if signed {
        value as f32
    } else {
        (value as u32) as f32
    };
    push_value(machine, Value::F32(f32_result(value)))
}

fn convert_i64_to_f32(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let value = if signed {
        value as f32
    } else {
        (value as u64) as f32
    };
    push_value(machine, Value::F32(f32_result(value)))
}

fn convert_i32_to_f64(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let value = if signed {
        value as f64
    } else {
        (value as u32) as f64
    };
    push_value(machine, Value::F64(f64_result(value)))
}

fn convert_i64_to_f64(machine: &mut Machine, signed: bool) -> Step {
    let value = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let value = if signed {
        value as f64
    } else {
        (value as u64) as f64
    };
    push_value(machine, Value::F64(f64_result(value)))
}

fn convert_f32_to_f64(machine: &mut Machine) -> Step {
    let value = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F64(f64_result(value as f64)))
}

fn convert_f64_to_f32(machine: &mut Machine) -> Step {
    let value = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F32(f32_result(value as f32)))
}

fn reinterpret_f32_to_i32(machine: &mut Machine) -> Step {
    let value = match pop_typed_f32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(value.to_bits() as i32))
}

fn reinterpret_f64_to_i64(machine: &mut Machine) -> Step {
    let value = match pop_typed_f64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I64(value.to_bits() as i64))
}

fn reinterpret_i32_to_f32(machine: &mut Machine) -> Step {
    let value = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F32(value as u32))
}

fn reinterpret_i64_to_f64(machine: &mut Machine) -> Step {
    let value = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::F64(value as u64))
}

fn unary_i32(machine: &mut Machine, operation: impl FnOnce(i32) -> i32) -> Step {
    let value = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(operation(value)))
}

fn unary_i64(machine: &mut Machine, operation: impl FnOnce(i64) -> i64) -> Step {
    let value = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I64(operation(value)))
}

fn unary_i64_to_i32(machine: &mut Machine, operation: impl FnOnce(i64) -> i32) -> Step {
    let value = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(operation(value)))
}

fn compare_i32(machine: &mut Machine, operation: impl FnOnce(i32, i32) -> bool) -> Step {
    let right = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(operation(left, right) as i32))
}

fn compare_i64(machine: &mut Machine, operation: impl FnOnce(i64, i64) -> bool) -> Step {
    let right = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(operation(left, right) as i32))
}

fn binary_i32(machine: &mut Machine, operation: impl FnOnce(i32, i32) -> i32) -> Step {
    let right = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I32(operation(left, right)))
}

fn binary_i64(machine: &mut Machine, operation: impl FnOnce(i64, i64) -> i64) -> Step {
    let right = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    push_value(machine, Value::I64(operation(left, right)))
}

fn div_i32_s(machine: &mut Machine) -> Step {
    let right = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    if left == i32::MIN && right == -1 {
        return Step::Trap(Trap::IntegerOverflow);
    }
    push_value(machine, Value::I32(left / right))
}

fn div_i32_u(machine: &mut Machine) -> Step {
    let right = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    push_value(machine, Value::I32(((left as u32) / (right as u32)) as i32))
}

fn rem_i32_s(machine: &mut Machine) -> Step {
    let right = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    push_value(machine, Value::I32(left.wrapping_rem(right)))
}

fn rem_i32_u(machine: &mut Machine) -> Step {
    let right = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i32(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    push_value(machine, Value::I32(((left as u32) % (right as u32)) as i32))
}

fn div_i64_s(machine: &mut Machine) -> Step {
    let right = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    if left == i64::MIN && right == -1 {
        return Step::Trap(Trap::IntegerOverflow);
    }
    push_value(machine, Value::I64(left / right))
}

fn div_i64_u(machine: &mut Machine) -> Step {
    let right = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    push_value(machine, Value::I64(((left as u64) / (right as u64)) as i64))
}

fn rem_i64_s(machine: &mut Machine) -> Step {
    let right = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    push_value(machine, Value::I64(left.wrapping_rem(right)))
}

fn rem_i64_u(machine: &mut Machine) -> Step {
    let right = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    let left = match pop_typed_i64(machine) {
        Ok(value) => value,
        Err(step) => return step,
    };
    if right == 0 {
        return Step::Trap(Trap::IntegerDivisionByZero);
    }
    push_value(machine, Value::I64(((left as u64) % (right as u64)) as i64))
}

fn return_frame(machine: &mut Machine) -> Step {
    let Some(frame) = machine.frames.last() else {
        return Step::Trap(Trap::HostFailure("no active frame".into()));
    };
    let arity = frame.return_arity;
    let control_base = frame.control_base;
    let stack_height = machine
        .control_stack
        .get(control_base)
        .map_or(0, |control| control.stack_height);
    let Some(start) = machine.operand_stack.len().checked_sub(arity) else {
        return Step::Trap(Trap::HostFailure(
            "operand stack underflow on return".into(),
        ));
    };
    let results = machine.operand_stack.split_off(start);
    machine.operand_stack.truncate(stack_height);
    machine.control_stack.truncate(control_base);
    machine.frames.pop();
    if machine.frames.is_empty() {
        Step::Return(results)
    } else {
        machine.operand_stack.extend(results);
        Step::Continue
    }
}

impl Machine {
    /// Create an empty machine with the default resource limits.
    pub fn new() -> Self {
        Self::with_store(Store::default())
    }

    /// Create an empty machine with an explicit store and default policy.
    pub fn with_store(store: Store) -> Self {
        Self::with_execution(store, ExecutionConfig::default())
    }

    /// Create an empty machine with an explicit store and execution policy.
    pub fn with_execution(store: Store, execution: ExecutionConfig) -> Self {
        Self {
            store,
            operand_stack: Vec::new(),
            frames: Vec::new(),
            control_stack: Vec::new(),
            steps: 0,
            pending_host_results: None,
            rng: DeterministicRng::new(execution.seed),
            execution,
        }
    }

    /// Draw the next value from the machine's reproducible effect stream.
    pub fn next_deterministic_u64(&mut self) -> u64 {
        self.rng.next_u64()
    }

    /// Execute until return, trap, host call, or the configured step limit.
    pub fn run(&mut self) -> Step {
        loop {
            let result = step(self);
            if !matches!(result, Step::Continue) {
                return result;
            }
        }
    }

    /// Resume a suspended host call with its results.
    pub fn resume_host_call(&mut self, results: Vec<Value>) -> Result<Step, Trap> {
        let Some(expected) = self.pending_host_results.take() else {
            return Err(Trap::HostFailure("no host call is pending".into()));
        };
        if results.len() != expected {
            return Err(Trap::HostFailure("host result count mismatch".into()));
        }
        for result in results {
            if let Step::Trap(trap) = push_value(self, result) {
                return Err(trap);
            }
        }
        Ok(Step::Continue)
    }

    /// Push an initial frame for a function body represented as decoded instructions.
    pub fn push_frame(&mut self, mut frame: Frame) -> Result<(), Trap> {
        if self.frames.len() >= self.store.limits.max_call_depth {
            return Err(Trap::CallDepthExceeded);
        }
        if self.operand_stack.len() >= self.store.limits.max_stack_depth {
            return Err(Trap::StackOverflow);
        }
        frame.control_base = self.control_stack.len();
        let function_end = frame.instrs.len().saturating_sub(1);
        self.control_stack.push(ControlFrame {
            kind: ControlKind::Function,
            block_type_arity: frame.return_arity,
            stack_height: self.operand_stack.len(),
            pc_else: None,
            pc_end: function_end,
            pc_start: 0,
        });
        self.frames.push(frame);
        Ok(())
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, Machine, Step};
    use crate::execution::ExecutionConfig;
    use crate::instr::{decode_body, BlockType, Instr, MemArg};
    use crate::store::{Instance, Store, StoreError};
    use tpt_wasm_types::{
        FunctionType, GlobalType, RefType, RefValue, ResourceLimits, ResultType, Trap, Value,
        ValueType,
    };

    fn machine_with_frame(instrs: Vec<Instr>, locals: Vec<Value>, return_arity: usize) -> Machine {
        let mut machine = Machine::new();
        machine
            .push_frame(Frame::new(0, 0, locals, instrs, return_arity))
            .unwrap();
        machine
    }

    #[test]
    fn constants_and_integer_add_return_values() {
        let mut machine = machine_with_frame(
            vec![
                Instr::I32Const(20),
                Instr::I32Const(22),
                Instr::I32Add,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(machine.run(), Step::Return(vec![Value::I32(42)]));
        assert!(machine.frames.is_empty());
    }

    #[test]
    fn locals_and_globals_are_checked() {
        let mut store = Store::default();
        let _decoy_global = store
            .add_global(
                GlobalType {
                    value_type: ValueType::I64,
                    mutable: false,
                },
                Value::I64(0),
            )
            .unwrap();
        let global = store
            .add_global(
                GlobalType {
                    value_type: ValueType::I32,
                    mutable: true,
                },
                Value::I32(7),
            )
            .unwrap();
        store
            .add_instance(Instance {
                module_types: Vec::new(),
                func_addrs: Vec::new(),
                table_addrs: Vec::new(),
                memory_addrs: Vec::new(),
                global_addrs: vec![global],
            })
            .unwrap();
        let mut machine = Machine::with_store(store);
        machine
            .push_frame(Frame::new(
                0,
                0,
                vec![Value::I32(3)],
                vec![
                    Instr::LocalGet(0),
                    Instr::GlobalGet(0),
                    Instr::I32Add,
                    Instr::LocalSet(0),
                    Instr::LocalGet(0),
                    Instr::End,
                ],
                1,
            ))
            .unwrap();
        assert_eq!(machine.run(), Step::Return(vec![Value::I32(10)]));
    }

    #[test]
    fn invalid_instruction_state_returns_traps_without_panicking() {
        let mut machine = machine_with_frame(vec![Instr::I32Add], Vec::new(), 0);
        assert!(matches!(
            super::step(&mut machine),
            Step::Trap(Trap::HostFailure(_))
        ));

        let mut machine = machine_with_frame(vec![Instr::Unreachable], Vec::new(), 0);
        assert_eq!(super::step(&mut machine), Step::Trap(Trap::Unreachable));
    }

    #[test]
    fn execution_step_limit_is_enforced() {
        let limits = ResourceLimits {
            max_execution_steps: Some(1),
            ..ResourceLimits::default()
        };
        let mut machine = Machine::with_store(Store::new(limits));
        machine
            .push_frame(Frame::new(
                0,
                0,
                Vec::new(),
                vec![Instr::Nop, Instr::Nop],
                0,
            ))
            .unwrap();
        assert_eq!(super::step(&mut machine), Step::Continue);
        assert_eq!(super::step(&mut machine), Step::Trap(Trap::StepsExhausted));
    }

    #[test]
    fn integer_division_traps_and_bit_operations_are_checked() {
        let mut zero_divisor = machine_with_frame(
            vec![
                Instr::I32Const(1),
                Instr::I32Const(0),
                Instr::I32DivS,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(zero_divisor.run(), Step::Trap(Trap::IntegerDivisionByZero));

        let mut overflow = machine_with_frame(
            vec![
                Instr::I32Const(i32::MIN),
                Instr::I32Const(-1),
                Instr::I32DivS,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(overflow.run(), Step::Trap(Trap::IntegerOverflow));

        let mut bits = machine_with_frame(
            vec![
                Instr::I32Const(0b1010),
                Instr::I32Const(0b0110),
                Instr::I32And,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(bits.run(), Step::Return(vec![Value::I32(0b0010)]));

        let mut shift = machine_with_frame(
            vec![
                Instr::I32Const(1),
                Instr::I32Const(33),
                Instr::I32Shl,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(shift.run(), Step::Return(vec![Value::I32(2)]));
    }

    #[test]
    fn memory_loads_stores_and_growth_follow_mvp_rules() {
        let mut store = Store::default();
        let _decoy_memory = store.allocate_memory(1, None).unwrap();
        let memory = store.allocate_memory(1, Some(2)).unwrap();
        store
            .add_instance(Instance {
                module_types: Vec::new(),
                func_addrs: Vec::new(),
                table_addrs: Vec::new(),
                memory_addrs: vec![memory],
                global_addrs: Vec::new(),
            })
            .unwrap();
        let mut machine = Machine::with_store(store);
        machine
            .push_frame(Frame::new(
                0,
                0,
                Vec::new(),
                vec![
                    Instr::I32Const(0),
                    Instr::I32Const(-2),
                    Instr::I32Store(MemArg {
                        align: 2,
                        offset: 0,
                        memory_index: 0,
                    }),
                    Instr::I32Const(0),
                    Instr::I32Load8s(MemArg {
                        align: 0,
                        offset: 0,
                        memory_index: 0,
                    }),
                    Instr::End,
                ],
                1,
            ))
            .unwrap();
        assert_eq!(machine.run(), Step::Return(vec![Value::I32(-2)]));

        let mut grow = Machine::new();
        grow.store = machine.store;
        grow.push_frame(Frame::new(
            0,
            0,
            Vec::new(),
            vec![Instr::I32Const(1), Instr::MemoryGrow(0), Instr::End],
            1,
        ))
        .unwrap();
        assert_eq!(grow.run(), Step::Return(vec![Value::I32(1)]));

        let mut failed = Machine::new();
        failed.store = grow.store;
        failed
            .push_frame(Frame::new(
                0,
                0,
                Vec::new(),
                vec![Instr::I32Const(1), Instr::MemoryGrow(0), Instr::End],
                1,
            ))
            .unwrap();
        assert_eq!(failed.run(), Step::Return(vec![Value::I32(-1)]));
    }

    #[test]
    fn memory_out_of_bounds_returns_a_trap() {
        let mut store = Store::default();
        let memory = store.allocate_memory(1, None).unwrap();
        store
            .add_instance(Instance {
                module_types: Vec::new(),
                func_addrs: Vec::new(),
                table_addrs: Vec::new(),
                memory_addrs: vec![memory],
                global_addrs: Vec::new(),
            })
            .unwrap();
        let mut machine = Machine::with_store(store);
        machine
            .push_frame(Frame::new(
                0,
                0,
                Vec::new(),
                vec![
                    Instr::I32Const(65535),
                    Instr::I32Load(MemArg {
                        align: 2,
                        offset: 0,
                        memory_index: 0,
                    }),
                    Instr::End,
                ],
                1,
            ))
            .unwrap();
        assert_eq!(machine.run(), Step::Trap(Trap::MemoryOutOfBounds));
    }

    #[test]
    fn frame_preparation_decodes_a_validated_body() {
        let body = [0x41, 0x07, 0x0b];
        let frame = Frame::from_body(0, 0, Vec::new(), &body, 1).unwrap();
        assert_eq!(frame.instrs, decode_body(&body).unwrap());
    }

    fn function_type(params: Vec<ValueType>, results: Vec<ValueType>) -> FunctionType {
        FunctionType {
            params: ResultType(params),
            results: ResultType(results),
        }
    }

    #[test]
    fn if_else_selects_the_expected_branch() {
        let mut then = machine_with_frame(
            vec![
                Instr::I32Const(1),
                Instr::If(BlockType::Value(ValueType::I32)),
                Instr::I32Const(10),
                Instr::Else,
                Instr::I32Const(20),
                Instr::End,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(then.run(), Step::Return(vec![Value::I32(10)]));

        let mut otherwise = machine_with_frame(
            vec![
                Instr::I32Const(0),
                Instr::If(BlockType::Value(ValueType::I32)),
                Instr::I32Const(10),
                Instr::Else,
                Instr::I32Const(20),
                Instr::End,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(otherwise.run(), Step::Return(vec![Value::I32(20)]));
    }

    #[test]
    fn br_if_skips_or_branches_to_a_block() {
        let instrs = |condition| {
            vec![
                Instr::Block(crate::instr::BlockType::Empty),
                Instr::I32Const(condition),
                Instr::BrIf(0),
                Instr::I32Const(1),
                Instr::End,
                Instr::I32Const(2),
                Instr::End,
            ]
        };
        let mut skipped = machine_with_frame(instrs(0), Vec::new(), 1);
        assert_eq!(skipped.run(), Step::Return(vec![Value::I32(2)]));
        let mut branched = machine_with_frame(
            vec![
                Instr::Block(crate::instr::BlockType::Value(ValueType::I32)),
                Instr::I32Const(9),
                Instr::I32Const(1),
                Instr::BrIf(0),
                Instr::End,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(branched.run(), Step::Return(vec![Value::I32(9)]));
    }

    #[test]
    fn direct_and_indirect_calls_execute_wasm_functions() {
        let callee_type = function_type(vec![], vec![ValueType::I32]);
        let mut store = Store::default();
        let callee = store
            .add_wasm_function(
                0,
                1,
                callee_type.clone(),
                vec![Instr::I32Const(42), Instr::End],
                vec![],
            )
            .unwrap();
        let caller_type = function_type(vec![], vec![ValueType::I32]);
        let instance = store
            .add_instance(Instance {
                module_types: vec![callee_type.clone(), caller_type.clone()],
                func_addrs: vec![callee, callee],
                table_addrs: vec![],
                memory_addrs: vec![],
                global_addrs: vec![],
            })
            .unwrap();
        let table = store.allocate_table(RefType::FuncRef, 1, None).unwrap();
        store
            .table_mut(table)
            .unwrap()
            .set(0, RefValue::FuncRef(callee))
            .unwrap();
        let instance = store.instances.get_mut(instance as usize).unwrap();
        instance.module_types = vec![callee_type.clone(), caller_type.clone()];
        instance.table_addrs = vec![table];

        let direct_store = store;
        let mut direct = Machine::with_store(direct_store.clone());
        direct
            .push_frame(Frame::new(
                0,
                0,
                Vec::new(),
                vec![Instr::Call(0), Instr::End],
                1,
            ))
            .unwrap();
        assert_eq!(direct.run(), Step::Return(vec![Value::I32(42)]));

        let mut indirect = Machine::with_store(direct_store.clone());
        indirect
            .push_frame(Frame::new(
                0,
                0,
                Vec::new(),
                vec![Instr::I32Const(0), Instr::CallIndirect(0, 0), Instr::End],
                1,
            ))
            .unwrap();
        assert_eq!(indirect.run(), Step::Return(vec![Value::I32(42)]));
    }

    #[test]
    fn host_calls_suspend_and_resume_with_checked_results() {
        let host_type = function_type(vec![], vec![ValueType::I32]);
        let mut store = Store::default();
        let host = store
            .add_host_function(host_type.clone(), "answer")
            .unwrap();
        let instance = store
            .add_instance(Instance {
                module_types: vec![host_type],
                func_addrs: vec![host],
                table_addrs: vec![],
                memory_addrs: vec![],
                global_addrs: vec![],
            })
            .unwrap();
        let mut machine = Machine::with_store(store);
        machine
            .push_frame(Frame::new(
                instance,
                0,
                Vec::new(),
                vec![Instr::Call(0), Instr::End],
                1,
            ))
            .unwrap();
        assert_eq!(
            machine.run(),
            Step::HostCall(super::HostCall {
                func_name: "answer".into(),
                args: vec![],
                expected_results: 1,
            })
        );
        assert_eq!(
            machine.resume_host_call(vec![Value::I32(7)]),
            Ok(Step::Continue)
        );
        assert_eq!(machine.run(), Step::Return(vec![Value::I32(7)]));
    }

    #[test]
    fn float_arithmetic_and_nan_handling_are_deterministic() {
        let mut add = machine_with_frame(
            vec![
                Instr::F32Const(1.5f32.to_bits()),
                Instr::F32Const(2.25f32.to_bits()),
                Instr::F32Add,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(add.run(), Step::Return(vec![Value::F32(3.75f32.to_bits())]));

        let mut compare = machine_with_frame(
            vec![
                Instr::F64Const(3.0f64.to_bits()),
                Instr::F64Const(2.0f64.to_bits()),
                Instr::F64Lt,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(compare.run(), Step::Return(vec![Value::I32(0)]));

        let mut min_zero = machine_with_frame(
            vec![
                Instr::F32Const((-0.0f32).to_bits()),
                Instr::F32Const(0.0f32.to_bits()),
                Instr::F32Min,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(
            min_zero.run(),
            Step::Return(vec![Value::F32((-0.0f32).to_bits())])
        );

        let mut max_zero = machine_with_frame(
            vec![
                Instr::F64Const((-0.0f64).to_bits()),
                Instr::F64Const(0.0f64.to_bits()),
                Instr::F64Max,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(
            max_zero.run(),
            Step::Return(vec![Value::F64(0.0f64.to_bits())])
        );

        let mut nan = machine_with_frame(
            vec![Instr::F32Const(0x7fc0_1234), Instr::F32Abs, Instr::End],
            Vec::new(),
            1,
        );
        assert_eq!(nan.run(), Step::Return(vec![Value::F32(0x7fc0_1234)]));
    }

    #[test]
    fn select_and_reference_null_are_executable() {
        let mut select = machine_with_frame(
            vec![
                Instr::I32Const(10),
                Instr::I32Const(20),
                Instr::I32Const(1),
                Instr::Select,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(select.run(), Step::Return(vec![Value::I32(10)]));

        let mut null = machine_with_frame(
            vec![
                Instr::RefNull(RefType::FuncRef),
                Instr::RefIsNull,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(null.run(), Step::Return(vec![Value::I32(1)]));
    }

    #[test]
    fn conversions_trap_when_out_of_range_and_reinterpret_preserves_bits() {
        let mut nan = machine_with_frame(
            vec![
                Instr::F32Const(f32::NAN.to_bits()),
                Instr::I32TruncF32S,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(nan.run(), Step::Trap(Trap::InvalidConversion));

        let mut out_of_range = machine_with_frame(
            vec![
                Instr::F64Const(4294967296.0f64.to_bits()),
                Instr::I32TruncF64U,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(out_of_range.run(), Step::Trap(Trap::InvalidConversion));

        let mut valid = machine_with_frame(
            vec![
                Instr::F64Const(3.9f64.to_bits()),
                Instr::I32TruncF64S,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(valid.run(), Step::Return(vec![Value::I32(3)]));

        let mut reinterpret = machine_with_frame(
            vec![
                Instr::I32Const(0x1234_5678),
                Instr::F32ReinterpretI32,
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(
            reinterpret.run(),
            Step::Return(vec![Value::F32(0x1234_5678)])
        );
    }

    #[test]
    fn loop_and_branch_table_control_flow_are_checked() {
        let mut loop_machine = machine_with_frame(
            vec![
                Instr::I32Const(0),
                Instr::LocalSet(0),
                Instr::Block(BlockType::Empty),
                Instr::Loop(BlockType::Empty),
                Instr::LocalGet(0),
                Instr::I32Const(1),
                Instr::I32Add,
                Instr::LocalTee(0),
                Instr::I32Const(5),
                Instr::I32LtS,
                Instr::BrIf(0),
                Instr::End,
                Instr::End,
                Instr::LocalGet(0),
                Instr::End,
            ],
            vec![Value::I32(0)],
            1,
        );
        assert_eq!(loop_machine.run(), Step::Return(vec![Value::I32(5)]));

        let mut table = machine_with_frame(
            vec![
                Instr::Block(BlockType::Empty),
                Instr::I32Const(7),
                Instr::I32Const(0),
                Instr::BrTable(crate::instr::BrTable {
                    targets: vec![0],
                    default: 0,
                }),
                Instr::End,
                Instr::I32Const(9),
                Instr::End,
            ],
            Vec::new(),
            1,
        );
        assert_eq!(table.run(), Step::Return(vec![Value::I32(9)]));
    }

    #[test]
    fn machine_carries_the_explicit_deterministic_policy() {
        let mut left =
            Machine::with_execution(Store::default(), ExecutionConfig::deterministic(19));
        let mut right =
            Machine::with_execution(Store::default(), ExecutionConfig::deterministic(19));
        let left_values = (0..4)
            .map(|_| left.next_deterministic_u64())
            .collect::<Vec<_>>();
        let right_values = (0..4)
            .map(|_| right.next_deterministic_u64())
            .collect::<Vec<_>>();
        assert_eq!(left_values, right_values);
        assert_eq!(left.execution, ExecutionConfig::deterministic(19));
    }

    #[test]
    fn repeated_execution_is_identical() {
        let run = || {
            let mut machine = machine_with_frame(
                vec![
                    Instr::I32Const(4),
                    Instr::I32Const(6),
                    Instr::I32Mul,
                    Instr::End,
                ],
                Vec::new(),
                1,
            );
            machine.run()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn immutable_global_set_is_reported_as_a_trap() {
        let mut store = Store::default();
        let global = store
            .add_global(
                GlobalType {
                    value_type: ValueType::I32,
                    mutable: false,
                },
                Value::I32(1),
            )
            .unwrap();
        assert_eq!(
            store.set_global(global, Value::I32(2)),
            Err(StoreError::ImmutableGlobal(global))
        );
    }
}
