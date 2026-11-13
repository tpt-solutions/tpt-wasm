// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The x86-64 encoder for the compiled subset. Safe code, tested on every target.
//!
//! Frame layout: `rbp` is the frame pointer. Each IR value has an 8-byte slot at a fixed negative
//! offset from `rbp`. An `i32` value uses the low four bytes of its slot and an `i64` value all
//! eight. Every copy between slots moves eight bytes, which is safe because an `i32` is only ever
//! read at four. The frame is `sub rsp, size` after the prologue, rounded up to 16, and a frame
//! larger than one page is refused: on Windows a larger frame needs stack probes, which this
//! encoder does not generate.
//!
//! Control flow: the blocks reachable from the entry are emitted one after another. A jump to a
//! block is recorded as a fixup and patched once every block has an offset. A branch copies its
//! values into the target's parameter slots, through temporaries, before it jumps.
//!
//! Traps: a trap stores its status in the context and returns zero. The context is passed in
//! the second integer argument register and kept in a slot.

use std::collections::{HashMap, HashSet, VecDeque};

use tpt_wasm_ir::{
    BasicBlock, BlockId, FloatComparison, FloatConversion, FloatTrunc, FloatUnary, IntComparison,
    IntConversion, IntUnary, IrFunction, IrInstr, MemoryLoad, MemoryStore, Reinterpret, SignExtend,
    Terminator, ValueId,
};
use tpt_wasm_types::ValueType;

use crate::{
    allocate_local_registers, allocate_registers, call_result, is_register_instruction,
    makes_calls, CallEnv, Context, JitError, STATUS_CALL_DEPTH, STATUS_GLOBAL_OUT_OF_BOUNDS,
    STATUS_INDIRECT_TYPE_MISMATCH, STATUS_INVALID_CONVERSION, STATUS_NULL_REFERENCE,
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

/// Register numbers in the ModRM `reg` and `rm` fields.
const EAX: u8 = 0;
const ECX: u8 = 1;
const EDX: u8 = 2;
const RBP_ENCODING: u8 = 5;

/// The largest frame the encoder emits without stack probes: one page.
const MAX_FRAME: i32 = 4096;

/// The byte offset of the trap status inside the context.
const STATUS_OFFSET: u8 = std::mem::offset_of!(Context, status) as u8;
/// The byte offset of the memory length inside the context.
const MEMORY_LENGTH_OFFSET: u8 = std::mem::offset_of!(Context, memory_len) as u8;
const FUNCTIONS_OFFSET: u8 = std::mem::offset_of!(Context, functions) as u8;
const DEPTH_OFFSET: u8 = std::mem::offset_of!(Context, depth) as u8;
const DEPTH_LIMIT_OFFSET: u8 = std::mem::offset_of!(Context, depth_limit) as u8;
const TABLE_ENTRIES_OFFSET: u8 = std::mem::offset_of!(Context, table_entries) as u8;
const TABLE_TYPES_OFFSET: u8 = std::mem::offset_of!(Context, table_types) as u8;
const TABLE_LEN_OFFSET: u8 = std::mem::offset_of!(Context, table_len) as u8;
const GLOBALS_OFFSET: u8 = std::mem::offset_of!(Context, globals) as u8;
const GLOBALS_LENGTH_OFFSET: u8 = std::mem::offset_of!(Context, globals_len) as u8;
const MEMORY_CAPACITY_OFFSET: u8 = std::mem::offset_of!(Context, memory_capacity) as u8;

/// Two-operand opcodes with `rax` as the destination and a memory source.
const OP_ADD: &[u8] = &[0x03];
const OP_SUB: &[u8] = &[0x2B];
const OP_AND: &[u8] = &[0x23];
const OP_OR: &[u8] = &[0x0B];
const OP_XOR: &[u8] = &[0x33];
const OP_IMUL: &[u8] = &[0x0F, 0xAF];
const OP_CMP: &[u8] = &[0x3B];

/// The condition code (the low nibble of `setcc` and `jcc`) for each integer comparison.
fn condition_code(comparison: IntComparison) -> u8 {
    match comparison {
        IntComparison::Eq => 0x4,
        IntComparison::Ne => 0x5,
        IntComparison::LtS => 0xC,
        IntComparison::LtU => 0x2,
        IntComparison::GtS => 0xF,
        IntComparison::GtU => 0x7,
        IntComparison::LeS => 0xE,
        IntComparison::LeU => 0x6,
        IntComparison::GeS => 0xD,
        IntComparison::GeU => 0x3,
    }
}

/// The stack slots of a function, by value, and the type of each value.
struct Frame {
    slots: HashMap<ValueId, i32>,
    types: HashMap<ValueId, ValueType>,
    used: i32,
    /// The slot holding the context pointer.
    context: i32,
    /// What the function's calls may name, and the table they may index.
    env: CallEnv,
    abi: Abi,
    /// Bytes reserved below the slots for a callee's shadow space. Set when the function calls.
    shadow: i32,
    /// The registers of the values that live in one, in a function that makes no call.
    registers: HashMap<ValueId, u8>,
    /// Whether the host has SSE4.1, which the rounding functions need.
    sse41: bool,
}

impl Frame {
    fn new(function: &IrFunction, abi: Abi, env: &CallEnv, sse41: bool) -> Self {
        Self {
            slots: HashMap::new(),
            types: function
                .values
                .iter()
                .map(|value| (value.id, value.value_type))
                .collect(),
            used: 0,
            context: 0,
            env: env.clone(),
            abi,
            shadow: 0,
            sse41,
            registers: HashMap::new(),
        }
    }

    /// A block of `words` consecutive slots, and the offset of its first word. The words ascend
    /// in memory from that offset, so the block can be passed as an array of arguments.
    fn block(&mut self, words: usize) -> i32 {
        self.used += 8 * words as i32;
        -self.used
    }

    fn is_callable(&self, function: u32) -> bool {
        self.env
            .callable
            .get(function as usize)
            .copied()
            .unwrap_or(false)
    }

    /// The `rbp`-relative offset of a value's slot, allocated on first use.
    fn slot(&mut self, value: ValueId) -> i32 {
        if let Some(&offset) = self.slots.get(&value) {
            return offset;
        }
        self.used += 8;
        let offset = -self.used;
        self.slots.insert(value, offset);
        offset
    }

    /// A fresh slot that no value owns.
    fn temporary(&mut self) -> i32 {
        self.used += 8;
        -self.used
    }

    /// The frame size: the slots and any shadow space, rounded up to a multiple of 16, which keeps
    /// the stack aligned at every call.
    fn size(&self) -> i32 {
        (self.used + self.shadow + 15) / 16 * 16
    }

    fn value_type(&self, value: ValueId) -> Option<ValueType> {
        self.types.get(&value).copied()
    }
}

/// A place a jump can go: a block of the function, or a label inside the emitted code.
#[derive(Debug, Clone, Copy)]
enum Target {
    Block(BlockId),
    Label(usize),
}

struct Emitter {
    bytes: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Target)>,
    /// The callee-saved registers the function uses, each with the slot its value was saved to. Every
    /// return restores them.
    restores: Vec<(u8, i32)>,
}

impl Emitter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            restores: Vec::new(),
        }
    }

    /// `[REX] opcode ModRM disp32` with `rbp` as the base. `wide` sets REX.W. A register number of
    /// 8 or more sets REX.R.
    fn memory_with(&mut self, wide: bool, opcode: &[u8], reg: u8, disp: i32) {
        let rex = 0x40 | if wide { 0x08 } else { 0 } | if reg >= 8 { 0x04 } else { 0 };
        if rex != 0x40 {
            self.bytes.push(rex);
        }
        self.bytes.extend_from_slice(opcode);
        self.bytes.push(0x80 | ((reg & 7) << 3) | RBP_ENCODING);
        self.bytes.extend_from_slice(&disp.to_le_bytes());
    }

    /// A 32-bit memory operand at `rbp + disp`.
    fn memory(&mut self, opcode: &[u8], reg: u8, disp: i32) {
        self.memory_with(false, opcode, reg, disp);
    }

    /// A 64-bit memory operand at `rbp + disp`.
    fn memory_wide(&mut self, opcode: &[u8], reg: u8, disp: i32) {
        self.memory_with(true, opcode, reg, disp);
    }

    /// `mov eax, [rbp + disp]`: a 32-bit load, zero-extended into `rax`.
    fn load32(&mut self, reg: u8, disp: i32) {
        self.memory(&[0x8B], reg, disp);
    }

    /// `mov rax, [rbp + disp]`.
    fn load64(&mut self, reg: u8, disp: i32) {
        self.memory_wide(&[0x8B], reg, disp);
    }

    /// `mov [rbp + disp], eax`: the low 32 bits.
    fn store32(&mut self, reg: u8, disp: i32) {
        self.memory(&[0x89], reg, disp);
    }

    /// `mov [rbp + disp], rax`.
    fn store64(&mut self, reg: u8, disp: i32) {
        self.memory_wide(&[0x89], reg, disp);
    }

    /// `mov dword [rbp + disp], imm32`.
    fn store_immediate32(&mut self, disp: i32, value: i32) {
        self.memory(&[0xC7], 0, disp);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// `mov qword [rbp + disp], 0`.
    fn zero64(&mut self, disp: i32) {
        self.memory_wide(&[0xC7], 0, disp);
        self.bytes.extend_from_slice(&0i32.to_le_bytes());
    }

    /// `cmp dword [rbp + disp], 0`.
    fn compare_with_zero32(&mut self, disp: i32) {
        self.memory(&[0x83], 7, disp);
        self.bytes.push(0x00);
    }

    /// `cmp qword [rbp + disp], 0`.
    fn compare_with_zero64(&mut self, disp: i32) {
        self.memory_wide(&[0x83], 7, disp);
        self.bytes.push(0x00);
    }

    /// `movabs <reg>, imm64`.
    fn movabs(&mut self, reg: u8, value: i64) {
        self.bytes.extend_from_slice(&[0x48, 0xB8 | (reg & 7)]);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// `setcc al; movzx eax, al`: the flags as a 0 or 1 in `eax`.
    fn set_from_flags(&mut self, condition: u8) {
        self.bytes
            .extend_from_slice(&[0x0F, 0x90 | condition, 0xC0]);
        self.bytes.extend_from_slice(&[0x0F, 0xB6, 0xC0]);
    }

    /// `movss xmm0, [rbp + disp]` (`prefix` 0xF3) or `movsd xmm0, [rbp + disp]` (0xF2).
    fn scalar_load_xmm0(&mut self, prefix: u8, disp: i32) {
        self.bytes.push(prefix);
        self.memory(&[0x0F, 0x10], 0, disp);
    }

    /// `addss`/`subss`/`mulss`/`divss` (or the `sd` forms) `xmm0, [rbp + disp]`, selected by
    /// `opcode` (0x58, 0x5C, 0x59, 0x5E) and the same `prefix` as the load.
    fn scalar_op_xmm0(&mut self, prefix: u8, opcode: u8, disp: i32) {
        self.bytes.push(prefix);
        self.memory(&[0x0F, opcode], 0, disp);
    }

    /// `movd eax, xmm0` or, when `wide`, `movq rax, xmm0`. The 32-bit form zero-extends.
    fn move_xmm0_to_rax(&mut self, wide: bool) {
        if wide {
            self.bytes
                .extend_from_slice(&[0x66, 0x48, 0x0F, 0x7E, 0xC0]);
        } else {
            self.bytes.extend_from_slice(&[0x66, 0x0F, 0x7E, 0xC0]);
        }
    }

    /// `ucomiss xmm0, [rbp + disp]` (or `ucomisd` when `wide`). Sets ZF, PF and CF; an unordered
    /// pair sets all three, so it reads as neither "above" nor "equal".
    fn unordered_compare_xmm0(&mut self, wide: bool, disp: i32) {
        if wide {
            self.bytes.push(0x66);
        }
        self.memory(&[0x0F, 0x2E], 0, disp);
    }

    /// A shift of `rax` by `cl`, 64 bits wide: `shl` (/4), `shr` (/5), or `sar` (/7).
    fn shift_by_cl_wide(&mut self, extension: u8) {
        self.bytes
            .extend_from_slice(&[0x48, 0xD3, 0xC0 | (extension << 3)]);
    }

    /// A shift of `eax` by `cl`, 32 bits wide. The hardware masks the count to five bits.
    fn shift_by_cl(&mut self, extension: u8) {
        self.bytes
            .extend_from_slice(&[0xD3, 0xC0 | (extension << 3)]);
    }

    fn new_label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }

    fn bind(&mut self, label: usize) {
        self.labels[label] = Some(self.bytes.len());
    }

    /// A jump or conditional jump to `target`, whose 32-bit displacement is patched later.
    fn jump(&mut self, opcode: &[u8], target: Target) {
        self.bytes.extend_from_slice(opcode);
        self.fixups.push((self.bytes.len(), target));
        self.bytes.extend_from_slice(&[0; 4]);
    }

    /// Patch every jump now that the offset of each block is known.
    fn resolve(&mut self, blocks: &HashMap<BlockId, usize>) -> Result<(), JitError> {
        for (position, target) in std::mem::take(&mut self.fixups) {
            let offset = match target {
                Target::Block(block) => *blocks.get(&block).ok_or_else(|| {
                    JitError::Unsupported("a jump to a block that was not emitted".into())
                })?,
                Target::Label(label) => self.labels[label]
                    .ok_or_else(|| JitError::Unsupported("a jump to an unbound label".into()))?,
            };
            let displacement = offset as i64 - (position as i64 + 4);
            let displacement = i32::try_from(displacement)
                .map_err(|_| JitError::Unsupported("a jump beyond 2 GiB".into()))?;
            self.bytes[position..position + 4].copy_from_slice(&displacement.to_le_bytes());
        }
        Ok(())
    }

    /// `push rbp; mov rbp, rsp; sub rsp, imm32`.
    fn prologue(frame_size: i32) -> Vec<u8> {
        let mut bytes = vec![0x55];
        bytes.extend_from_slice(&[0x48, 0x89, 0xE5]);
        bytes.extend_from_slice(&[0x48, 0x81, 0xEC]);
        bytes.extend_from_slice(&frame_size.to_le_bytes());
        bytes
    }

    /// Return from the function with `status` stored in the context. The return value is zero.
    fn trap(&mut self, frame: &Frame, status: u32) {
        // mov rcx, [rbp + context]; mov qword [rcx + STATUS], status; xor eax, eax
        self.memory_wide(&[0x8B], ECX, frame.context);
        self.bytes
            .extend_from_slice(&[0x48, 0xC7, 0x41, STATUS_OFFSET]);
        self.bytes.extend_from_slice(&(status as i32).to_le_bytes());
        self.bytes.extend_from_slice(&[0x31, 0xC0]);
        self.epilogue();
    }

    /// `leave; ret`.
    fn epilogue(&mut self) {
        for (register, slot) in self.restores.clone() {
            self.memory_with(true, &[0x8B], register, slot); // mov register, [slot]
        }
        self.bytes.push(0xC9);
        self.bytes.push(0xC3);
    }
}

