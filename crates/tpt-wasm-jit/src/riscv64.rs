// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The RISC-V (RV64GC, LP64) encoder for the compiled subset. Safe code, tested on every target.
//!
//! Every value lives in an 8-byte slot at a fixed offset from `sp`, and each slot holds a 64-bit
//! value. An `i32` is kept sign-extended, which is what the `W` forms of the arithmetic produce
//! and what `lw` loads, so the 64-bit comparisons order `i32` values correctly. Each instruction
//! loads its operands into scratch registers, performs one operation, and stores the result.
//!
//! The calling convention is the LP64 one: the arguments pointer arrives in `a0` and the context
//! pointer in `a1`. The result is returned in `a0`. Traps are stored in the context. Scratch
//! registers are `t0` to `t4`. `s0` is the frame pointer, callee-saved, so the prologue saves it
//! and the epilogue restores it, and every epilogue recomputes `sp` from `s0`.
//!
//! Limits: the loads and stores take 12-bit signed offsets, so a frame is at most 2 KiB and a
//! memory offset is below 2 GiB minus 4 KiB. A conditional branch reaches 4 KiB and a jump 1 MiB,
//! and a function beyond these is refused with `Unsupported`.

use std::collections::{HashMap, HashSet, VecDeque};

use tpt_wasm_ir::{
    BasicBlock, BlockId, FloatComparison, FloatUnary, IntComparison, IntConversion, IrFunction,
    IrInstr, MemoryLoad, MemoryStore, SignExtend, Terminator, ValueId,
};

use crate::x86::{check_supported, instruction_name, terminator_name};
use crate::{
    CallEnv, Context, JitError, STATUS_CALL_DEPTH, STATUS_INDIRECT_TYPE_MISMATCH,
    STATUS_NULL_REFERENCE, STATUS_TABLE_OUT_OF_BOUNDS,
};

/// Register numbers.
const ZERO: u32 = 0;
const SP: u32 = 2;
const S0: u32 = 8;
const T0: u32 = 5;
const T1: u32 = 6;
const T2: u32 = 7;
const T3: u32 = 28;
const T4: u32 = 29;
const A0: u32 = 10;
const A1: u32 = 11;
/// The return address, which a call writes and the prologue saves.
const RA: u32 = 1;
const FUNCTIONS_OFFSET: i32 = std::mem::offset_of!(Context, functions) as i32;
const DEPTH_OFFSET: i32 = std::mem::offset_of!(Context, depth) as i32;
const DEPTH_LIMIT_OFFSET: i32 = std::mem::offset_of!(Context, depth_limit) as i32;
const TABLE_ENTRIES_OFFSET: i32 = std::mem::offset_of!(Context, table_entries) as i32;
const TABLE_TYPES_OFFSET: i32 = std::mem::offset_of!(Context, table_types) as i32;
const TABLE_LEN_OFFSET: i32 = std::mem::offset_of!(Context, table_len) as i32;

/// The largest frame: a 12-bit signed offset reaches the highest slot.
const MAX_FRAME: u32 = 2048;
/// The largest memory offset, leaving room for the access width below it.
const MAX_OFFSET: u32 = (1 << 31) - 4096;

/// The byte offsets of the context fields the generated code uses.
const MEMORY_BASE_OFFSET: i32 = std::mem::offset_of!(Context, memory) as i32;
const MEMORY_LENGTH_OFFSET: i32 = std::mem::offset_of!(Context, memory_len) as i32;
const STATUS_OFFSET: i32 = std::mem::offset_of!(Context, status) as i32;

/// A place a jump can go: a block of the function, or a label inside the emitted code.
#[derive(Clone, Copy, Debug)]
enum Target {
    Block(BlockId),
    Label(usize),
}

/// The branch encodings whose offsets are patched.
#[derive(Clone, Copy, Debug)]
enum Fixup {
    /// A conditional branch: 13-bit signed byte offset.
    Branch13,
    /// `jal`: 21-bit signed byte offset.
    Jump21,
}

/// The stack slots of a function, as offsets from `sp`.
struct Frame {
    slots: HashMap<ValueId, u32>,
    used: u32,
    context: u32,
    /// What the function's calls may name, and the table they may index.
    env: CallEnv,
}

impl Frame {
    fn new(env: &CallEnv) -> Self {
        Self {
            slots: HashMap::new(),
            used: 0,
            context: 0,
            env: env.clone(),
        }
    }

    /// A block of `words` consecutive slots, and the offset of its first word. The words ascend
    /// from that offset, so the block can be passed as an array of arguments.
    fn block(&mut self, words: usize) -> u32 {
        let offset = self.used;
        self.used += 8 * words as u32;
        offset
    }

    fn is_callable(&self, function: u32) -> bool {
        self.env
            .callable
            .get(function as usize)
            .copied()
            .unwrap_or(false)
    }

    fn slot(&mut self, value: ValueId) -> u32 {
        if let Some(&offset) = self.slots.get(&value) {
            return offset;
        }
        let offset = self.used;
        self.used += 8;
        self.slots.insert(value, offset);
        offset
    }

    fn temporary(&mut self) -> u32 {
        let offset = self.used;
        self.used += 8;
        offset
    }

    /// The frame size: the slots, rounded up to 16 as the ABI requires.
    fn size(&self) -> u32 {
        self.used.div_ceil(16) * 16
    }
}

/// The instruction words, with the labels and fixups that patch their branches.
struct Emitter {
    words: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Target, Fixup)>,
}

