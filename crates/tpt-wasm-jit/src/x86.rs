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
    BasicBlock, BlockId, IntComparison, IntConversion, IrFunction, IrInstr, MemoryLoad,
    MemoryStore, SignExtend, Terminator, ValueId,
};
use tpt_wasm_types::ValueType;

use crate::{Context, JitError};

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
}

impl Frame {
    fn new(function: &IrFunction) -> Self {
        Self {
            slots: HashMap::new(),
            types: function
                .values
                .iter()
                .map(|value| (value.id, value.value_type))
                .collect(),
            used: 0,
            context: 0,
        }
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

    /// The frame size: the slots, rounded up to a multiple of 16.
    fn size(&self) -> i32 {
        (self.used + 15) / 16 * 16
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
}

impl Emitter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
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
        self.bytes.push(0xC9);
        self.bytes.push(0xC3);
    }
}

/// Compile `function` to machine code for `abi`.
pub(crate) fn encode(function: &IrFunction, abi: Abi) -> Result<Vec<u8>, JitError> {
    check_supported(function)?;
    let blocks: HashMap<BlockId, &BasicBlock> = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();

    let mut frame = Frame::new(function);
    let mut emitter = Emitter::new();

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
        let slot = frame.slot(*parameter);
        emitter.store64(EAX, slot);
    }
    // A declared local starts at zero, as Wasm requires; parameters are already set.
    let parameters: HashSet<ValueId> = function.params.iter().copied().collect();
    for local in &function.locals {
        if !parameters.contains(local) {
            let slot = frame.slot(*local);
            emitter.zero64(slot);
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
            let slot = frame.slot(values[0]);
            // A 32-bit result is loaded at 32 bits, which zero-extends; a 64-bit one at 64.
            match frame.value_type(values[0]) {
                Some(ValueType::I64) => emitter.load64(EAX, slot),
                _ => emitter.load32(EAX, slot),
            }
            emitter.epilogue();
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
            let condition = frame.slot(*condition);
            emitter.compare_with_zero32(condition);
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
    let temporaries: Vec<i32> = values
        .iter()
        .map(|value| {
            let source = frame.slot(*value);
            let temporary = frame.temporary();
            emitter.load64(EAX, source);
            emitter.store64(EAX, temporary);
            temporary
        })
        .collect();
    for (parameter, temporary) in parameters.iter().zip(temporaries) {
        let destination = frame.slot(*parameter);
        emitter.load64(EAX, temporary);
        emitter.store64(EAX, destination);
    }
}

/// Refuse anything the encoder would otherwise approximate.
pub(crate) fn check_supported(function: &IrFunction) -> Result<(), JitError> {
    if function.blocks.is_empty() {
        return Err(JitError::Unsupported("a function with no blocks".into()));
    }
    if function.function_type.results.0.len() != 1 {
        return Err(JitError::Unsupported(
            "a function that does not return exactly one value".into(),
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

fn emit_instruction(
    instruction: &IrInstr,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    match instruction {
        IrInstr::ConstI32 { result, value } => {
            let slot = frame.slot(*result);
            emitter.store_immediate32(slot, *value);
        }
        IrInstr::ConstI64 { result, value } => {
            let slot = frame.slot(*result);
            emitter.movabs(EAX, *value);
            emitter.store64(EAX, slot);
        }
        IrInstr::Drop { .. } => {}
        IrInstr::LocalGet { result, local } => {
            let source = frame.slot(*local);
            let destination = frame.slot(*result);
            emitter.load64(EAX, source);
            emitter.store64(EAX, destination);
        }
        IrInstr::LocalSet { local, value } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*local);
            emitter.load64(EAX, source);
            emitter.store64(EAX, destination);
        }
        IrInstr::LocalTee {
            result,
            local,
            value,
        } => {
            let source = frame.slot(*value);
            let local_slot = frame.slot(*local);
            let result_slot = frame.slot(*result);
            emitter.load64(EAX, source);
            emitter.store64(EAX, local_slot);
            emitter.store64(EAX, result_slot);
        }
        IrInstr::I32Add {
            result,
            left,
            right,
        } => binary32(OP_ADD, *result, *left, *right, frame, emitter),
        IrInstr::I32Sub {
            result,
            left,
            right,
        } => binary32(OP_SUB, *result, *left, *right, frame, emitter),
        IrInstr::I32Mul {
            result,
            left,
            right,
        } => binary32(OP_IMUL, *result, *left, *right, frame, emitter),
        IrInstr::I32And {
            result,
            left,
            right,
        } => binary32(OP_AND, *result, *left, *right, frame, emitter),
        IrInstr::I32Or {
            result,
            left,
            right,
        } => binary32(OP_OR, *result, *left, *right, frame, emitter),
        IrInstr::I32Xor {
            result,
            left,
            right,
        } => binary32(OP_XOR, *result, *left, *right, frame, emitter),
        IrInstr::I64Add {
            result,
            left,
            right,
        } => binary64(OP_ADD, *result, *left, *right, frame, emitter),
        IrInstr::I64Sub {
            result,
            left,
            right,
        } => binary64(OP_SUB, *result, *left, *right, frame, emitter),
        IrInstr::I64Mul {
            result,
            left,
            right,
        } => binary64(OP_IMUL, *result, *left, *right, frame, emitter),
        IrInstr::I64And {
            result,
            left,
            right,
        } => binary64(OP_AND, *result, *left, *right, frame, emitter),
        IrInstr::I64Or {
            result,
            left,
            right,
        } => binary64(OP_OR, *result, *left, *right, frame, emitter),
        IrInstr::I64Xor {
            result,
            left,
            right,
        } => binary64(OP_XOR, *result, *left, *right, frame, emitter),
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
        IrInstr::I32Compare {
            result,
            left,
            right,
            comparison,
        } => {
            let left = frame.slot(*left);
            let right = frame.slot(*right);
            let destination = frame.slot(*result);
            emitter.load32(EAX, left);
            emitter.memory(OP_CMP, EAX, right);
            emitter.set_from_flags(condition_code(*comparison));
            emitter.store32(EAX, destination);
        }
        IrInstr::I64Compare {
            result,
            left,
            right,
            comparison,
        } => {
            let left = frame.slot(*left);
            let right = frame.slot(*right);
            let destination = frame.slot(*result);
            emitter.load64(EAX, left);
            emitter.memory_wide(OP_CMP, EAX, right);
            emitter.set_from_flags(condition_code(*comparison));
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
            let wide = frame.value_type(*result) == Some(ValueType::I64);
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
                MemoryLoad::I32 => (4, &[0x8B, 0x04, 0x02]),
                MemoryLoad::I32_8U => (1, &[0x0F, 0xB6, 0x04, 0x02]),
                MemoryLoad::I32_8S => (1, &[0x0F, 0xBE, 0x04, 0x02]),
                MemoryLoad::I32_16U => (2, &[0x0F, 0xB7, 0x04, 0x02]),
                MemoryLoad::I32_16S => (2, &[0x0F, 0xBF, 0x04, 0x02]),
                MemoryLoad::I64 => (8, &[0x48, 0x8B, 0x04, 0x02]),
                MemoryLoad::I64_8U => (1, &[0x0F, 0xB6, 0x04, 0x02]),
                MemoryLoad::I64_8S => (1, &[0x48, 0x0F, 0xBE, 0x04, 0x02]),
                MemoryLoad::I64_16U => (2, &[0x0F, 0xB7, 0x04, 0x02]),
                MemoryLoad::I64_16S => (2, &[0x48, 0x0F, 0xBF, 0x04, 0x02]),
                MemoryLoad::I64_32U => (4, &[0x8B, 0x04, 0x02]),
                MemoryLoad::I64_32S => (4, &[0x48, 0x63, 0x04, 0x02]),
                other => return Err(JitError::Unsupported(format!("the {other:?} load"))),
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
            // The value is loaded into r8 and the store takes its low bits. An eight-byte store
            // uses the wide forms; the narrower ones store the low 8, 16, or 32 bits.
            let (width, access, wide): (u32, &[u8], bool) = match operation {
                MemoryStore::I32 => (4, &[0x44, 0x89, 0x04, 0x02], false),
                MemoryStore::I32_8 | MemoryStore::I64_8 => (1, &[0x44, 0x88, 0x04, 0x02], false),
                MemoryStore::I32_16 | MemoryStore::I64_16 => {
                    (2, &[0x66, 0x44, 0x89, 0x04, 0x02], false)
                }
                MemoryStore::I64 => (8, &[0x4C, 0x89, 0x04, 0x02], true),
                MemoryStore::I64_32 => (4, &[0x44, 0x89, 0x04, 0x02], false),
                other => return Err(JitError::Unsupported(format!("the {other:?} store"))),
            };
            emit_address(*address, *offset, width, frame, emitter);
            let source = frame.slot(*value);
            emitter.memory_with(wide, &[0x8B], 8, source); // mov r8(d), [value]
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

/// `result = left op right` on 32-bit values.
fn binary32(
    opcode: &[u8],
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    emitter.load32(EAX, left);
    emitter.memory(opcode, EAX, right);
    emitter.store32(EAX, destination);
}

/// `result = left op right` on 64-bit values.
fn binary64(
    opcode: &[u8],
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    emitter.load64(EAX, left);
    emitter.memory_wide(opcode, EAX, right);
    emitter.store64(EAX, destination);
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
    fn a_constant_return_has_the_expected_encoding() {
        let function = one_block(
            vec![IrInstr::ConstI32 {
                result: ValueId(0),
                value: 7,
            }],
            ValueId(0),
            vec![i32_value(0)],
        );
        let code = encode(&function, Abi::SysV).expect("compiles");
        assert_eq!(
            code,
            vec![
                0x55, // push rbp
                0x48, 0x89, 0xE5, // mov rbp, rsp
                0x48, 0x81, 0xEC, 0x20, 0x00, 0x00, 0x00, // sub rsp, 32
                0x48, 0x89, 0xBD, 0xF8, 0xFF, 0xFF,
                0xFF, // mov [rbp-8], rdi: the args pointer
                0x48, 0x89, 0xB5, 0xF0, 0xFF, 0xFF, 0xFF, // mov [rbp-16], rsi: the context
                0xC7, 0x85, 0xE8, 0xFF, 0xFF, 0xFF, 0x07, 0x00, 0x00, 0x00, // mov [rbp-24], 7
                0x8B, 0x85, 0xE8, 0xFF, 0xFF, 0xFF, // mov eax, [rbp-24]
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
        let code = encode(&function, Abi::SysV).expect("compiles");
        let tail = &code[code.len() - 5..];
        assert_eq!(tail[0], 0xE9);
        let displacement = i32::from_le_bytes(tail[1..5].try_into().unwrap());
        assert!(displacement < 0, "a back edge jumps backwards");
    }
}