/// Compile `function` to machine code for `abi`.
pub(crate) fn encode(
    function: &IrFunction,
    abi: Abi,
    env: &CallEnv,
    sse41: bool,
) -> Result<Vec<u8>, JitError> {
    check_supported(function)?;
    let blocks: HashMap<BlockId, &BasicBlock> = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();

    let mut frame = Frame::new(function, abi, env, sse41);
    if !makes_calls(function) {
        let locals =
            allocate_local_registers(function, &VALUE_REGISTERS, 4, is_x86_register_instruction);
        let remaining: Vec<u8> = VALUE_REGISTERS
            .iter()
            .copied()
            .filter(|register| !locals.values().any(|taken| taken == register))
            .collect();
        frame.registers = locals;
        frame.registers.extend(allocate_registers(
            function,
            &remaining,
            true,
            is_x86_register_instruction,
        ));
    }
    let mut emitter = Emitter::new();
    // Save each callee-saved register the function uses, in order, before the body runs.
    let mut saved: Vec<u8> = frame
        .registers
        .values()
        .copied()
        .filter(|register| CALLEE_SAVED.contains(register))
        .collect();
    saved.sort_unstable();
    saved.dedup();
    for register in saved {
        let slot = frame.temporary();
        emitter.memory_with(true, &[0x89], register, slot); // mov [slot], register
        emitter.restores.push((register, slot));
    }

    // The two pointers arrive in argument registers. They are kept in slots, because the code
    // below uses every scratch register. Each argument is an 8-byte slot in the arguments array.
    let args = frame.temporary();
    frame.context = frame.temporary();
    emitter.memory_wide(&[0x89], abi.args_register(), args);
    emitter.memory_wide(&[0x89], abi.context_register(), frame.context);
    for (index, parameter) in function.params.iter().enumerate() {
        emitter.memory_wide(&[0x8B], EAX, args); // mov rax, [args]
                                                 // mov rax, [rax + 8 * index]
        emitter.bytes.extend_from_slice(&[0x48, 0x8B, 0x80]);
        emitter
            .bytes
            .extend_from_slice(&(8 * index as i32).to_le_bytes());
        match frame.register(*parameter) {
            Some(register) => emit_register_form(&mut emitter, true, &[0x89], EAX, register),
            None => {
                let slot = frame.slot(*parameter);
                emitter.store64(EAX, slot);
            }
        }
    }
    // A declared local starts at zero, as Wasm requires; parameters are already set.
    let parameters: HashSet<ValueId> = function.params.iter().copied().collect();
    for local in &function.locals {
        if !parameters.contains(local) {
            match frame.register(*local) {
                Some(register) => {
                    emit_register_form(&mut emitter, true, &[0x31], register, register)
                }
                None => {
                    let slot = frame.slot(*local);
                    emitter.zero64(slot);
                }
            }
        }
    }

    // The blocks reachable from the entry, each emitted once.
    let mut offsets: HashMap<BlockId, usize> = HashMap::new();
    let mut queue: VecDeque<BlockId> = VecDeque::from([function.entry]);
    let mut scheduled: HashSet<BlockId> = HashSet::from([function.entry]);
    while let Some(id) = queue.pop_front() {
        let block = blocks
            .get(&id)
            .copied()
            .ok_or_else(|| JitError::Unsupported("a branch to a missing block".into()))?;
        offsets.insert(id, emitter.bytes.len());
        for instruction in &block.instrs {
            emit_instruction(instruction, &mut frame, &mut emitter)?;
        }
        for successor in emit_terminator(block, &blocks, &mut frame, &mut emitter)? {
            if scheduled.insert(successor) {
                queue.push_back(successor);
            }
        }
    }
    emitter.resolve(&offsets)?;

    let size = frame.size();
    if size > MAX_FRAME {
        return Err(JitError::Unsupported(format!(
            "a frame of {size} bytes; the largest emitted is {MAX_FRAME}"
        )));
    }
    let mut code = Emitter::prologue(size);
    code.extend(emitter.bytes);
    Ok(code)
}

/// Emit a block's terminator, and return the blocks it can transfer control to.
fn emit_terminator(
    block: &BasicBlock,
    blocks: &HashMap<BlockId, &BasicBlock>,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<Vec<BlockId>, JitError> {
    match &block.terminator {
        Terminator::Return(values) if values.len() == 1 => {
            match frame.register(values[0]) {
                Some(register) => emit_register_form(emitter, true, &[0x89], register, EAX), // mov rax, register
                None => {
                    let slot = frame.slot(values[0]);
                    // A 32-bit result is loaded at 32 bits, which zero-extends; a 64-bit one at 64.
                    match frame.value_type(values[0]) {
                        Some(ValueType::I64 | ValueType::F64) => emitter.load64(EAX, slot),
                        _ => emitter.load32(EAX, slot),
                    }
                }
            }
            emitter.epilogue();
            Ok(Vec::new())
        }
        Terminator::Return(values) if values.is_empty() => {
            // A function with no result returns zero, which its caller ignores.
            emitter.bytes.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax
            emitter.epilogue();
            Ok(Vec::new())
        }
        Terminator::Unreachable => {
            emitter.trap(frame, STATUS_UNREACHABLE);
            Ok(Vec::new())
        }
        Terminator::Return(values) => Err(JitError::Unsupported(format!(
            "a return of {} values",
            values.len()
        ))),
        Terminator::Branch { target, values } => {
            let next = target_block(blocks, *target)?;
            check_arity(next, values)?;
            copy_into_parameters(&next.params, values, frame, emitter);
            emitter.jump(&[0xE9], Target::Block(*target));
            Ok(vec![*target])
        }
        Terminator::CondBranch {
            condition,
            then_target,
            then_values,
            else_target,
            else_values,
        } => {
            let then_block = target_block(blocks, *then_target)?;
            let else_block = target_block(blocks, *else_target)?;
            check_arity(then_block, then_values)?;
            check_arity(else_block, else_values)?;
            // Zero goes to the else edge, anything else to the then edge.
            match frame.register(*condition) {
                Some(register) => emit_register_form(emitter, false, &[0x85], register, register), // test r32, r32
                None => {
                    let condition = frame.slot(*condition);
                    emitter.compare_with_zero32(condition);
                }
            }
            let else_path = emitter.new_label();
            emitter.jump(&[0x0F, 0x84], Target::Label(else_path)); // je
            copy_into_parameters(&then_block.params, then_values, frame, emitter);
            emitter.jump(&[0xE9], Target::Block(*then_target));
            emitter.bind(else_path);
            copy_into_parameters(&else_block.params, else_values, frame, emitter);
            emitter.jump(&[0xE9], Target::Block(*else_target));
            Ok(vec![*then_target, *else_target])
        }
        other => Err(JitError::Unsupported(format!(
            "the {} terminator",
            terminator_name(other)
        ))),
    }
}

fn target_block<'a>(
    blocks: &HashMap<BlockId, &'a BasicBlock>,
    target: BlockId,
) -> Result<&'a BasicBlock, JitError> {
    blocks
        .get(&target)
        .copied()
        .ok_or_else(|| JitError::Unsupported("a branch to a missing block".into()))
}

fn check_arity(block: &BasicBlock, values: &[ValueId]) -> Result<(), JitError> {
    if block.params.len() != values.len() {
        return Err(JitError::Unsupported(
            "a branch with the wrong arity".into(),
        ));
    }
    Ok(())
}

/// Copy branch values into the target's parameter slots, through temporaries so that the order
/// of the copies cannot change what a value means. Each copy is eight bytes.
fn copy_into_parameters(
    parameters: &[ValueId],
    values: &[ValueId],
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    // A value is copied straight into its parameter unless a later value of the same branch reads that
    // parameter, which the copy would have changed. Then every value goes through a temporary slot first, so
    // each is read before any parameter is written.
    let overwritten = parameters
        .iter()
        .enumerate()
        .any(|(index, parameter)| values[index + 1..].contains(parameter));
    if !overwritten {
        for (parameter, value) in parameters.iter().zip(values) {
            copy_value(*value, *parameter, frame, emitter);
        }
        return;
    }
    // Every value is copied to a temporary slot before any parameter is written, so a value that is also a
    // parameter of the same branch is read before it changes. A register value is stored from its register,
    // and a parameter in a register is loaded into it.
    let temporaries: Vec<i32> = values
        .iter()
        .map(|value| {
            let temporary = frame.temporary();
            match frame.register(*value) {
                Some(register) => emitter.memory_with(true, &[0x89], register, temporary),
                None => {
                    let source = frame.slot(*value);
                    emitter.load64(EAX, source);
                    emitter.store64(EAX, temporary);
                }
            }
            temporary
        })
        .collect();
    for (parameter, temporary) in parameters.iter().zip(temporaries) {
        match frame.register(*parameter) {
            Some(register) => emitter.memory_with(true, &[0x8B], register, temporary),
            None => {
                let destination = frame.slot(*parameter);
                emitter.load64(EAX, temporary);
                emitter.store64(EAX, destination);
            }
        }
    }
}

/// Refuse anything the encoder would otherwise approximate.
pub(crate) fn check_supported(function: &IrFunction) -> Result<(), JitError> {
    if function.blocks.is_empty() {
        return Err(JitError::Unsupported("a function with no blocks".into()));
    }
    if function.function_type.results.0.len() > 1 {
        return Err(JitError::Unsupported(
            "a function that returns more than one value".into(),
        ));
    }
    for value_type in function
        .function_type
        .params
        .0
        .iter()
        .chain(function.function_type.results.0.iter())
    {
        if !is_scalar(*value_type) {
            return Err(JitError::Unsupported(format!(
                "a signature with a {value_type:?} value"
            )));
        }
    }
    if let Some(value) = function
        .values
        .iter()
        .find(|value| !is_scalar(value.value_type))
    {
        return Err(JitError::Unsupported(format!(
            "a value of type {:?}",
            value.value_type
        )));
    }
    Ok(())
}