impl Emitter {
    fn new() -> Self {
        Self {
            words: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    fn push(&mut self, word: u32) {
        self.words.push(word);
    }

    fn new_label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }

    fn bind(&mut self, label: usize) {
        self.labels[label] = Some(self.words.len());
    }

    /// A branch or jump whose offset is patched later. `word` is the instruction with a zero
    /// offset.
    fn branch(&mut self, word: u32, target: Target, fixup: Fixup) {
        self.fixups.push((self.words.len(), target, fixup));
        self.push(word);
    }

    /// Patch every branch now that the word offset of each block is known. Offsets are in bytes,
    /// and each instruction is four bytes.
    fn resolve(&mut self, blocks: &HashMap<BlockId, usize>) -> Result<(), JitError> {
        for (position, target, fixup) in std::mem::take(&mut self.fixups) {
            let destination = match target {
                Target::Block(block) => *blocks.get(&block).ok_or_else(|| {
                    JitError::Unsupported("a branch to a block that was not emitted".into())
                })?,
                Target::Label(label) => self.labels[label]
                    .ok_or_else(|| JitError::Unsupported("a branch to an unbound label".into()))?,
            };
            let offset = (destination as i64 - position as i64) * 4;
            match fixup {
                Fixup::Branch13 => {
                    if !(-4096..4096).contains(&offset) {
                        return Err(JitError::Unsupported(
                            "a conditional branch beyond 4 KiB".into(),
                        ));
                    }
                    self.words[position] |= b_immediate(offset as u32);
                }
                Fixup::Jump21 => {
                    if !(-(1 << 20)..(1 << 20)).contains(&offset) {
                        return Err(JitError::Unsupported("a jump beyond 1 MiB".into()));
                    }
                    self.words[position] |= j_immediate(offset as u32);
                }
            }
        }
        Ok(())
    }
}

// ---- instruction encodings. Each returns one 32-bit word.

/// The I-type form: a 12-bit immediate, `rs1`, `funct3`, `rd`, and the opcode.
fn i_type(imm: i32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
    (((imm as u32) & 0xFFF) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

/// The S-type form: a 12-bit immediate split across the store, `rs2` and `rs1`.
fn s_type(imm: i32, rs2: u32, rs1: u32, funct3: u32, opcode: u32) -> u32 {
    let imm = (imm as u32) & 0xFFF;
    ((imm >> 5) << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | ((imm & 0x1F) << 7) | opcode
}

/// The R-type form.
fn r_type(funct7: u32, rs2: u32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
    (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

/// The offset bits of a B-type word, for a byte offset in -4 KiB..4 KiB.
fn b_immediate(offset: u32) -> u32 {
    let imm = offset & 0x1FFE;
    (((imm >> 12) & 1) << 31)
        | (((imm >> 5) & 0x3F) << 25)
        | (((imm >> 1) & 0xF) << 8)
        | (((imm >> 11) & 1) << 7)
}

/// The offset bits of a J-type word, for a byte offset in -1 MiB..1 MiB.
fn j_immediate(offset: u32) -> u32 {
    let imm = offset & 0x1F_FFFE;
    (((imm >> 20) & 1) << 31)
        | (((imm >> 1) & 0x3FF) << 21)
        | (((imm >> 11) & 1) << 20)
        | (((imm >> 12) & 0xFF) << 12)
}

/// `addi rd, rs1, imm`.
fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 0, rd, 0x13)
}

/// `addiw rd, rs1, imm`: a 32-bit add, sign-extended to 64 bits.
fn addiw(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 0, rd, 0x1B)
}

/// `ori rd, rs1, imm`.
fn ori(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 6, rd, 0x13)
}

/// `xori rd, rs1, imm`.
fn xori(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 4, rd, 0x13)
}

/// `sltiu rd, rs1, imm`.
fn sltiu(rd: u32, rs1: u32, imm: i32) -> u32 {
    i_type(imm, rs1, 3, rd, 0x13)
}

/// `lui rd, imm20`: the upper 20 bits, sign-extended from bit 31.
fn lui(rd: u32, imm20: u32) -> u32 {
    ((imm20 & 0xF_FFFF) << 12) | (rd << 7) | 0x37
}

/// `slli rd, rs1, shamt`: a 64-bit logical left shift.
fn slli(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type(shamt as i32, rs1, 1, rd, 0x13)
}

/// `srli rd, rs1, shamt`: a 64-bit logical right shift.
fn srli(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type(shamt as i32, rs1, 5, rd, 0x13)
}

/// `srai rd, rs1, shamt`: a 64-bit arithmetic right shift.
fn srai(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type((0x400 | shamt) as i32, rs1, 5, rd, 0x13)
}

/// A load, by `funct3`: `lb` (0), `lh` (1), `lw` (2), `ld` (3), `lbu` (4), `lhu` (5), `lwu` (6).
fn load(funct3: u32, rd: u32, rs1: u32, offset: i32) -> u32 {
    i_type(offset, rs1, funct3, rd, 0x03)
}

/// A store, by `funct3`: `sb` (0), `sh` (1), `sw` (2), `sd` (3).
fn store(funct3: u32, rs2: u32, rs1: u32, offset: i32) -> u32 {
    s_type(offset, rs2, rs1, funct3, 0x23)
}

/// A 32-bit register-register operation (`OP-32`).
fn op_w(funct7: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(funct7, rs2, rs1, funct3, rd, 0x3B)
}

/// A 64-bit register-register operation (`OP`).
fn op(funct7: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(funct7, rs2, rs1, funct3, rd, 0x33)
}

/// `beq` (funct3 0), `bne` (1), `bgeu` (7), with a zero offset to be patched.
fn branch(funct3: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(0, rs2, rs1, funct3, 0, 0x63)
}

/// `jal x0`, a plain jump, with a zero offset to be patched.
const JAL_ZERO: u32 = 0x6F;

/// `jalr x0, 0(ra)`: return.
const RET: u32 = 0x0000_8067;

/// The 32-bit constant `value`, sign-extended to 64 bits, in `rd`.
fn load_constant32(rd: u32, value: i32, emitter: &mut Emitter) {
    if (-2048..2048).contains(&value) {
        emitter.push(addi(rd, ZERO, value));
        return;
    }
    // hi is the upper 20 bits, rounded so that the signed low 12 bits fit.
    let hi = value.wrapping_add(0x800) >> 12;
    let lo = value.wrapping_sub(hi << 12);
    emitter.push(lui(rd, hi as u32));
    emitter.push(addiw(rd, rd, lo));
}

/// The 64-bit constant `value` in `rd`, built from 11-bit pieces so that every immediate is
/// positive and fits.
fn load_constant64(rd: u32, value: i64, emitter: &mut Emitter) {
    if let Ok(small) = i32::try_from(value) {
        load_constant32(rd, small, emitter);
        return;
    }
    let bits = value as u64;
    let pieces = [
        ((bits >> 53) & 0x7FF, 11),
        ((bits >> 42) & 0x7FF, 11),
        ((bits >> 31) & 0x7FF, 11),
        ((bits >> 20) & 0x7FF, 11),
        ((bits >> 9) & 0x7FF, 11),
        (bits & 0x1FF, 9),
    ];
    emitter.push(addi(rd, ZERO, pieces[0].0 as i32));
    for (piece, width) in &pieces[1..] {
        emitter.push(slli(rd, rd, *width));
        if *piece != 0 {
            emitter.push(ori(rd, rd, *piece as i32));
        }
    }
}

/// Compile `function` to RISC-V machine code.
pub(crate) fn encode(function: &IrFunction, env: &CallEnv) -> Result<Vec<u8>, JitError> {
    check_supported(function)?;
    let blocks: HashMap<BlockId, &BasicBlock> = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();

    let mut frame = Frame::new(env);
    let mut emitter = Emitter::new();

    // The two pointers arrive in a0 and a1 and are kept in slots, because the code below uses the
    // scratch registers freely. Each argument is an 8-byte slot in the arguments array.
    let args = frame.temporary();
    frame.context = frame.temporary();
    emitter.push(store(3, A0, SP, args as i32));
    emitter.push(store(3, A1, SP, frame.context as i32));
    for (index, parameter) in function.params.iter().enumerate() {
        emitter.push(load(3, T0, SP, args as i32));
        emitter.push(load(3, T0, T0, 8 * index as i32));
        let slot = frame.slot(*parameter);
        emitter.push(store(3, T0, SP, slot as i32));
    }
    let parameters: HashSet<ValueId> = function.params.iter().copied().collect();
    for local in &function.locals {
        if !parameters.contains(local) {
            let slot = frame.slot(*local);
            emitter.push(store(3, ZERO, SP, slot as i32));
        }
    }

    let mut offsets: HashMap<BlockId, usize> = HashMap::new();
    let mut queue: VecDeque<BlockId> = VecDeque::from([function.entry]);
    let mut scheduled: HashSet<BlockId> = HashSet::from([function.entry]);
    while let Some(id) = queue.pop_front() {
        let block = blocks
            .get(&id)
            .copied()
            .ok_or_else(|| JitError::Unsupported("a branch to a missing block".into()))?;
        offsets.insert(id, emitter.words.len());
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
    // The prologue: save s0 and the return address, which a call overwrites, point s0 at the
    // caller's sp, and reserve the frame.
    let mut words = vec![
        addi(SP, SP, -16),
        store(3, S0, SP, 0),
        store(3, RA, SP, 8),
        addi(S0, SP, 16),
        addi(SP, SP, -(size as i32)),
    ];
    words.extend(emitter.words);
    Ok(words
        .into_iter()
        .flat_map(|word| word.to_le_bytes())
        .collect())
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
            emitter.push(load(3, A0, SP, slot as i32));
            epilogue(emitter);
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
            emitter.branch(JAL_ZERO, Target::Block(*target), Fixup::Jump21);
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
            let slot = frame.slot(*condition);
            emitter.push(load(3, T0, SP, slot as i32));
            let else_path = emitter.new_label();
            // beq t0, zero, else: a zero condition takes the else edge.
            emitter.branch(
                branch(0, T0, ZERO),
                Target::Label(else_path),
                Fixup::Branch13,
            );
            copy_into_parameters(&then_block.params, then_values, frame, emitter);
            emitter.branch(JAL_ZERO, Target::Block(*then_target), Fixup::Jump21);
            emitter.bind(else_path);
            copy_into_parameters(&else_block.params, else_values, frame, emitter);
            emitter.branch(JAL_ZERO, Target::Block(*else_target), Fixup::Jump21);
            Ok(vec![*then_target, *else_target])
        }
        other => Err(JitError::Unsupported(format!(
            "the {} terminator",
            terminator_name(other)
        ))),
    }
}