/// The types the compiler generates code for. A float is held in its slot as its raw bits, so
/// the integer moves carry it unchanged and only the arithmetic needs the float forms.
fn is_scalar(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::I32 | ValueType::I64 | ValueType::F32 | ValueType::F64
    )
}

/// Scalar arithmetic on floats held as raw bits: the left operand goes into `xmm0`, the right
/// operates on it from its slot, and the result's bits go back to its slot. The slots are
/// zero-extended, so an `f32` never leaves stale bits in its upper half.
fn float_binary(
    opcode: u8,
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let prefix = if wide { 0xF2 } else { 0xF3 };
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    emitter.scalar_load_xmm0(prefix, left);
    emitter.scalar_op_xmm0(prefix, opcode, right);
    canonicalize_nan_xmm0(wide, emitter);
    emitter.move_xmm0_to_rax(wide);
    emitter.store64(EAX, destination);
}

/// Replace a NaN in `xmm0` with the canonical NaN that Micro produces, so the bits agree. The
/// hardware's own NaN differs: x86 makes a negative one for `inf - inf`, for example.
///
/// `ucomis xmm0, xmm0` is unordered exactly for a NaN, which sets PF, so the replacement is
/// skipped when PF is clear.
fn canonicalize_nan_xmm0(wide: bool, emitter: &mut Emitter) {
    if wide {
        emitter.bytes.push(0x66);
    }
    emitter.bytes.extend_from_slice(&[0x0F, 0x2E, 0xC0]); // ucomis xmm0, xmm0
    let jump = emitter.bytes.len();
    emitter.bytes.extend_from_slice(&[0x7B, 0]); // jnp, patched below
    let block = emitter.bytes.len();
    if wide {
        emitter.movabs(EAX, CANONICAL_F64 as i64);
        emitter
            .bytes
            .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0]); // movq xmm0, rax
    } else {
        emitter.bytes.push(0xB8); // mov eax, imm32
        emitter
            .bytes
            .extend_from_slice(&CANONICAL_F32.to_le_bytes());
        emitter.bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, 0xC0]); // movd xmm0, eax
    }
    emitter.bytes[jump + 1] =
        u8::try_from(emitter.bytes.len() - block).expect("the replacement is a few instructions");
}

/// The canonical NaNs Micro's deterministic mode produces for arithmetic.
const CANONICAL_F32: u32 = 0x7fc0_0000;
const CANONICAL_F64: u64 = 0x7ff8_0000_0000_0000;

/// A float comparison as a 0 or 1 in the result slot.
///
/// The ordered comparisons use `ucomis`, whose "above" flags are false for an unordered pair,
/// so `<` and `<=` swap the operands to reuse "above". `==` needs the zero flag and no parity;
/// `!=` is its exact complement, so a NaN compares unequal to everything, as Wasm requires.
fn float_compare(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    comparison: FloatComparison,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let prefix = if wide { 0xF2 } else { 0xF3 };
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    const ABOVE: u8 = 0x7;
    const ABOVE_OR_EQUAL: u8 = 0x3;
    match comparison {
        FloatComparison::Eq | FloatComparison::Ne => {
            emitter.scalar_load_xmm0(prefix, left);
            emitter.unordered_compare_xmm0(wide, right);
            if comparison == FloatComparison::Eq {
                // sete al; setnp cl; and al, cl
                emitter
                    .bytes
                    .extend_from_slice(&[0x0F, 0x94, 0xC0, 0x0F, 0x9B, 0xC1, 0x20, 0xC8]);
            } else {
                // setne al; setp cl; or al, cl
                emitter
                    .bytes
                    .extend_from_slice(&[0x0F, 0x95, 0xC0, 0x0F, 0x9A, 0xC1, 0x08, 0xC8]);
            }
            emitter.bytes.extend_from_slice(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
        }
        FloatComparison::Gt | FloatComparison::Ge => {
            let condition = if comparison == FloatComparison::Gt {
                ABOVE
            } else {
                ABOVE_OR_EQUAL
            };
            emitter.scalar_load_xmm0(prefix, left);
            emitter.unordered_compare_xmm0(wide, right);
            emitter.set_from_flags(condition);
        }
        FloatComparison::Lt | FloatComparison::Le => {
            let condition = if comparison == FloatComparison::Lt {
                ABOVE
            } else {
                ABOVE_OR_EQUAL
            };
            emitter.scalar_load_xmm0(prefix, right);
            emitter.unordered_compare_xmm0(wide, left);
            emitter.set_from_flags(condition);
        }
    }
    emitter.store64(EAX, destination);
}

/// Negation and absolute value, done on the sign bit rather than in the FPU: clearing or
/// flipping it is exact for every input, NaN included, and it is what the reference does.
fn float_unary(
    wide: bool,
    result: ValueId,
    value: ValueId,
    operation: FloatUnary,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    let value = frame.slot(value);
    let destination = frame.slot(result);
    if matches!(
        operation,
        FloatUnary::Ceil | FloatUnary::Floor | FloatUnary::Trunc | FloatUnary::Nearest
    ) {
        if !frame.sse41 {
            return Err(JitError::Unsupported(
                "a rounding function, which needs SSE4.1 on this host".into(),
            ));
        }
        // roundss/roundsd xmm0, xmm0, imm8. The low two bits of the immediate pick the mode: 0 for
        // nearest-even, 1 down (floor), 2 up (ceil), 3 toward zero (trunc). Bit 3 suppresses the
        // precision exception, which Wasm does not expose.
        let mode: u8 = match operation {
            FloatUnary::Nearest => 0x8,
            FloatUnary::Floor => 0x9,
            FloatUnary::Ceil => 0xA,
            _ => 0xB,
        };
        let opcode = if wide { 0x0B } else { 0x0A };
        emitter.scalar_load_xmm0(if wide { 0xF2 } else { 0xF3 }, value);
        emitter
            .bytes
            .extend_from_slice(&[0x66, 0x0F, 0x3A, opcode, 0xC0, mode]);
        canonicalize_nan_xmm0(wide, emitter);
        emitter.move_xmm0_to_rax(wide);
        emitter.store64(EAX, destination);
        return Ok(());
    }
    if operation == FloatUnary::Sqrt {
        // sqrtss/sqrtsd xmm0, xmm0. A negative operand gives the hardware's NaN, which is replaced.
        let prefix = if wide { 0xF2 } else { 0xF3 };
        emitter.scalar_load_xmm0(prefix, value);
        emitter.bytes.extend_from_slice(&[prefix, 0x0F, 0x51, 0xC0]);
        canonicalize_nan_xmm0(wide, emitter);
        emitter.move_xmm0_to_rax(wide);
        emitter.store64(EAX, destination);
        return Ok(());
    }
    match (wide, operation) {
        (false, FloatUnary::Neg) => {
            // mov eax, [value]; xor eax, 0x8000_0000
            emitter.load32(EAX, value);
            emitter.bytes.push(0x35);
            emitter
                .bytes
                .extend_from_slice(&0x8000_0000u32.to_le_bytes());
        }
        (false, FloatUnary::Abs) => {
            // mov eax, [value]; and eax, 0x7fff_ffff
            emitter.load32(EAX, value);
            emitter.bytes.push(0x25);
            emitter
                .bytes
                .extend_from_slice(&0x7fff_ffffu32.to_le_bytes());
        }
        (true, FloatUnary::Neg) => {
            // mov rax, [value]; movabs rcx, 1 << 63; xor rax, rcx
            emitter.load64(EAX, value);
            emitter.movabs(ECX, i64::MIN);
            emitter.bytes.extend_from_slice(&[0x48, 0x31, 0xC8]);
        }
        (true, FloatUnary::Abs) => {
            // mov rax, [value]; movabs rcx, i64::MAX; and rax, rcx
            emitter.load64(EAX, value);
            emitter.movabs(ECX, i64::MAX);
            emitter.bytes.extend_from_slice(&[0x48, 0x21, 0xC8]);
        }
        (_, other) => {
            return Err(JitError::Unsupported(format!(
                "the {other:?} float operation"
            )))
        }
    }
    emitter.store64(EAX, destination);
    Ok(())
}

/// `min` or, when `maximum`, `max`, with Micro's rules. A NaN in either operand gives the canonical
/// NaN. Two equal values have the same bits, except for zeros of different sign, where the sign bits
/// decide: `min` takes the OR of the bits, negative if either is, and `max` the AND, positive if
/// either is. Otherwise `min` takes the smaller operand and `max` the larger.
fn float_min_max(
    wide: bool,
    maximum: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    // eax = left, ecx = right; then xmm0 and xmm1 hold the same two values.
    if wide {
        emitter.load64(EAX, left);
        emitter.load64(ECX, right);
        emitter
            .bytes
            .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0]); // movq xmm0, rax
        emitter
            .bytes
            .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC9]); // movq xmm1, rcx
        emitter.bytes.extend_from_slice(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
    } else {
        emitter.load32(EAX, left);
        emitter.load32(ECX, right);
        emitter.bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, 0xC0]); // movd xmm0, eax
        emitter.bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, 0xC9]); // movd xmm1, ecx
        emitter.bytes.extend_from_slice(&[0x0F, 0x2E, 0xC1]); // ucomiss xmm0, xmm1
    }
    let nan = emitter.new_label();
    let unequal = emitter.new_label();
    let less = emitter.new_label();
    let done = emitter.new_label();
    emitter.jump(&[0x0F, 0x8A], Target::Label(nan)); // jp: unordered, a NaN
    emitter.jump(&[0x0F, 0x85], Target::Label(unequal)); // jne
                                                         // Equal: the bits combine. `or` for `min`, `and` for `max`.
    let combine: &[u8] = match (wide, maximum) {
        (true, false) => &[0x48, 0x09, 0xC8],
        (true, true) => &[0x48, 0x21, 0xC8],
        (false, false) => &[0x09, 0xC8],
        (false, true) => &[0x21, 0xC8],
    };
    emitter.bytes.extend_from_slice(combine);
    emitter.jump(&[0xE9], Target::Label(done));
    emitter.bind(unequal);
    emitter.jump(&[0x0F, 0x82], Target::Label(less)); // jb: left < right
                                                      // Greater: `min` takes the right operand, `max` keeps the left.
    if !maximum {
        emitter.bytes.extend_from_slice(if wide {
            &[0x48, 0x89, 0xC8]
        } else {
            &[0x89, 0xC8]
        }); // mov rax/eax, rcx/ecx
    }
    emitter.jump(&[0xE9], Target::Label(done));
    emitter.bind(less);
    // Less: `max` takes the right operand, `min` keeps the left.
    if maximum {
        emitter.bytes.extend_from_slice(if wide {
            &[0x48, 0x89, 0xC8]
        } else {
            &[0x89, 0xC8]
        });
    }
    emitter.jump(&[0xE9], Target::Label(done));
    emitter.bind(nan);
    if wide {
        emitter.movabs(EAX, CANONICAL_F64 as i64);
    } else {
        emitter.bytes.push(0xB8); // mov eax, imm32
        emitter
            .bytes
            .extend_from_slice(&CANONICAL_F32.to_le_bytes());
    }
    emitter.bind(done);
    emitter.store64(EAX, destination);
}

/// `copysign`: the magnitude of the left operand with the sign of the right, bit for bit, so a NaN
/// on the left keeps its payload, as Micro's `copysign` does.
fn float_copysign(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    if wide {
        emitter.load64(EAX, left);
        emitter.load64(ECX, right);
        emitter.movabs(EDX, i64::MAX);
        emitter.bytes.extend_from_slice(&[0x48, 0x21, 0xD0]); // and rax, rdx
        emitter.movabs(EDX, i64::MIN);
        emitter.bytes.extend_from_slice(&[0x48, 0x21, 0xD1]); // and rcx, rdx
        emitter.bytes.extend_from_slice(&[0x48, 0x09, 0xC8]); // or rax, rcx
    } else {
        emitter.load32(EAX, left);
        emitter.load32(ECX, right);
        emitter.bytes.push(0x25); // and eax, 0x7fff_ffff
        emitter
            .bytes
            .extend_from_slice(&0x7fff_ffffu32.to_le_bytes());
        emitter.bytes.extend_from_slice(&[0x81, 0xE1]); // and ecx, 0x8000_0000
        emitter
            .bytes
            .extend_from_slice(&0x8000_0000u32.to_le_bytes());
        emitter.bytes.extend_from_slice(&[0x09, 0xC8]); // or eax, ecx
    }
    emitter.store64(EAX, destination);
}

/// A float to an integer, truncating toward zero, which traps as Micro does: a NaN, an infinity, or
/// a truncated value outside the target's range is an invalid conversion.
///
/// The value is truncated first with `roundsd`, which needs SSE4.1, and then checked against
/// bounds that are exact powers of two in both widths: `t >= lo` and `hi > t`, each written so that a
/// NaN fails it. The conversion is then exact. An unsigned 64-bit target above 2^63 is reduced by
/// 2^63, converted, and given the top bit back.
fn float_trunc_into(
    operation: FloatTrunc,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    if !frame.sse41 {
        return Err(JitError::Unsupported(
            "a truncation, which needs SSE4.1 on this host".into(),
        ));
    }
    let (wide, signed, to_64) = match operation {
        FloatTrunc::I32FromF32S => (false, true, false),
        FloatTrunc::I32FromF32U => (false, false, false),
        FloatTrunc::I32FromF64S => (true, true, false),
        FloatTrunc::I32FromF64U => (true, false, false),
        FloatTrunc::I64FromF32S => (false, true, true),
        FloatTrunc::I64FromF32U => (false, false, true),
        FloatTrunc::I64FromF64S => (true, true, true),
        FloatTrunc::I64FromF64U => (true, false, true),
    };
    // The target's range, as the truncated values it accepts: [lo, hi).
    let (lo, hi) = match (signed, to_64) {
        (true, false) => (-2147483648.0f64, 2147483648.0f64),
        (false, false) => (0.0, 4294967296.0),
        (true, true) => (-9223372036854775808.0, 9223372036854775808.0),
        (false, true) => (0.0, 18446744073709551616.0),
    };
    let bits = |value: f64| -> i64 {
        if wide {
            value.to_bits() as i64
        } else {
            i64::from((value as f32).to_bits())
        }
    };
    let (lo_bits, hi_bits) = (bits(lo), bits(hi));
    let prefix = if wide { 0xF2 } else { 0xF3 };
    let source = frame.slot(value);
    let destination = frame.slot(result);

    // xmm0 = trunc(x), by roundss/roundsd with the truncating mode.
    emitter.scalar_load_xmm0(prefix, source);
    emitter.bytes.extend_from_slice(&[
        0x66,
        0x0F,
        0x3A,
        if wide { 0x0B } else { 0x0A },
        0xC0,
        0x0B,
    ]);
    // Trap unless t >= lo: `ucomis xmm0, xmm1` sets CF for t < lo, and for a NaN.
    emitter.movabs(ECX, lo_bits);
    move_rcx_to_xmm(wide, 1, emitter);
    ucomis_xmm(wide, 0, 1, emitter);
    guard_trap(frame, 0x73, STATUS_INVALID_CONVERSION, emitter)?; // jae: CF clear
                                                                  // Trap unless hi > t: `ucomis xmm2, xmm0` is "above" only when hi > t and neither is a NaN.
    emitter.movabs(ECX, hi_bits);
    move_rcx_to_xmm(wide, 2, emitter);
    ucomis_xmm(wide, 2, 0, emitter);
    guard_trap(frame, 0x77, STATUS_INVALID_CONVERSION, emitter)?; // ja: above

    let cvtt = |emitter: &mut Emitter, long: bool| {
        emitter.bytes.push(prefix);
        if long {
            emitter.bytes.push(0x48);
        }
        emitter.bytes.extend_from_slice(&[0x0F, 0x2C, 0xC0]); // cvtt{s,d}2si rax|eax, xmm0
    };
    match (signed, to_64) {
        (true, false) => cvtt(emitter, false),
        (false, false) | (true, true) => cvtt(emitter, true),
        (false, true) => {
            // xmm1 = 2^63; at or above it, reduce by 2^63, convert, and set the top bit again.
            let small = emitter.new_label();
            let done = emitter.new_label();
            emitter.movabs(ECX, two_to_the_63_bits(wide));
            move_rcx_to_xmm(wide, 1, emitter);
            ucomis_xmm(wide, 0, 1, emitter);
            emitter.jump(&[0x0F, 0x82], Target::Label(small)); // jb: below 2^63
            emitter.bytes.extend_from_slice(&[prefix, 0x0F, 0x5C, 0xC1]); // sub xmm0, xmm1
            cvtt(emitter, true);
            emitter.movabs(ECX, i64::MIN);
            emitter.bytes.extend_from_slice(&[0x48, 0x31, 0xC8]); // xor rax, rcx
            emitter.jump(&[0xE9], Target::Label(done));
            emitter.bind(small);
            cvtt(emitter, true);
            emitter.bind(done);
        }
    }
    emitter.store64(EAX, destination);
    Ok(())
}

/// The bits of 2^63 at the source width.
fn two_to_the_63_bits(wide: bool) -> i64 {
    if wide {
        (9223372036854775808.0f64).to_bits() as i64
    } else {
        i64::from((9223372036854775808.0f32).to_bits())
    }
}

/// `ucomisd xmm<a>, xmm<b>` (`wide`) or `ucomiss`: compares, setting CF for "below", ZF for "equal"
/// and PF for "unordered".
fn ucomis_xmm(wide: bool, a: u8, b: u8, emitter: &mut Emitter) {
    if wide {
        emitter.bytes.push(0x66);
    }
    emitter
        .bytes
        .extend_from_slice(&[0x0F, 0x2E, 0xC0 | (a << 3) | b]);
}

/// `movq xmm<n>, rcx` (`wide`) or `movd xmm<n>, ecx`: a constant's bits into `xmm<n>`.
fn move_rcx_to_xmm(wide: bool, register: u8, emitter: &mut Emitter) {
    let modrm = 0xC0 | (register << 3) | 1; // reg = xmm<n>, rm = rcx
    if wide {
        emitter
            .bytes
            .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, modrm]);
    } else {
        emitter.bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, modrm]);
    }
}

/// `movq xmm0, rax` (`wide`) or `movd xmm0, eax`: a value's bits into `xmm0`.
fn move_rax_to_xmm0(wide: bool, emitter: &mut Emitter) {
    if wide {
        emitter
            .bytes
            .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0]);
    } else {
        emitter.bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, 0xC0]);
    }
}

/// A conversion between widths, or from an integer to a float. Each is the hardware conversion,
/// which rounds to nearest-even as Micro's `as` does. An unsigned 64-bit source is converted by
/// halving it, keeping the lowest bit as a sticky bit so the rounding stays correct, and doubling
/// the result. Float to integer conversions trap, and are not compiled here.
fn float_convert_into(
    operation: FloatConversion,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    let source = frame.slot(value);
    let destination = frame.slot(result);
    let to_f64 = matches!(
        operation,
        FloatConversion::F64FromI32S
            | FloatConversion::F64FromI32U
            | FloatConversion::F64FromI64S
            | FloatConversion::F64FromI64U
            | FloatConversion::F64FromF32
    );
    let prefix = if to_f64 { 0xF2 } else { 0xF3 };
    match operation {
        FloatConversion::F32FromI32S | FloatConversion::F64FromI32S => {
            // The 32-bit form reads the sign of the low half.
            emitter.load32(EAX, source);
            emitter.bytes.extend_from_slice(&[prefix, 0x0F, 0x2A, 0xC0]); // cvtsi2ss/sd xmm0, eax
        }
        FloatConversion::F32FromI32U | FloatConversion::F64FromI32U => {
            // The zero-extended value is non-negative, so the 64-bit signed form is exact.
            emitter.load32(EAX, source);
            emitter
                .bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2ss/sd xmm0, rax
        }
        FloatConversion::F32FromI64S | FloatConversion::F64FromI64S => {
            emitter.load64(EAX, source);
            emitter
                .bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC0]);
        }
        FloatConversion::F32FromI64U | FloatConversion::F64FromI64U => {
            let signed = emitter.new_label();
            let done = emitter.new_label();
            emitter.load64(EAX, source);
            emitter.bytes.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
            emitter.jump(&[0x0F, 0x89], Target::Label(signed)); // jns: below 2^63
                                                                // rcx = (rax >> 1) | (rax & 1), converted and doubled.
            emitter.bytes.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
            emitter.bytes.extend_from_slice(&[0x48, 0xD1, 0xE9]); // shr rcx, 1
            emitter.bytes.extend_from_slice(&[0x83, 0xE0, 0x01]); // and eax, 1
            emitter.bytes.extend_from_slice(&[0x48, 0x09, 0xC1]); // or rcx, rax
            emitter
                .bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC1]); // cvtsi2 xmm0, rcx
            emitter.bytes.extend_from_slice(&[prefix, 0x0F, 0x58, 0xC0]); // add xmm0, xmm0
            emitter.jump(&[0xE9], Target::Label(done));
            emitter.bind(signed);
            emitter
                .bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2 xmm0, rax
            emitter.bind(done);
        }
        FloatConversion::F32FromF64 => {
            // cvtsd2ss: a NaN result is replaced by the canonical one, as Micro does.
            emitter.load64(EAX, source);
            move_rax_to_xmm0(true, emitter);
            emitter.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5A, 0xC0]);
            canonicalize_nan_xmm0(false, emitter);
            emitter.move_xmm0_to_rax(false);
            emitter.store64(EAX, destination);
            return Ok(());
        }
        FloatConversion::F64FromF32 => {
            emitter.load32(EAX, source);
            move_rax_to_xmm0(false, emitter);
            emitter.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5A, 0xC0]); // cvtss2sd
            canonicalize_nan_xmm0(true, emitter);
            emitter.move_xmm0_to_rax(true);
            emitter.store64(EAX, destination);
            return Ok(());
        }
    }
    emitter.move_xmm0_to_rax(to_f64);
    emitter.store64(EAX, destination);
    Ok(())
}

/// A reinterpretation copies the bits: the same value, in the other type's slot.
fn reinterpret_into(
    operation: Reinterpret,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let source = frame.slot(value);
    let destination = frame.slot(result);
    match operation {
        Reinterpret::I32FromF32 | Reinterpret::F32FromI32 => emitter.load32(EAX, source),
        Reinterpret::I64FromF64 | Reinterpret::F64FromI64 => emitter.load64(EAX, source),
    }
    emitter.store64(EAX, destination);
}

/// A direct call to another function of the module, through the call table in the context.
///
/// The arguments are copied to a block of slots in this frame, and the block's address is the
/// callee's argument pointer. The call counts a frame against the depth limit first, and a call
/// past the limit traps. A trap in the callee leaves its status in the context and returns zero,
/// so this function checks the status after the call and returns zero too, before it stores
/// anything. The return value is the callee's result register.
fn emit_call(
    function: u32,
    arguments: &[ValueId],
    results: &[ValueId],
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    if !frame.is_callable(function) {
        return Err(JitError::Unsupported(
            "a call to a function that is not compiled".into(),
        ));
    }
    let result = call_result(results)?;
    let block = frame.block(arguments.len().max(1));
    store_arguments(arguments, block, frame, emitter);
    let destination = match result {
        Some(result) => frame.slot(result),
        None => frame.temporary(),
    };
    enter_callee(frame, emitter)?;
    set_call_registers(block, frame, emitter);
    // mov rax, [rax + functions]; call [rax + 8 * function]
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x8B, 0x40, FUNCTIONS_OFFSET]);
    emitter.bytes.extend_from_slice(&[0xFF, 0x90]);
    emitter
        .bytes
        .extend_from_slice(&(8 * function as i32).to_le_bytes());
    leave_callee(frame, destination, emitter);
    Ok(())
}