/// Restore `s0` and `sp`, then return. `s0` holds the caller's `sp` less 16, so `sp` is recovered
/// from it and does not depend on the frame size.
fn epilogue(emitter: &mut Emitter) {
    emitter.push(addi(SP, S0, -16));
    emitter.push(load(3, S0, SP, 0));
    emitter.push(load(3, RA, SP, 8));
    emitter.push(addi(SP, SP, 16));
    emitter.push(RET);
}

/// Store `status` in the context and return zero.
fn trap(frame: &Frame, status: u32, emitter: &mut Emitter) {
    emitter.push(load(3, T4, SP, frame.context as i32)); // the context
    emitter.push(addi(T3, ZERO, status as i32));
    emitter.push(store(3, T3, T4, STATUS_OFFSET));
    emitter.push(addi(A0, ZERO, 0));
    epilogue(emitter);
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

/// Copy branch values into the target's parameter slots, through temporaries so that the order of
/// the copies cannot change what a value means. Each copy is eight bytes.
fn copy_into_parameters(
    parameters: &[ValueId],
    values: &[ValueId],
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let temporaries: Vec<u32> = values
        .iter()
        .map(|value| {
            let source = frame.slot(*value);
            let temporary = frame.temporary();
            emitter.push(load(3, T0, SP, source as i32));
            emitter.push(store(3, T0, SP, temporary as i32));
            temporary
        })
        .collect();
    for (parameter, temporary) in parameters.iter().zip(temporaries) {
        let destination = frame.slot(*parameter);
        emitter.push(load(3, T0, SP, temporary as i32));
        emitter.push(store(3, T0, SP, destination as i32));
    }
}

/// A direct call to another function of the module, through the call table in the context.
///
/// The arguments are copied to a block of slots in this frame, and `a0` points at the block. The
/// context is passed in `a1`. The call counts a frame against the depth limit first, and a call
/// past the limit traps. A trap in the callee leaves its status in the context and returns zero,
/// so the status is checked after the call, and a non-zero status returns zero from here too.
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
    let [result] = results else {
        return Err(JitError::Unsupported(
            "a call with other than one result".into(),
        ));
    };
    let block = frame.block(arguments.len().max(1));
    if block >= 2048 {
        return Err(JitError::Unsupported(
            "an argument block beyond the reach of a 12-bit offset".into(),
        ));
    }
    store_arguments(arguments, block, frame, emitter);
    let destination = frame.slot(*result);
    enter_callee(frame, emitter);
    // a0 = the argument block; a1 = the context; t0 = the callee's entry, the table at
    // `functions` plus eight bytes per function, loaded through `jalr`.
    emitter.push(addi(A0, SP, block as i32));
    emitter.push(addi(A1, T2, 0));
    emitter.push(load(3, T0, T2, FUNCTIONS_OFFSET));
    load_constant32(T1, 8 * function as i32, emitter);
    emitter.push(op(0, 0, T0, T0, T1)); // add t0, t0, t1
    emitter.push(load(3, T0, T0, 0));
    emitter.push(i_type(0, T0, 0, RA, 0x67)); // jalr ra, 0(t0)
    leave_callee(frame, destination, emitter);
    Ok(())
}

/// An indirect call through the module's table, checked in Micro's order: the index against the
/// table's length, then the slot against null, then the slot's signature against the call's.
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
        .ok_or_else(|| JitError::Unsupported("an indirect call to an unknown type".into()))?;
    let [result] = results else {
        return Err(JitError::Unsupported(
            "a call with other than one result".into(),
        ));
    };
    let block = frame.block(arguments.len().max(1));
    if block >= 2048 {
        return Err(JitError::Unsupported(
            "an argument block beyond the reach of a 12-bit offset".into(),
        ));
    }
    store_arguments(arguments, block, frame, emitter);
    let operand_slot = frame.slot(operand);
    let destination = frame.slot(*result);
    let entry = frame.temporary();

    // t0 = the index, zero-extended from its i32 slot (lwu); t2 = the context.
    emitter.push(load(6, T0, SP, operand_slot as i32));
    emitter.push(load(3, T2, SP, frame.context as i32));
    // Out of range traps: bltu t0, len, ok.
    emitter.push(load(3, T1, T2, TABLE_LEN_OFFSET));
    guard_trap(frame, 6, T0, T1, STATUS_TABLE_OUT_OF_BOUNDS, emitter);
    // t1 = the slot's type: types[index], where a null slot holds -1.
    emitter.push(load(3, T1, T2, TABLE_TYPES_OFFSET));
    emitter.push(slli(T3, T0, 3));
    emitter.push(op(0, 0, T1, T1, T3));
    emitter.push(load(3, T1, T1, 0));
    // Null traps: bne t1, -1, ok.
    emitter.push(addi(T3, ZERO, -1));
    guard_trap(frame, 1, T1, T3, STATUS_NULL_REFERENCE, emitter);
    // The slot's type against the call's: beq t1, expected, ok.
    load_constant64(T3, expected as i64, emitter);
    guard_trap(frame, 0, T1, T3, STATUS_INDIRECT_TYPE_MISMATCH, emitter);
    // t1 = the slot's entry, kept in a slot for the call.
    emitter.push(load(3, T1, T2, TABLE_ENTRIES_OFFSET));
    emitter.push(slli(T3, T0, 3));
    emitter.push(op(0, 0, T1, T1, T3));
    emitter.push(load(3, T1, T1, 0));
    emitter.push(store(3, T1, SP, entry as i32));

    enter_callee(frame, emitter);
    emitter.push(addi(A0, SP, block as i32));
    emitter.push(addi(A1, T2, 0));
    emitter.push(load(3, T0, SP, entry as i32));
    emitter.push(i_type(0, T0, 0, RA, 0x67)); // jalr ra, 0(t0)
    leave_callee(frame, destination, emitter);
    Ok(())
}