/// An indirect call through the module's table: the index is checked against the table's length,
/// the slot's type against the call's type, and then the slot's entry is called. The checks come
/// in the order Micro makes them, so the same trap is reported for the same input.
///
/// The table's contents are fixed for the life of the module, and every entry is a function
/// compiled in this module (see [`Frame::env`]), so the entry the slot names can be called.
fn emit_call_indirect(
    type_index: u32,
    operand: ValueId,
    arguments: &[ValueId],
    results: &[ValueId],
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    if !frame.env.indirect {
        return Err(JitError::Unsupported(
            "an indirect call the native tier cannot resolve".into(),
        ));
    }
    let expected = frame
        .env
        .type_ids
        .get(type_index as usize)
        .copied()
        .and_then(|id| i32::try_from(id).ok())
        .ok_or_else(|| JitError::Unsupported("an indirect call to an unknown type".into()))?;
    let result = call_result(results)?;
    let block = frame.block(arguments.len().max(1));
    store_arguments(arguments, block, frame, emitter);
    let operand_slot = frame.slot(operand);
    let destination = match result {
        Some(result) => frame.slot(result),
        None => frame.temporary(),
    };
    let entry = frame.temporary();
    let context = frame.context;

    // rcx = the index, zero-extended from its i32 slot; rax = the context.
    emitter.memory(&[0x8B], ECX, operand_slot);
    emitter.memory_wide(&[0x8B], EAX, context);
    // Out of range traps: cmp rcx, [rax + table_len]; jb ok.
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x3B, 0x48, TABLE_LEN_OFFSET]);
    guard_trap(frame, 0x72, STATUS_TABLE_OUT_OF_BOUNDS, emitter)?;
    // rdx = the slot's type: `-1` for a null slot, else the canonical id of its signature.
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x8B, 0x50, TABLE_TYPES_OFFSET]); // mov rdx, [rax + types]
    emitter.bytes.extend_from_slice(&[0x48, 0x8B, 0x14, 0xCA]); // mov rdx, [rdx + rcx * 8]
    emitter.bytes.extend_from_slice(&[0x48, 0x83, 0xFA, 0xFF]); // cmp rdx, -1
    guard_trap(frame, 0x75, STATUS_NULL_REFERENCE, emitter)?; // jne ok
    emitter.bytes.extend_from_slice(&[0x48, 0x81, 0xFA]); // cmp rdx, imm32
    emitter.bytes.extend_from_slice(&expected.to_le_bytes());
    guard_trap(frame, 0x74, STATUS_INDIRECT_TYPE_MISMATCH, emitter)?; // je ok
                                                                      // rdx = the slot's entry, kept in a slot because the call's registers are set after the depth
                                                                      // check, and the context register may be rdx itself.
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x8B, 0x50, TABLE_ENTRIES_OFFSET]); // mov rdx, [rax + entries]
    emitter.bytes.extend_from_slice(&[0x48, 0x8B, 0x14, 0xCA]); // mov rdx, [rdx + rcx * 8]
    emitter.memory_wide(&[0x89], EDX, entry); // mov [entry], rdx

    enter_callee(frame, emitter)?;
    set_call_registers(block, frame, emitter);
    emitter.memory(&[0xFF], 2, entry); // call qword [entry]
    leave_callee(frame, destination, emitter);
    Ok(())
}

/// Copy each argument into the block that the callee reads as its argument array.
fn store_arguments(arguments: &[ValueId], block: i32, frame: &mut Frame, emitter: &mut Emitter) {
    for (index, argument) in arguments.iter().enumerate() {
        let source = frame.slot(*argument);
        emitter.load64(EAX, source);
        emitter.store64(EAX, block + 8 * index as i32);
    }
}

/// Count one more active frame against the depth limit, and trap if that is past it. Leaves the
/// context in `rax`.
fn enter_callee(frame: &Frame, emitter: &mut Emitter) -> Result<(), JitError> {
    // mov rax, [ctx]; mov rcx, [rax + depth]; cmp rcx, [rax + depth_limit]; jb ok; <trap>; ok:
    emitter.memory_wide(&[0x8B], EAX, frame.context);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x8B, 0x48, DEPTH_OFFSET]);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x3B, 0x48, DEPTH_LIMIT_OFFSET]);
    guard_trap(frame, 0x72, STATUS_CALL_DEPTH, emitter)?;
    // add rcx, 1; mov [rax + depth], rcx
    emitter.bytes.extend_from_slice(&[0x48, 0x83, 0xC1, 0x01]);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x89, 0x48, DEPTH_OFFSET]);
    Ok(())
}

/// The callee's registers: `lea <args>, [rbp + block]` and the context into its register, from
/// `rax`, which still holds the context.
fn set_call_registers(block: i32, frame: &Frame, emitter: &mut Emitter) {
    let args_register = frame.abi.args_register();
    let context_register = frame.abi.context_register();
    emitter.memory_wide(&[0x8D], args_register, block);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x89, 0xC0 | context_register]);
}

/// After the callee returns: count the frame off, and if the callee left a status behind, return
/// zero without storing anything. Otherwise store the callee's result, which is in `rax`.
fn leave_callee(frame: &mut Frame, destination: i32, emitter: &mut Emitter) {
    // mov rcx, [ctx]; sub qword [rcx + depth], 1; cmp qword [rcx + status], 0; je ok;
    // xor eax, eax; leave; ret; ok:
    emitter.memory_wide(&[0x8B], ECX, frame.context);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x83, 0x69, DEPTH_OFFSET, 0x01]);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x83, 0x79, STATUS_OFFSET, 0x00]);
    emitter.bytes.extend_from_slice(&[0x74, 0x04]); // je past the four bytes below
    emitter.bytes.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax
    emitter.epilogue();
    emitter.store64(EAX, destination);
    frame.shadow = 32;
}

/// A short conditional jump, `jump`, that skips a trap with `status` when it is taken. The trap is
/// emitted in place, after the jump.
fn guard_trap(frame: &Frame, jump: u8, status: u32, emitter: &mut Emitter) -> Result<(), JitError> {
    let at = emitter.bytes.len();
    emitter.bytes.extend_from_slice(&[jump, 0]);
    emitter.trap(frame, status);
    let distance = emitter.bytes.len() - (at + 2);
    emitter.bytes[at + 1] = u8::try_from(distance)
        .map_err(|_| JitError::Unsupported("a trap sequence too long for a short jump".into()))?;
    Ok(())
}

/// Check that global `index` is inside the globals the caller provided, then leave the array's base in
/// `rax`. Returns the byte offset of the global within it, for `[rax + offset]`. An index past the end
/// traps, so code called without its globals cannot read past them.
fn globals_base_into_rax(
    index: u32,
    frame: &Frame,
    emitter: &mut Emitter,
) -> Result<i32, JitError> {
    if !frame.env.globals {
        return Err(JitError::Unsupported(
            "a global access without a globals array".into(),
        ));
    }
    let index = i32::try_from(index)
        .map_err(|_| JitError::Unsupported("a global index past the encodable range".into()))?;
    let offset = index
        .checked_mul(8)
        .ok_or_else(|| JitError::Unsupported("a global index past the encodable range".into()))?;
    emitter.memory_wide(&[0x8B], EDX, frame.context); // mov rdx, [context]
                                                      // cmp qword [rdx + globals_len], index; ja inside; trap
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x81, 0x7A, GLOBALS_LENGTH_OFFSET]);
    emitter.bytes.extend_from_slice(&index.to_le_bytes());
    let inside = emitter.new_label();
    emitter.jump(&[0x0F, 0x87], Target::Label(inside));
    emitter.trap(frame, STATUS_GLOBAL_OUT_OF_BOUNDS);
    emitter.bind(inside);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x8B, 0x42, GLOBALS_OFFSET]); // mov rax, [rdx + globals]
    Ok(offset)
}

/// `result = clz(value)`, `ctz(value)` or `popcnt(value)` on 32 or 64 bits.
///
/// `bsr` and `bsf` leave their destination undefined for a zero input, so a zero input is given the
/// width's count by a conditional move on the zero flag. `popcnt` is computed in software, since the
/// instruction needs a CPU feature the target identity does not record.
fn int_unary(
    wide: bool,
    operation: IntUnary,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let source = frame.slot(value);
    let destination = frame.slot(result);
    let bits: i32 = if wide { 64 } else { 32 };
    if wide {
        emitter.load64(ECX, source);
    } else {
        emitter.load32(ECX, source);
    }
    match operation {
        IntUnary::Clz => {
            // rdx = -1; rax = bsr(rcx), or -1 for zero; rcx = (bits - 1) - rax; rax = rcx.
            emitter
                .bytes
                .extend_from_slice(&[0x48, 0xC7, 0xC2, 0xFF, 0xFF, 0xFF, 0xFF]); // mov rdx, -1
            emitter.bytes.extend_from_slice(&[0x48, 0x0F, 0xBD, 0xC1]); // bsr rax, rcx
            emitter.bytes.extend_from_slice(&[0x48, 0x0F, 0x44, 0xC2]); // cmovz rax, rdx
            emitter.bytes.extend_from_slice(&[0x48, 0xC7, 0xC1]); // mov rcx, bits - 1
            emitter.bytes.extend_from_slice(&(bits - 1).to_le_bytes());
            emitter.bytes.extend_from_slice(&[0x48, 0x29, 0xC1]); // sub rcx, rax
            emitter.bytes.extend_from_slice(&[0x48, 0x89, 0xC8]); // mov rax, rcx
        }
        IntUnary::Ctz => {
            emitter.bytes.extend_from_slice(&[0x48, 0xC7, 0xC2]); // mov rdx, bits
            emitter.bytes.extend_from_slice(&bits.to_le_bytes());
            emitter.bytes.extend_from_slice(&[0x48, 0x0F, 0xBC, 0xC1]); // bsf rax, rcx
            emitter.bytes.extend_from_slice(&[0x48, 0x0F, 0x44, 0xC2]); // cmovz rax, rdx
        }
        IntUnary::Popcnt => {
            emitter.bytes.extend_from_slice(&[0x48, 0x89, 0xC8]); // mov rax, rcx
            popcount_rax(emitter);
        }
    }
    emitter.store64(EAX, destination);
}

/// The number of set bits in `rax`, the standard divide-and-add sequence on 64 bits. A 32-bit input
/// is zero-extended, so its count is the same. Clobbers `rcx` and `rdx`.
fn popcount_rax(emitter: &mut Emitter) {
    // x -= (x >> 1) & 0x5555...
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x89, 0xC1, 0x48, 0xD1, 0xE9]); // mov rcx, rax; shr rcx, 1
    emitter.movabs(EDX, 0x5555_5555_5555_5555);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x21, 0xD1, 0x48, 0x29, 0xC8]); // and rcx, rdx; sub rax, rcx
                                                                   // x = (x & 0x3333...) + ((x >> 2) & 0x3333...)
    emitter.movabs(EDX, 0x3333_3333_3333_3333);
    emitter.bytes.extend_from_slice(&[
        0x48, 0x89, 0xC1, // mov rcx, rax
        0x48, 0x21, 0xD1, // and rcx, rdx
        0x48, 0xC1, 0xE8, 0x02, // shr rax, 2
        0x48, 0x21, 0xD0, // and rax, rdx
        0x48, 0x01, 0xC8, // add rax, rcx
    ]);
    // x = (x + (x >> 4)) & 0x0F0F...
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x89, 0xC1, 0x48, 0xC1, 0xE9, 0x04, 0x48, 0x01, 0xC8]);
    emitter.movabs(EDX, 0x0F0F_0F0F_0F0F_0F0F);
    emitter.bytes.extend_from_slice(&[0x48, 0x21, 0xD0]); // and rax, rdx
                                                          // x * 0x0101... >> 56: the byte sums collect in the top byte.
    emitter.movabs(EDX, 0x0101_0101_0101_0101);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x0F, 0xAF, 0xC2, 0x48, 0xC1, 0xE8, 0x38]); // imul rax, rdx; shr rax, 56
}

/// `result = left rotl count` or `left rotr count` on 32 or 64 bits. `rol` and `ror` take the count
/// modulo the width, as WebAssembly does.
fn int_rotate(
    wide: bool,
    rotate_left: bool,
    result: ValueId,
    value: ValueId,
    count: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let source = frame.slot(value);
    let count = frame.slot(count);
    let destination = frame.slot(result);
    if wide {
        emitter.load64(EAX, source);
    } else {
        emitter.load32(EAX, source);
    }
    emitter.load32(ECX, count);
    // d3 /0 is rol and d3 /1 is ror, both by cl.
    if wide {
        emitter.bytes.push(0x48);
    }
    emitter
        .bytes
        .extend_from_slice(&[0xD3, if rotate_left { 0xC0 } else { 0xC8 }]);
    emitter.store64(EAX, destination);
}

/// `result = memory.grow(delta)` in place. The new size is the old size plus `delta` pages; it fails,
/// with -1, when it is past the reservation, which is the memory's maximum. A success stores the new
/// size in the context, where the caller reads it back, and returns the old size in pages.
fn memory_grow(result: ValueId, delta: ValueId, frame: &mut Frame, emitter: &mut Emitter) {
    let delta_slot = frame.slot(delta);
    let destination = frame.slot(result);
    emitter.memory_wide(&[0x8B], EDX, frame.context); // mov rdx, [context]
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x8B, 0x42, MEMORY_LENGTH_OFFSET]); // mov rax, [rdx + length]
    emitter.load32(ECX, delta_slot); // rcx = the pages to add, zero-extended
    emitter.bytes.extend_from_slice(&[0x48, 0xC1, 0xE1, 0x10]); // shl rcx, 16
    emitter.bytes.extend_from_slice(&[0x48, 0x01, 0xC1]); // add rcx, rax: the new size
                                                          // cmp rcx, [rdx + capacity]; ja fail: the new size is past the reservation.
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x3B, 0x4A, MEMORY_CAPACITY_OFFSET]);
    let fail = emitter.new_label();
    emitter.jump(&[0x0F, 0x87], Target::Label(fail));
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x89, 0x4A, MEMORY_LENGTH_OFFSET]); // mov [rdx + length], rcx
    emitter.bytes.extend_from_slice(&[0x48, 0xC1, 0xE8, 0x10]); // shr rax, 16: the old size in pages
    let done = emitter.new_label();
    emitter.jump(&[0xE9], Target::Label(done));
    emitter.bind(fail);
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF]); // mov rax, -1
    emitter.bind(done);
    emitter.store64(EAX, destination);
}

/// The registers that hold values in a function that makes no call: r9, r10 and r11. They are
/// caller-saved on both conventions, and the encoder never uses them, so a leaf function can keep values
/// in them between instructions.
const VALUE_REGISTERS: [u8; 9] = [8, 9, 10, 11, 3, 12, 13, 14, 15];

/// The callee-saved registers among the value registers: rbx and r12 to r15. A leaf function that uses one
/// saves it on entry and restores it on every return, so the caller sees it unchanged.
const CALLEE_SAVED: [u8; 5] = [3, 12, 13, 14, 15];

/// The register a value lives in, if it has one.
impl Frame {
    fn register(&self, value: ValueId) -> Option<u8> {
        self.registers.get(&value).copied()
    }
}

/// An instruction in a REX-prefixed register form: `prefix? opcode modrm(reg, rm)`. The REX prefix is
/// needed for a 64-bit operation, or when either register is one of r8 to r15.
fn emit_register_form(emitter: &mut Emitter, wide: bool, opcode: &[u8], reg: u8, rm: u8) {
    let rex = 0x40
        | if wide { 0x08 } else { 0 }
        | if reg >= 8 { 0x04 } else { 0 }
        | if rm >= 8 { 0x01 } else { 0 };
    if rex != 0x40 {
        emitter.bytes.push(rex);
    }
    emitter.bytes.extend_from_slice(opcode);
    emitter.bytes.push(0xC0 | ((reg & 7) << 3) | (rm & 7));
}

/// Load a value into `into`, from its register or its slot. `wide` loads 64 bits, else 32.
fn load_value(frame: &mut Frame, value: ValueId, wide: bool, into: u8, emitter: &mut Emitter) {
    match frame.register(value) {
        Some(register) => emit_register_form(emitter, wide, &[0x89], register, into), // mov into, register
        None => {
            let slot = frame.slot(value);
            if wide {
                emitter.load64(into, slot);
            } else {
                emitter.load32(into, slot);
            }
        }
    }
}

/// Store `from` into a value's register or slot. `wide` stores 64 bits, else 32.
fn store_value(frame: &mut Frame, value: ValueId, wide: bool, from: u8, emitter: &mut Emitter) {
    match frame.register(value) {
        Some(register) => emit_register_form(emitter, wide, &[0x89], from, register), // mov register, from
        None => {
            let slot = frame.slot(value);
            if wide {
                emitter.store64(from, slot);
            } else {
                emitter.store32(from, slot);
            }
        }
    }
}

/// `result = left op right` with `opcode` in its `r, r/m` form, on 32 or 64 bits.
fn int_binary(
    wide: bool,
    opcode: &[u8],
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    load_value(frame, left, wide, EAX, emitter);
    match frame.register(right) {
        Some(register) => emit_register_form(emitter, wide, opcode, EAX, register),
        None => {
            let slot = frame.slot(right);
            if wide {
                emitter.memory_wide(opcode, EAX, slot);
            } else {
                emitter.memory(opcode, EAX, slot);
            }
        }
    }
    store_value(frame, result, wide, EAX, emitter);
}

/// Copy a value to another through the register or slot each has. Two registers take one move, a register and
/// a slot take one memory move, and two slots go through `eax`.
fn copy_value(from: ValueId, to: ValueId, frame: &mut Frame, emitter: &mut Emitter) {
    match (frame.register(from), frame.register(to)) {
        (Some(source), Some(destination)) => {
            if source != destination {
                emit_register_form(emitter, true, &[0x89], source, destination);
            }
        }
        (Some(source), None) => {
            let slot = frame.slot(to);
            emitter.memory_with(true, &[0x89], source, slot);
        }
        (None, Some(destination)) => {
            let slot = frame.slot(from);
            emitter.memory_with(true, &[0x8B], destination, slot);
        }
        (None, None) => {
            let source = frame.slot(from);
            let destination = frame.slot(to);
            emitter.load64(EAX, source);
            emitter.store64(EAX, destination);
        }
    }
}

/// Whether an instruction is handled by the register path on x86-64: the shared register instructions, and the
/// integer comparisons, which read their operands from registers or slots and write their result the same way.
fn is_x86_register_instruction(instruction: &IrInstr) -> bool {
    is_register_instruction(instruction)
        || matches!(
            instruction,
            IrInstr::I32Compare { .. } | IrInstr::I64Compare { .. }
        )
}

/// `result = left <comparison> right` as 0 or 1, on 32 or 64 bits. The left operand goes to eax, the right is
/// compared from its register or its slot, and the flags become the result.
fn int_compare(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    comparison: IntComparison,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    load_value(frame, left, wide, EAX, emitter);
    match frame.register(right) {
        Some(register) => emit_register_form(emitter, wide, OP_CMP, EAX, register),
        None => {
            let slot = frame.slot(right);
            if wide {
                emitter.memory_wide(OP_CMP, EAX, slot);
            } else {
                emitter.memory(OP_CMP, EAX, slot);
            }
        }
    }
    emitter.set_from_flags(condition_code(comparison));
    store_value(frame, result, false, EAX, emitter);
}

/// Emit an instruction of [`is_register_instruction`].
fn emit_register_instruction(
    instruction: &IrInstr,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    match instruction {
        IrInstr::ConstI32 { result, value } => match frame.register(*result) {
            Some(register) => {
                if register >= 8 {
                    emitter.bytes.push(0x41);
                }
                emitter.bytes.push(0xB8 | (register & 7)); // mov r32, imm32
                emitter.bytes.extend_from_slice(&value.to_le_bytes());
            }
            None => {
                let slot = frame.slot(*result);
                emitter.store_immediate32(slot, *value);
            }
        },
        IrInstr::ConstI64 { result, value } => match frame.register(*result) {
            Some(register) => {
                emitter
                    .bytes
                    .push(0x48 | if register >= 8 { 0x01 } else { 0 });
                emitter.bytes.push(0xB8 | (register & 7)); // movabs r64, imm64
                emitter.bytes.extend_from_slice(&value.to_le_bytes());
            }
            None => {
                let slot = frame.slot(*result);
                emitter.movabs(EAX, *value);
                emitter.store64(EAX, slot);
            }
        },
        IrInstr::LocalGet { result, local } => copy_value(*local, *result, frame, emitter),
        IrInstr::LocalSet { local, value } => copy_value(*value, *local, frame, emitter),
        IrInstr::I32Add {
            result,
            left,
            right,
        } => int_binary(false, OP_ADD, *result, *left, *right, frame, emitter),
        IrInstr::I32Sub {
            result,
            left,
            right,
        } => int_binary(false, OP_SUB, *result, *left, *right, frame, emitter),
        IrInstr::I32Mul {
            result,
            left,
            right,
        } => int_binary(false, OP_IMUL, *result, *left, *right, frame, emitter),
        IrInstr::I32And {
            result,
            left,
            right,
        } => int_binary(false, OP_AND, *result, *left, *right, frame, emitter),
        IrInstr::I32Or {
            result,
            left,
            right,
        } => int_binary(false, OP_OR, *result, *left, *right, frame, emitter),
        IrInstr::I32Xor {
            result,
            left,
            right,
        } => int_binary(false, OP_XOR, *result, *left, *right, frame, emitter),
        IrInstr::I64Add {
            result,
            left,
            right,
        } => int_binary(true, OP_ADD, *result, *left, *right, frame, emitter),
        IrInstr::I64Sub {
            result,
            left,
            right,
        } => int_binary(true, OP_SUB, *result, *left, *right, frame, emitter),
        IrInstr::I64Mul {
            result,
            left,
            right,
        } => int_binary(true, OP_IMUL, *result, *left, *right, frame, emitter),
        IrInstr::I64And {
            result,
            left,
            right,
        } => int_binary(true, OP_AND, *result, *left, *right, frame, emitter),
        IrInstr::I64Or {
            result,
            left,
            right,
        } => int_binary(true, OP_OR, *result, *left, *right, frame, emitter),
        IrInstr::I64Xor {
            result,
            left,
            right,
        } => int_binary(true, OP_XOR, *result, *left, *right, frame, emitter),
        IrInstr::I32Compare {
            result,
            left,
            right,
            comparison,
        } => int_compare(false, *result, *left, *right, *comparison, frame, emitter),
        IrInstr::I64Compare {
            result,
            left,
            right,
            comparison,
        } => int_compare(true, *result, *left, *right, *comparison, frame, emitter),
        other => {
            return Err(JitError::Unsupported(format!(
                "the {} instruction in a register",
                instruction_name(other)
            )))
        }
    }
    Ok(())
}