/// Copy each argument into the block that the callee reads as its argument array.
fn store_arguments(arguments: &[ValueId], block: u32, frame: &mut Frame, emitter: &mut Emitter) {
    for (index, argument) in arguments.iter().enumerate() {
        let source = frame.slot(*argument);
        emitter.push(load(3, T0, SP, source as i32));
        emitter.push(store(3, T0, SP, (block + 8 * index as u32) as i32));
    }
}

/// Count one more active frame against the depth limit, and trap if that is past it. Leaves the
/// context in `t2`.
fn enter_callee(frame: &Frame, emitter: &mut Emitter) {
    // t2 = context; t0 = depth; t1 = limit; trap unless depth < limit (bltu).
    emitter.push(load(3, T2, SP, frame.context as i32));
    emitter.push(load(3, T0, T2, DEPTH_OFFSET));
    emitter.push(load(3, T1, T2, DEPTH_LIMIT_OFFSET));
    guard_trap(frame, 6, T0, T1, STATUS_CALL_DEPTH, emitter);
    // depth += 1
    emitter.push(addi(T0, T0, 1));
    emitter.push(store(3, T0, T2, DEPTH_OFFSET));
}

/// After the callee returns: count the frame off, and if the callee left a status behind, return
/// zero without storing anything. Otherwise store the callee's result, which is in `a0`.
fn leave_callee(frame: &Frame, destination: u32, emitter: &mut Emitter) {
    // depth -= 1, then return zero if the callee left a status behind.
    emitter.push(load(3, T2, SP, frame.context as i32));
    emitter.push(load(3, T0, T2, DEPTH_OFFSET));
    emitter.push(addi(T0, T0, -1));
    emitter.push(store(3, T0, T2, DEPTH_OFFSET));
    emitter.push(load(3, T1, T2, STATUS_OFFSET));
    let guard = emitter.words.len();
    emitter.push(0); // patched below
    emitter.push(addi(A0, ZERO, 0));
    epilogue(emitter);
    let distance = 4 * (emitter.words.len() - guard) as u32;
    emitter.words[guard] = branch(0, T1, ZERO) | b_immediate(distance);
    emitter.push(store(3, A0, SP, destination as i32));
}

/// A conditional branch, `funct3` on `rs1` and `rs2`, that skips a trap with `status` when it is
/// taken. The trap is emitted in place, after the branch.
fn guard_trap(frame: &Frame, funct3: u32, rs1: u32, rs2: u32, status: u32, emitter: &mut Emitter) {
    let at = emitter.words.len();
    emitter.push(0); // patched below
    trap(frame, status, emitter);
    let distance = 4 * (emitter.words.len() - at) as u32;
    emitter.words[at] = branch(funct3, rs1, rs2) | b_immediate(distance);
}