fn emit_instruction(
    instruction: &IrInstr,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    if is_x86_register_instruction(instruction) {
        return emit_register_instruction(instruction, frame, emitter);
    }
    match instruction {
        IrInstr::F32Min {
            result,
            left,
            right,
        } => float_min_max(false, false, *result, *left, *right, frame, emitter),
        IrInstr::F32Max {
            result,
            left,
            right,
        } => float_min_max(false, true, *result, *left, *right, frame, emitter),
        IrInstr::F64Min {
            result,
            left,
            right,
        } => float_min_max(true, false, *result, *left, *right, frame, emitter),
        IrInstr::F64Max {
            result,
            left,
            right,
        } => float_min_max(true, true, *result, *left, *right, frame, emitter),
        IrInstr::F32Copysign {
            result,
            left,
            right,
        } => float_copysign(false, *result, *left, *right, frame, emitter),
        IrInstr::F64Copysign {
            result,
            left,
            right,
        } => float_copysign(true, *result, *left, *right, frame, emitter),
        IrInstr::FloatConvert {
            result,
            value,
            operation,
        } => float_convert_into(*operation, *result, *value, frame, emitter)?,
        IrInstr::Reinterpret {
            result,
            value,
            operation,
        } => reinterpret_into(*operation, *result, *value, frame, emitter),
        IrInstr::FloatTrunc {
            result,
            value,
            operation,
        } => float_trunc_into(*operation, *result, *value, frame, emitter)?,
        IrInstr::MemorySize { result } => {
            // The memory cannot grow during a native call, so its length, in bytes, is fixed for
            // the call. The size in pages is that length shifted right by 16 bits.
            let destination = frame.slot(*result);
            emitter.memory_wide(&[0x8B], EAX, frame.context); // mov rax, [ctx]
            emitter
                .bytes
                .extend_from_slice(&[0x48, 0x8B, 0x40, MEMORY_LENGTH_OFFSET]); // mov rax, [rax + len]
            emitter.bytes.extend_from_slice(&[0x48, 0xC1, 0xE8, 0x10]); // shr rax, 16
            emitter.store64(EAX, destination);
        }
        IrInstr::GlobalGet { result, global } => {
            let destination = frame.slot(*result);
            let offset = globals_base_into_rax(*global, frame, emitter)?;
            emitter.bytes.extend_from_slice(&[0x48, 0x8B, 0x80]); // mov rax, [rax + offset]
            emitter.bytes.extend_from_slice(&offset.to_le_bytes());
            emitter.store64(EAX, destination);
        }
        IrInstr::GlobalSet { global, value } => {
            let source = frame.slot(*value);
            let offset = globals_base_into_rax(*global, frame, emitter)?;
            emitter.memory_wide(&[0x8B], ECX, source); // mov rcx, [value]
            emitter.bytes.extend_from_slice(&[0x48, 0x89, 0x88]); // mov [rax + offset], rcx
            emitter.bytes.extend_from_slice(&offset.to_le_bytes());
        }
        IrInstr::I32Unary {
            result,
            value,
            operation,
        } => int_unary(false, *operation, *result, *value, frame, emitter),
        IrInstr::I64Unary {
            result,
            value,
            operation,
        } => int_unary(true, *operation, *result, *value, frame, emitter),
        IrInstr::I32Rotl {
            result,
            left,
            right,
        } => int_rotate(false, true, *result, *left, *right, frame, emitter),
        IrInstr::I32Rotr {
            result,
            left,
            right,
        } => int_rotate(false, false, *result, *left, *right, frame, emitter),
        IrInstr::I64Rotl {
            result,
            left,
            right,
        } => int_rotate(true, true, *result, *left, *right, frame, emitter),
        IrInstr::I64Rotr {
            result,
            left,
            right,
        } => int_rotate(true, false, *result, *left, *right, frame, emitter),
        IrInstr::MemoryGrow { result, delta } => {
            if !frame.env.memory_grow {
                return Err(JitError::Unsupported(
                    "a memory.grow of a memory that is not reserved".into(),
                ));
            }
            memory_grow(*result, *delta, frame, emitter)
        }
        IrInstr::Drop { .. } => {}
        IrInstr::Call {
            function,
            arguments,
            results,
        } => emit_call(*function, arguments, results, frame, emitter)?,
        IrInstr::CallIndirect {
            type_index,
            table,
            operand,
            arguments,
            results,
        } => {
            // MVP has one table, the one at index 0.
            if *table != 0 {
                return Err(JitError::Unsupported("a call through another table".into()));
            }
            emit_call_indirect(*type_index, *operand, arguments, results, frame, emitter)?
        }
        IrInstr::ConstF32 { result, value } => {
            let slot = frame.slot(*result);
            emitter.movabs(EAX, i64::from(*value));
            emitter.store64(EAX, slot);
        }
        IrInstr::ConstF64 { result, value } => {
            let slot = frame.slot(*result);
            emitter.movabs(EAX, *value as i64);
            emitter.store64(EAX, slot);
        }
        IrInstr::F32Add {
            result,
            left,
            right,
        } => float_binary(0x58, false, *result, *left, *right, frame, emitter),
        IrInstr::F32Sub {
            result,
            left,
            right,
        } => float_binary(0x5C, false, *result, *left, *right, frame, emitter),
        IrInstr::F32Mul {
            result,
            left,
            right,
        } => float_binary(0x59, false, *result, *left, *right, frame, emitter),
        IrInstr::F32Div {
            result,
            left,
            right,
        } => float_binary(0x5E, false, *result, *left, *right, frame, emitter),
        IrInstr::F64Add {
            result,
            left,
            right,
        } => float_binary(0x58, true, *result, *left, *right, frame, emitter),
        IrInstr::F64Sub {
            result,
            left,
            right,
        } => float_binary(0x5C, true, *result, *left, *right, frame, emitter),
        IrInstr::F64Mul {
            result,
            left,
            right,
        } => float_binary(0x59, true, *result, *left, *right, frame, emitter),
        IrInstr::F64Div {
            result,
            left,
            right,
        } => float_binary(0x5E, true, *result, *left, *right, frame, emitter),
        IrInstr::F32Compare {
            result,
            left,
            right,
            comparison,
        } => float_compare(false, *result, *left, *right, *comparison, frame, emitter),
        IrInstr::F64Compare {
            result,
            left,
            right,
            comparison,
        } => float_compare(true, *result, *left, *right, *comparison, frame, emitter),
        IrInstr::F32Unary {
            result,
            value,
            operation,
        } => float_unary(false, *result, *value, *operation, frame, emitter)?,
        IrInstr::F64Unary {
            result,
            value,
            operation,
        } => float_unary(true, *result, *value, *operation, frame, emitter)?,
        IrInstr::LocalTee {
            result,
            local,
            value,
        } => {
            copy_value(*value, *local, frame, emitter);
            copy_value(*value, *result, frame, emitter);
        }
        IrInstr::I32Shl {
            result,
            left,
            right,
        } => shift(false, 4, *result, *left, *right, frame, emitter),
        IrInstr::I32ShrU {
            result,
            left,
            right,
        } => shift(false, 5, *result, *left, *right, frame, emitter),
        IrInstr::I32ShrS {
            result,
            left,
            right,
        } => shift(false, 7, *result, *left, *right, frame, emitter),
        IrInstr::I64Shl {
            result,
            left,
            right,
        } => shift(true, 4, *result, *left, *right, frame, emitter),
        IrInstr::I64ShrU {
            result,
            left,
            right,
        } => shift(true, 5, *result, *left, *right, frame, emitter),
        IrInstr::I64ShrS {
            result,
            left,
            right,
        } => shift(true, 7, *result, *left, *right, frame, emitter),
        IrInstr::I32DivU {
            result,
            left,
            right,
        } => divide(false, false, false, *result, *left, *right, frame, emitter),
        IrInstr::I32RemU {
            result,
            left,
            right,
        } => divide(false, true, false, *result, *left, *right, frame, emitter),
        IrInstr::I32DivS {
            result,
            left,
            right,
        } => divide(true, false, false, *result, *left, *right, frame, emitter),
        IrInstr::I32RemS {
            result,
            left,
            right,
        } => divide(true, true, false, *result, *left, *right, frame, emitter),
        IrInstr::I64DivU {
            result,
            left,
            right,
        } => divide(false, false, true, *result, *left, *right, frame, emitter),
        IrInstr::I64RemU {
            result,
            left,
            right,
        } => divide(false, true, true, *result, *left, *right, frame, emitter),
        IrInstr::I64DivS {
            result,
            left,
            right,
        } => divide(true, false, true, *result, *left, *right, frame, emitter),
        IrInstr::I64RemS {
            result,
            left,
            right,
        } => divide(true, true, true, *result, *left, *right, frame, emitter),
        IrInstr::I32Eqz { result, value } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            emitter.compare_with_zero32(source);
            emitter.set_from_flags(0x4); // sete
            emitter.store32(EAX, destination);
        }
        IrInstr::I64Eqz { result, value } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            emitter.compare_with_zero64(source);
            emitter.set_from_flags(0x4);
            emitter.store32(EAX, destination);
        }
        IrInstr::Select {
            result,
            condition,
            left,
            right,
        } => {
            // The result is `left` when the condition is non-zero, else `right`. `mov` leaves the
            // flags alone, so the comparison still holds for the `cmovne`.
            let wide = matches!(
                frame.value_type(*result),
                Some(ValueType::I64 | ValueType::F64)
            );
            let condition = frame.slot(*condition);
            let left = frame.slot(*left);
            let right = frame.slot(*right);
            let destination = frame.slot(*result);
            emitter.compare_with_zero32(condition);
            if wide {
                emitter.load64(EAX, right);
                emitter.load64(ECX, left);
                emitter.bytes.extend_from_slice(&[0x48, 0x0F, 0x45, 0xC1]); // cmovne rax, rcx
                emitter.store64(EAX, destination);
            } else {
                emitter.load32(EAX, right);
                emitter.load32(ECX, left);
                emitter.bytes.extend_from_slice(&[0x0F, 0x45, 0xC1]); // cmovne eax, ecx
                emitter.store32(EAX, destination);
            }
        }
        IrInstr::IntConvert {
            result,
            value,
            operation,
        } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            match operation {
                // Keep the low 32 bits.
                IntConversion::I32WrapI64 => {
                    emitter.load64(EAX, source);
                    emitter.store32(EAX, destination);
                }
                // movsxd rax, dword [source]
                IntConversion::I64ExtendI32S => {
                    emitter.memory_wide(&[0x63], EAX, source);
                    emitter.store64(EAX, destination);
                }
                // mov eax, [source] zero-extends.
                IntConversion::I64ExtendI32U => {
                    emitter.load32(EAX, source);
                    emitter.store64(EAX, destination);
                }
            }
        }
        IrInstr::SignExtend {
            result,
            value,
            operation,
        } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            match operation {
                SignExtend::I32Extend8S => {
                    emitter.load32(EAX, source);
                    emitter.bytes.extend_from_slice(&[0x0F, 0xBE, 0xC0]); // movsx eax, al
                    emitter.store32(EAX, destination);
                }
                SignExtend::I32Extend16S => {
                    emitter.load32(EAX, source);
                    emitter.bytes.extend_from_slice(&[0x0F, 0xBF, 0xC0]); // movsx eax, ax
                    emitter.store32(EAX, destination);
                }
                SignExtend::I64Extend8S => {
                    emitter.load64(EAX, source);
                    emitter.bytes.extend_from_slice(&[0x48, 0x0F, 0xBE, 0xC0]); // movsx rax, al
                    emitter.store64(EAX, destination);
                }
                SignExtend::I64Extend16S => {
                    emitter.load64(EAX, source);
                    emitter.bytes.extend_from_slice(&[0x48, 0x0F, 0xBF, 0xC0]); // movsx rax, ax
                    emitter.store64(EAX, destination);
                }
                SignExtend::I64Extend32S => {
                    emitter.load64(EAX, source);
                    emitter.bytes.extend_from_slice(&[0x48, 0x63, 0xC0]); // movsxd rax, eax
                    emitter.store64(EAX, destination);
                }
            }
        }
        IrInstr::Load {
            result,
            address,
            offset,
            operation,
            bounds: _,
        } => {
            // The bounds hint is ignored: every access is checked here. Each variant names the
            // access width and the extension to the 64-bit slot.
            let (width, access): (u32, &[u8]) = match operation {
                // An f32 or f64 load moves the same bits as the integer load of that width.
                MemoryLoad::I32 | MemoryLoad::F32 => (4, &[0x8B, 0x04, 0x02]),
                MemoryLoad::I32_8U => (1, &[0x0F, 0xB6, 0x04, 0x02]),
                MemoryLoad::I32_8S => (1, &[0x0F, 0xBE, 0x04, 0x02]),
                MemoryLoad::I32_16U => (2, &[0x0F, 0xB7, 0x04, 0x02]),
                MemoryLoad::I32_16S => (2, &[0x0F, 0xBF, 0x04, 0x02]),
                MemoryLoad::I64 | MemoryLoad::F64 => (8, &[0x48, 0x8B, 0x04, 0x02]),
                MemoryLoad::I64_8U => (1, &[0x0F, 0xB6, 0x04, 0x02]),
                MemoryLoad::I64_8S => (1, &[0x48, 0x0F, 0xBE, 0x04, 0x02]),
                MemoryLoad::I64_16U => (2, &[0x0F, 0xB7, 0x04, 0x02]),
                MemoryLoad::I64_16S => (2, &[0x48, 0x0F, 0xBF, 0x04, 0x02]),
                MemoryLoad::I64_32U => (4, &[0x8B, 0x04, 0x02]),
                MemoryLoad::I64_32S => (4, &[0x48, 0x63, 0x04, 0x02]),
            };
            emit_address(*address, *offset, width, frame, emitter);
            emitter.bytes.extend_from_slice(access);
            let destination = frame.slot(*result);
            emitter.store64(EAX, destination);
        }
        IrInstr::Store {
            address,
            value,
            offset,
            operation,
            bounds: _,
        } => {
            // The value is loaded into rcx and the store takes its low bits, so r8 stays free for values. An eight-byte store
            // uses the wide forms; the narrower ones store the low 8, 16, or 32 bits.
            let (width, access, wide): (u32, &[u8], bool) = match operation {
                MemoryStore::I32 | MemoryStore::F32 => (4, &[0x89, 0x0C, 0x02], false),
                MemoryStore::I32_8 | MemoryStore::I64_8 => (1, &[0x88, 0x0C, 0x02], false),
                MemoryStore::I32_16 | MemoryStore::I64_16 => (2, &[0x66, 0x89, 0x0C, 0x02], false),
                MemoryStore::I64 | MemoryStore::F64 => (8, &[0x48, 0x89, 0x0C, 0x02], true),
                MemoryStore::I64_32 => (4, &[0x89, 0x0C, 0x02], false),
            };
            emit_address(*address, *offset, width, frame, emitter);
            let source = frame.slot(*value);
            emitter.memory_with(wide, &[0x8B], ECX, source); // mov rcx(d), [value]
            emitter.bytes.extend_from_slice(access);
        }
        other => {
            return Err(JitError::Unsupported(format!(
                "the {} instruction",
                instruction_name(other)
            )))
        }
    }
    Ok(())
}

/// `result = left << right` (or a right shift, by `extension`): the count goes in `cl`.
fn shift(
    wide: bool,
    extension: u8,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    if wide {
        emitter.load64(ECX, right);
        emitter.load64(EAX, left);
        emitter.shift_by_cl_wide(extension);
        emitter.store64(EAX, destination);
    } else {
        emitter.load32(ECX, right);
        emitter.load32(EAX, left);
        emitter.shift_by_cl(extension);
        emitter.store32(EAX, destination);
    }
}

/// Division and remainder with Wasm's traps, at 32 or 64 bits.
///
/// A zero divisor traps. For signed division, the minimum value divided by -1 traps, while its
/// remainder is zero, so a divisor of -1 takes a path that does not divide.
#[allow(clippy::too_many_arguments)]
fn divide(
    signed: bool,
    remainder: bool,
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    if wide {
        emitter.load64(ECX, right);
        emitter.load64(EAX, left);
    } else {
        emitter.load32(ECX, right);
        emitter.load32(EAX, left);
    }

    // A zero divisor is a trap: `test rcx, rcx; jnz ok; trap; ok:`.
    let nonzero = emitter.new_label();
    emitter.bytes.extend_from_slice(&[0x48, 0x85, 0xC9]);
    emitter.jump(&[0x0F, 0x85], Target::Label(nonzero));
    emitter.trap(frame, crate::STATUS_DIVIDE_BY_ZERO);
    emitter.bind(nonzero);

    let normal = emitter.new_label();
    let done = emitter.new_label();
    if signed {
        // cmp rcx, -1 (or cmp ecx, -1 at 32 bits, whose upper half is zero): a divisor of -1 is
        // the case that can overflow.
        if wide {
            emitter.bytes.extend_from_slice(&[0x48, 0x83, 0xF9, 0xFF]);
        } else {
            emitter.bytes.extend_from_slice(&[0x83, 0xF9, 0xFF]);
        }
        emitter.jump(&[0x0F, 0x85], Target::Label(normal));
        if remainder {
            // The remainder by -1 is zero.
            emitter.bytes.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax
            emitter.jump(&[0xE9], Target::Label(done));
        } else {
            // The minimum value of the operand's width, divided by -1, overflows.
            if wide {
                emitter.movabs(EDX, i64::MIN);
                emitter.bytes.extend_from_slice(&[0x48, 0x39, 0xD0]); // cmp rax, rdx
            } else {
                emitter.bytes.extend_from_slice(&[0x3D]); // cmp eax, imm32
                emitter.bytes.extend_from_slice(&i32::MIN.to_le_bytes());
            }
            emitter.jump(&[0x0F, 0x85], Target::Label(normal));
            emitter.trap(frame, crate::STATUS_OVERFLOW);
        }
    }
    emitter.bind(normal);
    match (signed, wide) {
        (true, true) => emitter
            .bytes
            .extend_from_slice(&[0x48, 0x99, 0x48, 0xF7, 0xF9]), // cqo; idiv rcx
        (true, false) => emitter.bytes.extend_from_slice(&[0x99, 0xF7, 0xF9]), // cdq; idiv ecx
        (false, true) => emitter
            .bytes
            .extend_from_slice(&[0x31, 0xD2, 0x48, 0xF7, 0xF1]), // xor edx, edx; div rcx
        (false, false) => emitter.bytes.extend_from_slice(&[0x31, 0xD2, 0xF7, 0xF1]), // xor edx, edx; div ecx
    }
    if remainder {
        // The remainder is in edx/rdx.
        emitter.bytes.extend_from_slice(&[0x48, 0x89, 0xD0]); // mov rax, rdx
    }
    emitter.bind(done);
    if wide {
        emitter.store64(EAX, destination);
    } else {
        emitter.store32(EAX, destination);
    }
}

/// Leave the address of an access in `rax` and the memory base in `rdx`, after trapping if the
/// access does not lie wholly inside the memory.
///
/// The effective address is the 32-bit operand plus the 32-bit offset, computed in 64 bits, so it
/// cannot wrap. The check is `address + width <= memory_len`, read from the context.
fn emit_address(
    address: ValueId,
    offset: u32,
    width: u32,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let address = frame.slot(address);
    emitter.load32(EAX, address); // zero-extends into rax
    emitter.bytes.extend_from_slice(&[0x48, 0xB9]); // movabs rcx, offset
    emitter
        .bytes
        .extend_from_slice(&u64::from(offset).to_le_bytes());
    emitter.bytes.extend_from_slice(&[0x48, 0x01, 0xC8]); // add rax, rcx
    emitter.bytes.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x83, 0xC1, width as u8]); // add rcx, width
    emitter.memory_wide(&[0x8B], EDX, frame.context); // mov rdx, [context]
                                                      // cmp rcx, [rdx + MEMORY_LENGTH]; jbe inside; trap
    emitter
        .bytes
        .extend_from_slice(&[0x48, 0x3B, 0x4A, MEMORY_LENGTH_OFFSET]);
    let inside = emitter.new_label();
    emitter.jump(&[0x0F, 0x86], Target::Label(inside));
    emitter.trap(frame, crate::STATUS_MEMORY_OUT_OF_BOUNDS);
    emitter.bind(inside);
    emitter.bytes.extend_from_slice(&[0x48, 0x8B, 0x12]); // mov rdx, [rdx]: the base
}

/// The variant name of an instruction, for error messages.
pub(crate) fn instruction_name(instruction: &IrInstr) -> String {
    let text = format!("{instruction:?}");
    text.split([' ', '{']).next().unwrap_or_default().to_owned()
}

pub(crate) fn terminator_name(terminator: &Terminator) -> &'static str {
    match terminator {
        Terminator::Return(_) => "return",
        Terminator::Trap(_) => "trap",
        Terminator::Unreachable => "unreachable",
        Terminator::Branch { .. } => "branch",
        Terminator::CondBranch { .. } => "conditional branch",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_wasm_ir::{BasicBlock, BlockId, IrValue};
    use tpt_wasm_types::{FunctionType, ResultType, ValueType};

    fn one_block(instrs: Vec<IrInstr>, result: ValueId, values: Vec<IrValue>) -> IrFunction {
        IrFunction {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            params: Vec::new(),
            locals: Vec::new(),
            values,
            entry: BlockId(0),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instrs,
                terminator: Terminator::Return(vec![result]),
            }],
        }
    }

    fn i32_value(id: u32) -> IrValue {
        IrValue {
            id: ValueId(id),
            value_type: ValueType::I32,
        }
    }

    /// The bytes for `return 7`, worked out by hand. The frame holds the two saved pointers and
    /// one value: three 8-byte slots, so 24 bytes rounded to 32.
    #[test]
    fn a_rounding_function_needs_sse41_and_is_refused_without_it() {
        // f64.ceil of a constant, returning the rounded value (an f64, which the test function's
        // declared i32 result does not check: only the encoder runs here).
        let value = |id: u32, value_type: ValueType| IrValue {
            id: ValueId(id),
            value_type,
        };
        let function = one_block(
            vec![IrInstr::F64Unary {
                result: ValueId(1),
                value: ValueId(0),
                operation: FloatUnary::Ceil,
            }],
            ValueId(1),
            vec![value(0, ValueType::F64), value(1, ValueType::F64)],
        );
        assert!(
            matches!(
                encode(&function, Abi::SysV, &CallEnv::default(), false),
                Err(JitError::Unsupported(_))
            ),
            "without SSE4.1 the rounding function is refused, not approximated"
        );
        assert!(encode(&function, Abi::SysV, &CallEnv::default(), true).is_ok());
    }

    #[test]
    fn a_chain_inside_one_block_takes_registers_except_a_terminator_read() {
        // v0 and v1 are read only by v2's definition, and v2 only by v3's, all in one block. v3 is
        // returned, so the terminator reads it and it stays in its slot.
        let instrs = vec![
            IrInstr::ConstI32 {
                result: ValueId(0),
                value: 1,
            },
            IrInstr::ConstI32 {
                result: ValueId(1),
                value: 2,
            },
            IrInstr::I32Add {
                result: ValueId(2),
                left: ValueId(0),
                right: ValueId(1),
            },
            IrInstr::I32Add {
                result: ValueId(3),
                left: ValueId(2),
                right: ValueId(2),
            },
        ];
        let values = (0..4).map(i32_value).collect();
        let function = one_block(instrs, ValueId(3), values);
        let registers = allocate_registers(
            &function,
            &VALUE_REGISTERS,
            false,
            is_x86_register_instruction,
        );
        assert!(registers.contains_key(&ValueId(0)));
        assert!(registers.contains_key(&ValueId(1)));
        assert!(registers.contains_key(&ValueId(2)));
        assert!(
            !registers.contains_key(&ValueId(3)),
            "a value the terminator returns is read outside the register instructions"
        );
    }

    #[test]
    fn a_value_read_in_another_block_takes_no_register() {
        // v0 is defined in block 0 and read in block 1, so it must be in its slot.
        let function = IrFunction {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            params: Vec::new(),
            locals: Vec::new(),
            values: (0..2).map(i32_value).collect(),
            entry: BlockId(0),
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instrs: vec![IrInstr::ConstI32 {
                        result: ValueId(0),
                        value: 5,
                    }],
                    terminator: Terminator::Branch {
                        target: BlockId(1),
                        values: Vec::new(),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instrs: vec![IrInstr::I32Add {
                        result: ValueId(1),
                        left: ValueId(0),
                        right: ValueId(0),
                    }],
                    terminator: Terminator::Return(vec![ValueId(1)]),
                },
            ],
        };
        assert!(allocate_registers(
            &function,
            &VALUE_REGISTERS,
            false,
            is_x86_register_instruction
        )
        .is_empty());
    }

    #[test]
    fn a_constant_return_has_the_expected_encoding() {
        let function = one_block(
            vec![IrInstr::ConstI32 {
                result: ValueId(0),
                value: 7,
            }],
            ValueId(0),
            vec![i32_value(0)],
        );
        let code = encode(&function, Abi::SysV, &CallEnv::default(), false).expect("compiles");
        assert_eq!(
            code,
            vec![
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
                0x48, 0x81, 0xEC, 0x10, 0x00, 0x00, 0x00, // sub rsp, 16
                0x48, 0x89, 0xBD, 0xF8, 0xFF, 0xFF,
                0xFF, // mov [rbp-8], rdi: the args pointer
                0x48, 0x89, 0xB5, 0xF0, 0xFF, 0xFF, 0xFF, // mov [rbp-16], rsi: the context
                0x41, 0xB8, 0x07, 0x00, 0x00,
                0x00, // mov r8d, 7: the constant lives in a register
                0x4C, 0x89, 0xC0, // mov rax, r8: the return
                0xC9, // leave
                0xC3, // ret
            ]
        );
    }

    #[test]
    fn the_context_offsets_match_the_context_layout() {
        assert_eq!(STATUS_OFFSET, 16);
        assert_eq!(MEMORY_LENGTH_OFFSET, 8);
    }

    #[test]
    fn comparison_condition_codes_match_the_setcc_table() {
        assert_eq!(condition_code(IntComparison::Eq), 0x4); // sete
        assert_eq!(condition_code(IntComparison::Ne), 0x5); // setne
        assert_eq!(condition_code(IntComparison::LtS), 0xC); // setl
        assert_eq!(condition_code(IntComparison::LtU), 0x2); // setb
        assert_eq!(condition_code(IntComparison::GeU), 0x3); // setae
        assert_eq!(condition_code(IntComparison::GtS), 0xF); // setg
    }

    #[test]
    fn a_back_edge_is_a_backward_jump() {
        // Block 1 branches to itself, so the code ends with a jump whose displacement is negative.
        let function = IrFunction {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            params: Vec::new(),
            locals: Vec::new(),
            values: vec![i32_value(0)],
            entry: BlockId(0),
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instrs: Vec::new(),
                    terminator: Terminator::Branch {
                        target: BlockId(1),
                        values: Vec::new(),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instrs: Vec::new(),
                    terminator: Terminator::Branch {
                        target: BlockId(1),
                        values: Vec::new(),
                    },
                },
            ],
        };
        let code = encode(&function, Abi::SysV, &CallEnv::default(), false).expect("compiles");
        let tail = &code[code.len() - 5..];
        assert_eq!(tail[0], 0xE9);
        let displacement = i32::from_le_bytes(tail[1..5].try_into().unwrap());
        assert!(displacement < 0, "a back edge jumps backwards");
    }
}