fn emit_instruction(
    instruction: &IrInstr,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    match instruction {
        IrInstr::ConstI32 { result, value } => {
            let slot = frame.slot(*result);
            load_constant32(T0, *value, emitter);
            emitter.push(store(3, T0, SP, slot as i32));
        }
        IrInstr::ConstI64 { result, value } => {
            let slot = frame.slot(*result);
            load_constant64(T0, *value, emitter);
            emitter.push(store(3, T0, SP, slot as i32));
        }
        IrInstr::ConstF32 { result, value } => {
            let slot = frame.slot(*result);
            load_constant64(T0, i64::from(*value), emitter);
            emitter.push(store(3, T0, SP, slot as i32));
        }
        IrInstr::ConstF64 { result, value } => {
            let slot = frame.slot(*result);
            load_constant64(T0, *value as i64, emitter);
            emitter.push(store(3, T0, SP, slot as i32));
        }
        IrInstr::F32Add {
            result,
            left,
            right,
        } => float_binary(false, 0x00, *result, *left, *right, frame, emitter),
        IrInstr::F32Sub {
            result,
            left,
            right,
        } => float_binary(false, 0x04, *result, *left, *right, frame, emitter),
        IrInstr::F32Mul {
            result,
            left,
            right,
        } => float_binary(false, 0x08, *result, *left, *right, frame, emitter),
        IrInstr::F32Div {
            result,
            left,
            right,
        } => float_binary(false, 0x0C, *result, *left, *right, frame, emitter),
        IrInstr::F64Add {
            result,
            left,
            right,
        } => float_binary(true, 0x00, *result, *left, *right, frame, emitter),
        IrInstr::F64Sub {
            result,
            left,
            right,
        } => float_binary(true, 0x04, *result, *left, *right, frame, emitter),
        IrInstr::F64Mul {
            result,
            left,
            right,
        } => float_binary(true, 0x08, *result, *left, *right, frame, emitter),
        IrInstr::F64Div {
            result,
            left,
            right,
        } => float_binary(true, 0x0C, *result, *left, *right, frame, emitter),
        IrInstr::F32Compare {
            result,
            left,
            right,
            comparison,
        } => float_compare_into(false, *result, *left, *right, *comparison, frame, emitter),
        IrInstr::F64Compare {
            result,
            left,
            right,
            comparison,
        } => float_compare_into(true, *result, *left, *right, *comparison, frame, emitter),
        IrInstr::F32Unary {
            result,
            value,
            operation,
        } => float_unary_arm(false, *result, *value, *operation, frame, emitter)?,
        IrInstr::F64Unary {
            result,
            value,
            operation,
        } => float_unary_arm(true, *result, *value, *operation, frame, emitter)?,
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
        IrInstr::Drop { .. } => {}
        IrInstr::LocalGet { result, local } => copy(*local, *result, frame, emitter),
        IrInstr::LocalSet { local, value } => copy(*value, *local, frame, emitter),
        IrInstr::LocalTee {
            result,
            local,
            value,
        } => {
            let source = frame.slot(*value);
            let local_slot = frame.slot(*local);
            let result_slot = frame.slot(*result);
            emitter.push(load(3, T0, SP, source as i32));
            emitter.push(store(3, T0, SP, local_slot as i32));
            emitter.push(store(3, T0, SP, result_slot as i32));
        }
        // The `W` forms compute 32-bit results, sign-extended to 64 bits. Their shift counts are
        // the low five bits of the second operand, which is the Wasm masking rule.
        IrInstr::I32Add {
            result,
            left,
            right,
        } => binary(
            op_w(0x00, 0, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I32Sub {
            result,
            left,
            right,
        } => binary(
            op_w(0x20, 0, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I32Mul {
            result,
            left,
            right,
        } => binary(
            op_w(0x01, 0, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I32Shl {
            result,
            left,
            right,
        } => binary(
            op_w(0x00, 1, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I32ShrU {
            result,
            left,
            right,
        } => binary(
            op_w(0x00, 5, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I32ShrS {
            result,
            left,
            right,
        } => binary(
            op_w(0x20, 5, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        // The 64-bit forms. The bitwise operations need no `W` form: on sign-extended operands they
        // produce sign-extended results.
        IrInstr::I32And {
            result,
            left,
            right,
        }
        | IrInstr::I64And {
            result,
            left,
            right,
        } => binary(
            op(0x00, 7, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I32Or {
            result,
            left,
            right,
        }
        | IrInstr::I64Or {
            result,
            left,
            right,
        } => binary(
            op(0x00, 6, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I32Xor {
            result,
            left,
            right,
        }
        | IrInstr::I64Xor {
            result,
            left,
            right,
        } => binary(
            op(0x00, 4, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I64Add {
            result,
            left,
            right,
        } => binary(
            op(0x00, 0, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I64Sub {
            result,
            left,
            right,
        } => binary(
            op(0x20, 0, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I64Mul {
            result,
            left,
            right,
        } => binary(
            op(0x01, 0, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I64Shl {
            result,
            left,
            right,
        } => binary(
            op(0x00, 1, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I64ShrU {
            result,
            left,
            right,
        } => binary(
            op(0x00, 5, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
        IrInstr::I64ShrS {
            result,
            left,
            right,
        } => binary(
            op(0x20, 5, T0, T0, T1),
            *result,
            *left,
            *right,
            frame,
            emitter,
        ),
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
        IrInstr::I32Eqz { result, value } | IrInstr::I64Eqz { result, value } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            emitter.push(load(3, T0, SP, source as i32));
            emitter.push(sltiu(T0, T0, 1));
            emitter.push(store(3, T0, SP, destination as i32));
        }
        IrInstr::I32Compare {
            result,
            left,
            right,
            comparison,
        }
        | IrInstr::I64Compare {
            result,
            left,
            right,
            comparison,
        } => {
            let left = frame.slot(*left);
            let right = frame.slot(*right);
            let destination = frame.slot(*result);
            emitter.push(load(3, T0, SP, left as i32));
            emitter.push(load(3, T1, SP, right as i32));
            compare(*comparison, emitter);
            emitter.push(store(3, T0, SP, destination as i32));
        }
        IrInstr::Select {
            result,
            condition,
            left,
            right,
        } => {
            // The result is `left` when the condition is non-zero, else `right`. A branch picks
            // which operand is copied.
            let condition = frame.slot(*condition);
            let left = frame.slot(*left);
            let right = frame.slot(*right);
            let destination = frame.slot(*result);
            emitter.push(load(3, T0, SP, condition as i32));
            emitter.push(load(3, T1, SP, right as i32));
            emitter.push(load(3, T2, SP, left as i32));
            let take_left = emitter.new_label();
            let done = emitter.new_label();
            // bne t0, zero, take_left
            emitter.branch(
                branch(1, T0, ZERO),
                Target::Label(take_left),
                Fixup::Branch13,
            );
            emitter.push(addi(T0, T1, 0));
            emitter.branch(JAL_ZERO, Target::Label(done), Fixup::Jump21);
            emitter.bind(take_left);
            emitter.push(addi(T0, T2, 0));
            emitter.bind(done);
            emitter.push(store(3, T0, SP, destination as i32));
        }
        IrInstr::IntConvert {
            result,
            value,
            operation,
        } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            emitter.push(load(3, T0, SP, source as i32));
            match operation {
                // addiw by zero sign-extends the low 32 bits.
                IntConversion::I32WrapI64 => emitter.push(addiw(T0, T0, 0)),
                // The slot already holds the sign-extended 32-bit value.
                IntConversion::I64ExtendI32S => {}
                // Keep the low 32 bits, zero-extended.
                IntConversion::I64ExtendI32U => {
                    emitter.push(slli(T0, T0, 32));
                    emitter.push(srli(T0, T0, 32));
                }
            }
            emitter.push(store(3, T0, SP, destination as i32));
        }
        IrInstr::SignExtend {
            result,
            value,
            operation,
        } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            let shift = match operation {
                SignExtend::I32Extend8S | SignExtend::I64Extend8S => 56,
                SignExtend::I32Extend16S | SignExtend::I64Extend16S => 48,
                SignExtend::I64Extend32S => 32,
            };
            emitter.push(load(3, T0, SP, source as i32));
            emitter.push(slli(T0, T0, shift));
            emitter.push(srai(T0, T0, shift));
            emitter.push(store(3, T0, SP, destination as i32));
        }
        IrInstr::Load {
            result,
            address,
            offset,
            operation,
            bounds: _,
        } => {
            // The bounds hint is ignored: every access is checked here.
            let (width, funct3) = match operation {
                // An f32 or f64 load moves the same bits as the integer load of that width.
                MemoryLoad::I32 | MemoryLoad::F32 | MemoryLoad::I64_32S => (4, 2),
                MemoryLoad::I64_32U => (4, 6),
                MemoryLoad::I64 | MemoryLoad::F64 => (8, 3),
                MemoryLoad::I32_8U | MemoryLoad::I64_8U => (1, 4),
                MemoryLoad::I32_8S | MemoryLoad::I64_8S => (1, 0),
                MemoryLoad::I32_16U | MemoryLoad::I64_16U => (2, 5),
                MemoryLoad::I32_16S | MemoryLoad::I64_16S => (2, 1),
            };
            emit_address(*address, *offset, width, frame, emitter)?;
            emitter.push(load(funct3, T0, T3, 0));
            let destination = frame.slot(*result);
            emitter.push(store(3, T0, SP, destination as i32));
        }
        IrInstr::Store {
            address,
            value,
            offset,
            operation,
            bounds: _,
        } => {
            let (width, funct3) = match operation {
                MemoryStore::I32 | MemoryStore::F32 | MemoryStore::I64_32 => (4, 2),
                MemoryStore::I64 | MemoryStore::F64 => (8, 3),
                MemoryStore::I32_8 | MemoryStore::I64_8 => (1, 0),
                MemoryStore::I32_16 | MemoryStore::I64_16 => (2, 1),
            };
            emit_address(*address, *offset, width, frame, emitter)?;
            let source = frame.slot(*value);
            emitter.push(load(3, T1, SP, source as i32));
            emitter.push(store(funct3, T1, T3, 0));
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

/// Copy a value's slot to another, eight bytes at a time.
/// `fmv.w.x f<fd>, x<rs1>` (`wide`: `fmv.d.x`): a slot's bits into a float register. The 32-bit
/// form reads the low half.
fn fmv_to_float(wide: bool, fd: u32, rs1: u32) -> u32 {
    r_type(if wide { 0x79 } else { 0x78 }, 0, rs1, 0, fd, 0x53)
}

/// `fmv.x.w x<rd>, f<fs1>` (`wide`: `fmv.x.d`): a float register's bits back into a slot. The
/// 32-bit form sign-extends, which leaves the low half, the value, unchanged.
fn fmv_from_float(wide: bool, rd: u32, fs1: u32) -> u32 {
    r_type(if wide { 0x71 } else { 0x70 }, 0, fs1, 0, rd, 0x53)
}

/// `fadd`, `fsub`, `fmul`, or `fdiv` (by the single-precision `funct7`: 0x00, 0x04, 0x08, 0x0C)
/// with the round-to-nearest-even mode Wasm requires. The double form is one `funct7` higher.
fn float_arithmetic(wide: bool, funct7: u32, fd: u32, fs1: u32, fs2: u32) -> u32 {
    r_type(funct7 | u32::from(wide), fs2, fs1, 0, fd, 0x53)
}

/// `feq` (funct3 2), `flt` (1), or `fle` (0): a float comparison into an integer register.
fn float_compare_bit(wide: bool, funct3: u32, rd: u32, fs1: u32, fs2: u32) -> u32 {
    r_type(0x50 | u32::from(wide), fs2, fs1, funct3, rd, 0x53)
}

/// `fsgnjn` (negate) or, when `abs`, `fsgnjx` (absolute value) of a register with itself: a
/// sign-bit operation, exact on every input.
fn float_sign(wide: bool, abs: bool, fd: u32, fs: u32) -> u32 {
    let funct3 = if abs { 2 } else { 1 };
    r_type(0x10 | u32::from(wide), fs, fs, funct3, fd, 0x53)
}

/// `result = left op right` on floats: operands in `t0` and `t1`, moved to `f0` and `f1`, and the
/// result's bits moved back to `t0` for the store.
fn float_binary(
    wide: bool,
    funct7: u32,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    emitter.push(load(3, T0, SP, left as i32));
    emitter.push(load(3, T1, SP, right as i32));
    emitter.push(fmv_to_float(wide, 0, T0));
    emitter.push(fmv_to_float(wide, 1, T1));
    emitter.push(float_arithmetic(wide, funct7, 0, 0, 1));
    canonicalize_nan(wide, emitter);
    emitter.push(fmv_from_float(wide, T0, 0));
    emitter.push(store(3, T0, SP, destination as i32));
}

/// Replace a NaN in `f0` with the canonical NaN that Micro produces. `feq f0, f0` is 0 only for a
/// NaN, so the replacement runs only then.
fn canonicalize_nan(wide: bool, emitter: &mut Emitter) {
    emitter.push(float_compare_bit(wide, 2, T1, 0, 0)); // feq t1, f0, f0
    let guard = emitter.words.len();
    emitter.push(0); // bne t1, zero past the replacement, patched below
    let bits = if wide {
        0x7ff8_0000_0000_0000i64
    } else {
        0x7fc0_0000i64
    };
    load_constant64(T0, bits, emitter);
    emitter.push(fmv_to_float(wide, 0, T0));
    let distance = 4 * (emitter.words.len() - guard) as u32;
    emitter.words[guard] = branch(1, T1, ZERO) | b_immediate(distance);
}

/// A float comparison as 0 or 1 in the result slot. `feq`, `flt` and `fle` give 0 for an unordered
/// pair, so `!=` is `feq` inverted, which makes a NaN unequal to everything.
fn float_compare_into(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    comparison: FloatComparison,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    emitter.push(load(3, T0, SP, left as i32));
    emitter.push(load(3, T1, SP, right as i32));
    emitter.push(fmv_to_float(wide, 0, T0));
    emitter.push(fmv_to_float(wide, 1, T1));
    match comparison {
        FloatComparison::Eq => emitter.push(float_compare_bit(wide, 2, T0, 0, 1)),
        FloatComparison::Ne => {
            emitter.push(float_compare_bit(wide, 2, T0, 0, 1));
            emitter.push(xori(T0, T0, 1));
        }
        // `a < b` is `flt a, b`; `a > b` is `flt b, a`, so the unordered result stays 0.
        FloatComparison::Lt => emitter.push(float_compare_bit(wide, 1, T0, 0, 1)),
        FloatComparison::Gt => emitter.push(float_compare_bit(wide, 1, T0, 1, 0)),
        FloatComparison::Le => emitter.push(float_compare_bit(wide, 0, T0, 0, 1)),
        FloatComparison::Ge => emitter.push(float_compare_bit(wide, 0, T0, 1, 0)),
    }
    emitter.push(store(3, T0, SP, destination as i32));
}

/// `fcvt.l.d` (`wide`) or `fcvt.l.s`: a float to a 64-bit integer, in the rounding mode `rm`
/// (0 nearest-even, 1 toward zero, 2 down, 3 up).
fn float_to_int64(wide: bool, rm: u32, rd: u32, fs: u32) -> u32 {
    r_type(if wide { 0x61 } else { 0x60 }, 2, fs, rm, rd, 0x53)
}

/// `fcvt.d.l` (`wide`) or `fcvt.s.l`: a 64-bit integer to a float. Exact for the integers that
/// reach it, which are below 2^52 for `f64` and 2^23 for `f32`.
fn int64_to_float(wide: bool, fd: u32, rs: u32) -> u32 {
    r_type(if wide { 0x69 } else { 0x68 }, 2, rs, 0, fd, 0x53)
}

/// `fsgnj`: `fd` gets the magnitude of `fs1` with the sign of `fs2`.
fn float_copy_sign(wide: bool, fd: u32, fs1: u32, fs2: u32) -> u32 {
    r_type(0x10 | u32::from(wide), fs2, fs1, 0, fd, 0x53)
}

/// The rounding functions, which RISC-V has no instruction for. A value of magnitude at least 2^52
/// (2^23 for `f32`), and a NaN, is already integral or is a NaN, and passes through. Any other is
/// converted to a 64-bit integer with the mode, and back, which is exact below the threshold. The
/// sign of the operand is then copied on, so a zero result is `-0` for a negative operand.
fn float_round_into(
    wide: bool,
    operation: FloatUnary,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let rounding = match operation {
        FloatUnary::Nearest => 0,
        FloatUnary::Trunc => 1,
        FloatUnary::Floor => 2,
        _ => 3, // Ceil
    };
    let threshold = if wide {
        0x4330_0000_0000_0000i64 // 2^52
    } else {
        0x4b00_0000i64 // 2^23
    };
    let source = frame.slot(value);
    let destination = frame.slot(result);
    let keep = emitter.new_label();
    emitter.push(load(3, T0, SP, source as i32));
    emitter.push(fmv_to_float(wide, 0, T0)); // f0 = x
                                             // A NaN fails `feq x, x`: keep it, and the canonical NaN step sets it.
    emitter.push(float_compare_bit(wide, 2, T2, 0, 0));
    emitter.branch(branch(0, T2, ZERO), Target::Label(keep), Fixup::Branch13);
    // |x| at or above the threshold: already an integer. `fsgnjx` gives the magnitude.
    emitter.push(float_sign(wide, true, 1, 0));
    load_constant64(T2, threshold, emitter);
    emitter.push(fmv_to_float(wide, 2, T2));
    emitter.push(float_compare_bit(wide, 0, T2, 2, 1)); // fle t2, threshold, |x|
    emitter.branch(branch(1, T2, ZERO), Target::Label(keep), Fixup::Branch13);
    // Convert with the mode and back. Both are exact below the threshold.
    emitter.push(float_to_int64(wide, rounding, T2, 0));
    emitter.push(int64_to_float(wide, 1, T2));
    emitter.push(float_copy_sign(wide, 0, 1, 0)); // f0 = |f1| with the sign of x
    emitter.bind(keep);
    canonicalize_nan(wide, emitter);
    emitter.push(fmv_from_float(wide, T0, 0));
    emitter.push(store(3, T0, SP, destination as i32));
}

/// `fsqrt.s` or, when `wide`, `fsqrt.d`, rounding to nearest even, on `f<fs1>` into `f<fd>`.
fn float_sqrt_op(wide: bool, fd: u32, fs1: u32) -> u32 {
    r_type(0x2C | u32::from(wide), 0, fs1, 0, fd, 0x53)
}

/// `min` or, when `maximum`, `max`, with Micro's rules: a NaN in either operand gives the canonical
/// NaN; equal operands take the OR of their bits for `min` and the AND for `max`, which picks the
/// negative zero for `min` and the positive one for `max`; otherwise `min` takes the smaller and
/// `max` the larger. The comparisons run on `f0` and `f1`, the bits on `t0` and `t1`.
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
    emitter.push(load(3, T0, SP, left as i32));
    emitter.push(load(3, T1, SP, right as i32));
    emitter.push(fmv_to_float(wide, 0, T0));
    emitter.push(fmv_to_float(wide, 1, T1));
    let nan = emitter.new_label();
    let equal = emitter.new_label();
    let less = emitter.new_label();
    let done = emitter.new_label();
    // A NaN makes `feq x, x` zero: branch to the canonical NaN when either operand is one.
    emitter.push(float_compare_bit(wide, 2, T2, 0, 0));
    emitter.branch(branch(0, T2, ZERO), Target::Label(nan), Fixup::Branch13);
    emitter.push(float_compare_bit(wide, 2, T2, 1, 1));
    emitter.branch(branch(0, T2, ZERO), Target::Label(nan), Fixup::Branch13);
    emitter.push(float_compare_bit(wide, 2, T2, 0, 1));
    emitter.branch(branch(1, T2, ZERO), Target::Label(equal), Fixup::Branch13);
    emitter.push(float_compare_bit(wide, 1, T2, 0, 1));
    emitter.branch(branch(1, T2, ZERO), Target::Label(less), Fixup::Branch13);
    // Greater: `min` takes the right operand, `max` keeps the left.
    if !maximum {
        emitter.push(addi(T0, T1, 0));
    }
    emitter.branch(JAL_ZERO, Target::Label(done), Fixup::Jump21);
    emitter.bind(less);
    // Less: `max` takes the right operand, `min` keeps the left.
    if maximum {
        emitter.push(addi(T0, T1, 0));
    }
    emitter.branch(JAL_ZERO, Target::Label(done), Fixup::Jump21);
    emitter.bind(equal);
    emitter.push(op(0, if maximum { 7 } else { 6 }, T0, T0, T1)); // and for max, or for min
    emitter.branch(JAL_ZERO, Target::Label(done), Fixup::Jump21);
    emitter.bind(nan);
    let bits = if wide {
        0x7ff8_0000_0000_0000i64
    } else {
        0x7fc0_0000i64
    };
    load_constant64(T0, bits, emitter);
    emitter.bind(done);
    emitter.push(store(3, T0, SP, destination as i32));
}

/// `copysign`: the left operand's magnitude with the right operand's sign, bit for bit. A 32-bit
/// result is masked to its low half, so its upper half is zero, as the other backends leave it.
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
    let (magnitude, sign) = if wide {
        (i64::MAX, i64::MIN)
    } else {
        (0x7fff_ffff, 0x8000_0000)
    };
    emitter.push(load(3, T0, SP, left as i32));
    emitter.push(load(3, T1, SP, right as i32));
    load_constant64(T2, magnitude, emitter);
    emitter.push(op(0, 7, T0, T0, T2)); // and t0, t0, t2
    load_constant64(T2, sign, emitter);
    emitter.push(op(0, 7, T1, T1, T2)); // and t1, t1, t2
    emitter.push(op(0, 6, T0, T0, T1)); // or t0, t0, t1
    emitter.push(store(3, T0, SP, destination as i32));
}

/// Negation and absolute value, the only unary floats the native code covers. The rest are refused.
fn float_unary_arm(
    wide: bool,
    result: ValueId,
    value: ValueId,
    operation: FloatUnary,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    if matches!(
        operation,
        FloatUnary::Ceil | FloatUnary::Floor | FloatUnary::Trunc | FloatUnary::Nearest
    ) {
        float_round_into(wide, operation, result, value, frame, emitter);
        return Ok(());
    }
    if operation == FloatUnary::Sqrt {
        let source = frame.slot(value);
        let destination = frame.slot(result);
        emitter.push(load(3, T0, SP, source as i32));
        emitter.push(fmv_to_float(wide, 0, T0));
        emitter.push(float_sqrt_op(wide, 0, 0));
        canonicalize_nan(wide, emitter);
        emitter.push(fmv_from_float(wide, T0, 0));
        emitter.push(store(3, T0, SP, destination as i32));
        return Ok(());
    }
    let abs = match operation {
        FloatUnary::Neg => false,
        FloatUnary::Abs => true,
        other => {
            return Err(JitError::Unsupported(format!(
                "the {other:?} float operation"
            )))
        }
    };
    let source = frame.slot(value);
    let destination = frame.slot(result);
    emitter.push(load(3, T0, SP, source as i32));
    emitter.push(fmv_to_float(wide, 0, T0));
    emitter.push(float_sign(wide, abs, 0, 0));
    emitter.push(fmv_from_float(wide, T0, 0));
    emitter.push(store(3, T0, SP, destination as i32));
    Ok(())
}

fn copy(from: ValueId, to: ValueId, frame: &mut Frame, emitter: &mut Emitter) {
    let source = frame.slot(from);
    let destination = frame.slot(to);
    emitter.push(load(3, T0, SP, source as i32));
    emitter.push(store(3, T0, SP, destination as i32));
}

/// Leave the result of comparing `t0` with `t1` in `t0`, as 0 or 1.
///
/// The operands are sign-extended 32-bit values for `i32` and full values for `i64`. In both cases
/// the 64-bit comparisons order them correctly: signed comparisons agree with the signed order, and
/// unsigned comparisons agree with the unsigned order of the sign-extended values.
fn compare(comparison: IntComparison, emitter: &mut Emitter) {
    match comparison {
        IntComparison::Eq => {
            emitter.push(op(0x00, 4, T0, T0, T1)); // xor
            emitter.push(sltiu(T0, T0, 1));
        }
        IntComparison::Ne => {
            emitter.push(op(0x00, 4, T0, T0, T1)); // xor
            emitter.push(op(0x00, 3, T0, ZERO, T0)); // sltu t0, zero, t0
        }
        IntComparison::LtS => emitter.push(op(0x00, 2, T0, T0, T1)), // slt
        IntComparison::GtS => emitter.push(op(0x00, 2, T0, T1, T0)),
        IntComparison::LeS => {
            emitter.push(op(0x00, 2, T0, T1, T0));
            emitter.push(xori(T0, T0, 1));
        }
        IntComparison::GeS => {
            emitter.push(op(0x00, 2, T0, T0, T1));
            emitter.push(xori(T0, T0, 1));
        }
        IntComparison::LtU => emitter.push(op(0x00, 3, T0, T0, T1)), // sltu
        IntComparison::GtU => emitter.push(op(0x00, 3, T0, T1, T0)),
        IntComparison::LeU => {
            emitter.push(op(0x00, 3, T0, T1, T0));
            emitter.push(xori(T0, T0, 1));
        }
        IntComparison::GeU => {
            emitter.push(op(0x00, 3, T0, T0, T1));
            emitter.push(xori(T0, T0, 1));
        }
    }
}

/// `result = left op right` on `t0` and `t1`.
fn binary(
    word: u32,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    emitter.push(load(3, T0, SP, left as i32));
    emitter.push(load(3, T1, SP, right as i32));
    emitter.push(word);
    emitter.push(store(3, T0, SP, destination as i32));
}

/// Division and remainder, signed or unsigned, at 32 or 64 bits, with Wasm's traps.
///
/// A zero divisor traps. For signed division, the minimum value divided by -1 traps, and its
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
    emitter.push(load(3, T0, SP, left as i32));
    emitter.push(load(3, T1, SP, right as i32));

    // A zero divisor is a trap: `bne t1, zero, ok; trap; ok:`. The sign-extended form of a 32-bit
    // zero is zero, so the 64-bit test is right at both widths.
    let nonzero = emitter.new_label();
    emitter.branch(branch(1, T1, ZERO), Target::Label(nonzero), Fixup::Branch13);
    trap(frame, crate::STATUS_DIVIDE_BY_ZERO, emitter);
    emitter.bind(nonzero);

    let done = emitter.new_label();
    let normal = emitter.new_label();
    if signed {
        // t2 = -1. A divisor of -1 is the case that can overflow.
        emitter.push(addi(T2, ZERO, -1));
        emitter.branch(branch(1, T1, T2), Target::Label(normal), Fixup::Branch13); // bne t1, t2
        if remainder {
            // The remainder by -1 is zero.
            emitter.push(addi(T0, ZERO, 0));
            emitter.branch(JAL_ZERO, Target::Label(done), Fixup::Jump21);
        } else {
            // t2 = the minimum value at this width, sign-extended. Its quotient by -1 traps.
            if wide {
                emitter.push(addi(T2, ZERO, 1));
                emitter.push(slli(T2, T2, 63));
            } else {
                emitter.push(lui(T2, 0x80000)); // sign-extends to i32::MIN
            }
            emitter.branch(branch(1, T0, T2), Target::Label(normal), Fixup::Branch13); // bne t0, t2
            trap(frame, crate::STATUS_OVERFLOW, emitter);
        }
    }
    emitter.bind(normal);
    let word = match (signed, remainder, wide) {
        (true, false, true) => op(0x01, 4, T0, T0, T1), // div
        (true, true, true) => op(0x01, 6, T0, T0, T1),  // rem
        (false, false, true) => op(0x01, 5, T0, T0, T1), // divu
        (false, true, true) => op(0x01, 7, T0, T0, T1), // remu
        (true, false, false) => op_w(0x01, 4, T0, T0, T1), // divw
        (true, true, false) => op_w(0x01, 6, T0, T0, T1), // remw
        (false, false, false) => op_w(0x01, 5, T0, T0, T1), // divuw
        (false, true, false) => op_w(0x01, 7, T0, T0, T1), // remuw
    };
    emitter.push(word);
    emitter.bind(done);
    emitter.push(store(3, T0, SP, destination as i32));
}

/// Compute the address of an access of `width` bytes into `t3`, after checking it against the
/// memory and loading the memory base.
///
/// The effective address is the 32-bit operand plus the offset, computed in 64 bits, so it cannot
/// wrap. The check is `address + width <= memory_len`, read from the context.
fn emit_address(
    address: ValueId,
    offset: u32,
    width: u32,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    if offset > MAX_OFFSET {
        return Err(JitError::Unsupported(
            "a memory offset too large for the RISC-V encoder".into(),
        ));
    }
    let address = frame.slot(address);
    emitter.push(load(3, T0, SP, address as i32));
    // Zero-extend the 32-bit operand: the slot holds it sign-extended.
    emitter.push(slli(T0, T0, 32));
    emitter.push(srli(T0, T0, 32));
    load_constant32(T1, offset as i32, emitter);
    emitter.push(op(0x00, 0, T0, T0, T1)); // add t0, t0, t1: the effective address
    emitter.push(addi(T2, T0, width as i32)); // the end of the access
    emitter.push(load(3, T3, SP, frame.context as i32)); // the context
    emitter.push(load(3, T4, T3, MEMORY_LENGTH_OFFSET)); // the memory length
    let inside = emitter.new_label();
    // bgeu t4, t2, inside: the access ends at or before the end of the memory.
    emitter.branch(branch(7, T4, T2), Target::Label(inside), Fixup::Branch13);
    trap(frame, crate::STATUS_MEMORY_OUT_OF_BOUNDS, emitter);
    emitter.bind(inside);
    emitter.push(load(3, T3, T3, MEMORY_BASE_OFFSET)); // the memory base
    emitter.push(op(0x00, 0, T3, T3, T0)); // add t3, t3, t0: the address of the access
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_offsets_match_the_context_layout() {
        assert_eq!(MEMORY_BASE_OFFSET, 0);
        assert_eq!(MEMORY_LENGTH_OFFSET, 8);
        assert_eq!(STATUS_OFFSET, 16);
    }

    #[test]
    fn a_constant_loads_with_addi_when_it_fits_and_lui_addiw_otherwise() {
        let mut emitter = Emitter::new();
        load_constant32(T0, 7, &mut emitter);
        assert_eq!(emitter.words, vec![0x0070_0293]); // addi t0, zero, 7
        let mut emitter = Emitter::new();
        load_constant32(T0, 0x1234_5678, &mut emitter);
        assert_eq!(emitter.words.len(), 2);
    }

    #[test]
    fn a_return_ends_with_the_return_instruction() {
        use tpt_wasm_ir::IrValue;
        use tpt_wasm_types::{FunctionType, ResultType, ValueType};
        let function = IrFunction {
            function_type: FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            },
            params: Vec::new(),
            locals: Vec::new(),
            values: vec![IrValue {
                id: ValueId(0),
                value_type: ValueType::I32,
            }],
            entry: BlockId(0),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instrs: vec![IrInstr::ConstI32 {
                    result: ValueId(0),
                    value: 7,
                }],
                terminator: Terminator::Return(vec![ValueId(0)]),
            }],
        };
        let code = encode(&function, &CallEnv::default()).expect("compiles");
        let last = u32::from_le_bytes(code[code.len() - 4..].try_into().unwrap());
        assert_eq!(last, RET, "jalr x0, 0(ra)");
    }
}
