// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The AArch64 encoder for the compiled subset. Safe code, tested on every target.
//!
//! The same design as the x86-64 encoder: every IR value has an 8-byte slot at a fixed offset
//! from `sp`. An `i32` uses the low four bytes and an `i64` all eight, and every copy between
//! slots moves eight bytes. Control flow is the same graph walk, with fixups patched once the
//! offset of each block is known.
//!
//! The calling convention is AAPCS64: the arguments pointer arrives in `x0` and the context
//! pointer in `x1`. The result is returned in `x0`, whole for an `i64` and zero-extended for an
//! `i32`. Trap statuses are stored in the context. Scratch registers are `x9` to `x13`, and
//! `x29` is the frame pointer, set from `sp` on entry. Every epilogue restores `sp` from `x29`, so
//! a trap sequence does not need the final frame size.

use std::collections::{HashMap, HashSet, VecDeque};

use tpt_wasm_ir::{
    BasicBlock, BlockId, FloatComparison, FloatConversion, FloatTrunc, FloatUnary, IntComparison,
    IntConversion, IntUnary, IrFunction, IrInstr, MemoryLoad, MemoryStore, Reinterpret, SignExtend,
    Terminator, ValueId,
};
use tpt_wasm_types::ValueType;

use crate::x86::{check_supported, instruction_name, terminator_name};
use crate::{
    allocate_registers, call_result, is_register_instruction, makes_calls, CallEnv, Context,
    JitError, STATUS_CALL_DEPTH, STATUS_GLOBAL_OUT_OF_BOUNDS, STATUS_INDIRECT_TYPE_MISMATCH,
    STATUS_INVALID_CONVERSION, STATUS_NULL_REFERENCE, STATUS_TABLE_OUT_OF_BOUNDS,
    STATUS_UNREACHABLE,
};

/// Scratch registers. `9` holds the first operand, `10` the second, `11` a temporary, and `12`
/// and `13` the memory base and length. The numbers serve as both `w` and `x` registers.
const R9: u32 = 9;
const R10: u32 = 10;
const R11: u32 = 11;
const R12: u32 = 12;
const R13: u32 = 13;
const FRAME_POINTER: u32 = 29;
const SP: u32 = 31;

/// Condition codes for the conditional forms.
const EQ: u32 = 0;
const NE: u32 = 1;
const LS: u32 = 9;
/// The `hi` condition: unsigned greater than.
const HI: u32 = 8;

/// The largest frame the encoder emits.
const MAX_FRAME: u32 = 4096;

/// The byte offsets of the context fields the generated code uses.
const MEMORY_BASE_OFFSET: u32 = std::mem::offset_of!(Context, memory) as u32;
const MEMORY_LENGTH_OFFSET: u32 = std::mem::offset_of!(Context, memory_len) as u32;
const STATUS_OFFSET: u32 = std::mem::offset_of!(Context, status) as u32;
const HOST_CALL_OFFSET: u32 = std::mem::offset_of!(Context, host_call) as u32;
const FUNCTIONS_OFFSET: u32 = std::mem::offset_of!(Context, functions) as u32;
const DEPTH_OFFSET: u32 = std::mem::offset_of!(Context, depth) as u32;
const DEPTH_LIMIT_OFFSET: u32 = std::mem::offset_of!(Context, depth_limit) as u32;
const TABLE_ENTRIES_OFFSET: u32 = std::mem::offset_of!(Context, table_entries) as u32;
const TABLE_TYPES_OFFSET: u32 = std::mem::offset_of!(Context, table_types) as u32;
const TABLE_LEN_OFFSET: u32 = std::mem::offset_of!(Context, table_len) as u32;
const GLOBALS_OFFSET: u32 = std::mem::offset_of!(Context, globals) as u32;
const GLOBALS_LENGTH_OFFSET: u32 = std::mem::offset_of!(Context, globals_len) as u32;
const MEMORY_CAPACITY_OFFSET: u32 = std::mem::offset_of!(Context, memory_capacity) as u32;
/// The `lo` condition: unsigned less than, the test that keeps a call within the depth limit.
const LO: u32 = 3;

/// A place a branch can go: a block of the function, or a label inside the emitted code.
#[derive(Clone, Copy, Debug)]
enum Target {
    Block(BlockId),
    Label(usize),
}

/// The branch encodings whose immediates are patched: `B` takes 26 bits, the compare-and-branch
/// and conditional forms take 19.
#[derive(Clone, Copy, Debug)]
enum Fixup {
    Branch26,
    Branch19,
}

/// The stack slots of a function, as offsets from `sp`, and the type of each value.
struct Frame {
    slots: HashMap<ValueId, u32>,
    types: HashMap<ValueId, ValueType>,
    used: u32,
    context: u32,
    /// What the function's calls may name, and the table they may index.
    env: CallEnv,
    /// The registers of the values that live in one, in a function that makes no call.
    registers: HashMap<ValueId, u8>,
}

impl Frame {
    fn new(function: &IrFunction, env: &CallEnv) -> Self {
        Self {
            env: env.clone(),
            registers: HashMap::new(),
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

    /// The frame size: the slots, rounded up to 16 as AAPCS64 requires.
    fn size(&self) -> u32 {
        self.used.div_ceil(16) * 16
    }

    /// Whether the slot is read and written at 64 bits: `i64`, and `f64`, whose bits fill the slot.
    fn is_wide(&self, value: ValueId) -> bool {
        matches!(
            self.types.get(&value),
            Some(ValueType::I64 | ValueType::F64)
        )
    }
}

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

    /// A branch whose immediate is patched later. `opcode` is the instruction with a zero
    /// immediate.
    fn branch(&mut self, opcode: u32, target: Target, fixup: Fixup) {
        self.fixups.push((self.words.len(), target, fixup));
        self.push(opcode);
    }

    /// Patch every branch now that the word offset of each block is known.
    fn resolve(&mut self, blocks: &HashMap<BlockId, usize>) -> Result<(), JitError> {
        for (position, target, fixup) in std::mem::take(&mut self.fixups) {
            let destination = match target {
                Target::Block(block) => *blocks.get(&block).ok_or_else(|| {
                    JitError::Unsupported("a branch to a block that was not emitted".into())
                })?,
                Target::Label(label) => self.labels[label]
                    .ok_or_else(|| JitError::Unsupported("a branch to an unbound label".into()))?,
            };
            let distance = destination as i64 - position as i64;
            match fixup {
                Fixup::Branch26 => {
                    if !(-(1 << 25)..(1 << 25)).contains(&distance) {
                        return Err(JitError::Unsupported("a branch beyond 128 MiB".into()));
                    }
                    self.words[position] |= (distance as u32) & 0x03FF_FFFF;
                }
                Fixup::Branch19 => {
                    if !(-(1 << 18)..(1 << 18)).contains(&distance) {
                        return Err(JitError::Unsupported(
                            "a conditional branch beyond 1 MiB".into(),
                        ));
                    }
                    self.words[position] |= ((distance as u32) & 0x7_FFFF) << 5;
                }
            }
        }
        Ok(())
    }
}

// ---- instruction encodings. Each returns one 32-bit word. The `x` forms set the sf bit.

/// `ldr w<rt>, [x<rn>, #imm]`, with `imm` a multiple of 4.
fn ldr_w(rt: u32, rn: u32, imm: u32) -> u32 {
    0xB940_0000 | ((imm / 4) << 10) | (rn << 5) | rt
}

/// `ldr x<rt>, [x<rn>, #imm]`, with `imm` a multiple of 8.
fn ldr_x(rt: u32, rn: u32, imm: u32) -> u32 {
    0xF940_0000 | ((imm / 8) << 10) | (rn << 5) | rt
}

/// `str x<rt>, [x<rn>, #imm]`.
fn str_x(rt: u32, rn: u32, imm: u32) -> u32 {
    0xF900_0000 | ((imm / 8) << 10) | (rn << 5) | rt
}

/// `movz w<rd>, #imm16`.
fn movz_w(rd: u32, imm16: u32) -> u32 {
    0x5280_0000 | (imm16 << 5) | rd
}

/// `movk w<rd>, #imm16, lsl #16`.
fn movk_w_high(rd: u32, imm16: u32) -> u32 {
    0x72A0_0000 | (imm16 << 5) | rd
}

/// `movz w<rd>, #imm16, lsl #16`.
fn movz_w_high(rd: u32, imm16: u32) -> u32 {
    0x52A0_0000 | (imm16 << 5) | rd
}

/// `movz x<rd>, #imm16, lsl #(16 * hw)`.
fn movz_x(rd: u32, imm16: u32, hw: u32) -> u32 {
    0xD280_0000 | (hw << 21) | (imm16 << 5) | rd
}

/// `movk x<rd>, #imm16, lsl #(16 * hw)`.
fn movk_x(rd: u32, imm16: u32, hw: u32) -> u32 {
    0xF280_0000 | (hw << 21) | (imm16 << 5) | rd
}

/// `add x<rd>, x<rn>, x<rm>`.
fn add_x(rd: u32, rn: u32, rm: u32) -> u32 {
    0x8B00_0000 | (rm << 16) | (rn << 5) | rd
}

/// `add x<rd>, x<rn>, #imm12`.
fn add_x_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
    0x9100_0000 | (imm12 << 10) | (rn << 5) | rd
}

/// `add sp, x<rn>, #0`, which is `mov sp, x<rn>`.
fn mov_to_sp(rn: u32) -> u32 {
    add_x_imm(SP, rn, 0)
}

/// `add x<rd>, sp, #0`, which is `mov x<rd>, sp`.
fn mov_from_sp(rd: u32) -> u32 {
    add_x_imm(rd, SP, 0)
}

/// A register-register data-processing operation: `base` is the 32-bit encoding, and the `x`
/// form sets the sf bit.
fn alu(wide: bool, base: u32, rd: u32, rn: u32, rm: u32) -> u32 {
    let base = if wide { base | 0x8000_0000 } else { base };
    base | (rm << 16) | (rn << 5) | rd
}

const ADD: u32 = 0x0B00_0000;
const SUB: u32 = 0x4B00_0000;
const AND: u32 = 0x0A00_0000;
const ORR: u32 = 0x2A00_0000;
const EOR: u32 = 0x4A00_0000;
const LSLV: u32 = 0x1AC0_2000;
const LSRV: u32 = 0x1AC0_2400;
const ASRV: u32 = 0x1AC0_2800;
const UDIV: u32 = 0x1AC0_0800;
const SDIV: u32 = 0x1AC0_0C00;

/// `mul <rd>, <rn>, <rm>`, which is `madd` with the zero register as the addend.
fn mul(wide: bool, rd: u32, rn: u32, rm: u32) -> u32 {
    let base = if wide { 0x9B00_7C00 } else { 0x1B00_7C00 };
    base | (rm << 16) | (rn << 5) | rd
}

/// `msub <rd>, <rn>, <rm>, <ra>`, which computes `ra - rn * rm`.
fn msub(wide: bool, rd: u32, rn: u32, rm: u32, ra: u32) -> u32 {
    let base = if wide { 0x9B00_8000 } else { 0x1B00_8000 };
    base | (rm << 16) | (ra << 10) | (rn << 5) | rd
}

/// `cmp <rn>, <rm>`.
fn cmp(wide: bool, rn: u32, rm: u32) -> u32 {
    let base = if wide { 0xEB00_001F } else { 0x6B00_001F };
    base | (rm << 16) | (rn << 5)
}

/// `cmp <rn>, #imm12`.
fn cmp_imm(wide: bool, rn: u32, imm12: u32) -> u32 {
    let base = if wide { 0xF100_001F } else { 0x7100_001F };
    base | (imm12 << 10) | (rn << 5)
}

/// `cmn <rn>, #imm12`, which compares `rn` with `-imm12`.
fn cmn_imm(wide: bool, rn: u32, imm12: u32) -> u32 {
    let base = if wide { 0xB100_001F } else { 0x3100_001F };
    base | (imm12 << 10) | (rn << 5)
}

/// `cset <rd>, <cond>`, which is `csinc <rd>, zr, zr, <the inverse of cond>`.
fn cset(wide: bool, rd: u32, cond: u32) -> u32 {
    let base = if wide { 0x9A9F_07E0 } else { 0x1A9F_07E0 };
    base | ((cond ^ 1) << 12) | rd
}

/// `csel <rd>, <rn>, <rm>, <cond>`: `rd = cond ? rn : rm`.
fn csel(wide: bool, rd: u32, rn: u32, rm: u32, cond: u32) -> u32 {
    let base = if wide { 0x9A80_0000 } else { 0x1A80_0000 };
    base | (rm << 16) | (cond << 12) | (rn << 5) | rd
}

/// A register-indexed memory operation: `base` has the index and base fields clear; `rt` is the
/// data register, `rn` the base, `rm` the index.
fn indexed(base: u32, rt: u32, rn: u32, rm: u32) -> u32 {
    base | (rm << 16) | (rn << 5) | rt
}

/// The condition of an integer comparison, as the `cond` field of the conditional forms.
fn condition(comparison: IntComparison) -> u32 {
    match comparison {
        IntComparison::Eq => 0,
        IntComparison::Ne => 1,
        IntComparison::GeU => 2,
        IntComparison::LtU => 3,
        IntComparison::GtU => 8,
        IntComparison::LeU => 9,
        IntComparison::GeS => 10,
        IntComparison::LtS => 11,
        IntComparison::GtS => 12,
        IntComparison::LeS => 13,
    }
}

/// `cbz <rt>` and `cbnz <rt>` (the `x` forms set the sf bit), with a zero immediate.
fn cbz(wide: bool, rt: u32) -> u32 {
    (if wide { 0xB400_0000 } else { 0x3400_0000 }) | rt
}

fn cbnz(wide: bool, rt: u32) -> u32 {
    (if wide { 0xB500_0000 } else { 0x3500_0000 }) | rt
}

/// `b.<cond>`, with a zero immediate to be patched.
fn b_cond(cond: u32) -> u32 {
    0x5400_0000 | cond
}

/// `b`, with a zero immediate to be patched.
const B: u32 = 0x1400_0000;

// The register-indexed loads and stores. Each is one access of a given width and extension.
const LOAD_W: u32 = 0xB860_6800; // ldr w
const LOAD_X: u32 = 0xF860_6800; // ldr x
const LOAD_BYTE_W: u32 = 0x3860_6800; // ldrb
const LOAD_BYTE_SIGNED_W: u32 = 0x38E0_6800; // ldrsb to w
const LOAD_BYTE_SIGNED_X: u32 = 0x38A0_6800; // ldrsb to x
const LOAD_HALF_W: u32 = 0x7860_6800; // ldrh
const LOAD_HALF_SIGNED_W: u32 = 0x78E0_6800; // ldrsh to w
const LOAD_HALF_SIGNED_X: u32 = 0x78A0_6800; // ldrsh to x
const LOAD_WORD_SIGNED_X: u32 = 0xB8A0_6800; // ldrsw to x
const STORE_W: u32 = 0xB820_6800;
const STORE_X: u32 = 0xF820_6800;
const STORE_BYTE: u32 = 0x3820_6800;
const STORE_HALF: u32 = 0x7820_6800;

/// `movz x<rd>, #status, lsl #32`-free status store helper: a status is an immediate of at most
/// a few bits, so `movz w` suffices.
fn movz_status(rd: u32, status: u32) -> u32 {
    movz_w(rd, status)
}

// ---- the encoder.

/// Compile `function` to AArch64 machine code.
pub(crate) fn encode(function: &IrFunction, env: &CallEnv) -> Result<Vec<u8>, JitError> {
    check_supported(function)?;
    let blocks: HashMap<BlockId, &BasicBlock> = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();

    let mut frame = Frame::new(function, env);
    if !makes_calls(function) {
        frame.registers =
            allocate_registers(function, &VALUE_REGISTERS, false, is_register_instruction);
    }
    let mut emitter = Emitter::new();

    // The two pointers arrive in x0 and x1, and are kept in slots, because the code below uses
    // every scratch register. Each argument is an 8-byte slot in the arguments array.
    let args = frame.temporary();
    frame.context = frame.temporary();
    emitter.push(str_x(0, SP, args));
    emitter.push(str_x(1, SP, frame.context));
    for (index, parameter) in function.params.iter().enumerate() {
        emitter.push(ldr_x(R9, SP, args));
        emitter.push(ldr_x(R9, R9, 8 * index as u32));
        let slot = frame.slot(*parameter);
        emitter.push(str_x(R9, SP, slot));
    }
    let parameters: HashSet<ValueId> = function.params.iter().copied().collect();
    for local in &function.locals {
        if !parameters.contains(local) {
            let slot = frame.slot(*local);
            emitter.push(str_x(31, SP, slot)); // xzr: zero
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
    if size >= MAX_FRAME {
        return Err(JitError::Unsupported(format!(
            "a frame of {size} bytes; the largest emitted is {}",
            MAX_FRAME - 1
        )));
    }
    // The prologue: stp x29, x30, [sp, #-16]!; mov x29, sp; sub sp, sp, #size. It is written in
    // front of the body, which uses only sp-relative offsets.
    let mut words = vec![
        0xA9BF_7BFD,
        mov_from_sp(FRAME_POINTER),
        0xD100_03FF | (size << 10),
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
            if frame.is_wide(values[0]) {
                emitter.push(ldr_x(0, SP, slot));
            } else {
                emitter.push(ldr_w(0, SP, slot)); // zero-extends into x0
            }
            epilogue(emitter);
            Ok(Vec::new())
        }
        Terminator::Return(values) if values.is_empty() => {
            emitter.push(movz_w(0, 0)); // a function with no result returns zero
            epilogue(emitter);
            Ok(Vec::new())
        }
        Terminator::Unreachable => {
            trap(frame, STATUS_UNREACHABLE, emitter);
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
            emitter.branch(B, Target::Block(*target), Fixup::Branch26);
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
            emitter.push(ldr_w(R9, SP, slot));
            let else_path = emitter.new_label();
            // A zero condition takes the else edge.
            emitter.branch(cbz(false, R9), Target::Label(else_path), Fixup::Branch19);
            copy_into_parameters(&then_block.params, then_values, frame, emitter);
            emitter.branch(B, Target::Block(*then_target), Fixup::Branch26);
            emitter.bind(else_path);
            copy_into_parameters(&else_block.params, else_values, frame, emitter);
            emitter.branch(B, Target::Block(*else_target), Fixup::Branch26);
            Ok(vec![*then_target, *else_target])
        }
        other => Err(JitError::Unsupported(format!(
            "the {} terminator",
            terminator_name(other)
        ))),
    }
}

/// `add sp, x29, #0; ldp x29, x30, [sp], #16; ret`. Independent of the frame size.
fn epilogue(emitter: &mut Emitter) {
    emitter.push(mov_to_sp(FRAME_POINTER));
    emitter.push(0xA8C1_7BFD); // ldp x29, x30, [sp], #16
    emitter.push(0xD65F_03C0); // ret
}

/// Store `status` in the context and return zero.
fn trap(frame: &Frame, status: u32, emitter: &mut Emitter) {
    emitter.push(ldr_x(R12, SP, frame.context)); // the context
    emitter.push(movz_status(R13, status));
    emitter.push(str_x(R13, R12, STATUS_OFFSET));
    emitter.push(movz_w(0, 0));
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

/// Copy branch values into the target's parameter slots, through temporaries so the order of the
/// copies cannot change what a value means. Each copy is eight bytes.
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
            emitter.push(ldr_x(R9, SP, source));
            emitter.push(str_x(R9, SP, temporary));
            temporary
        })
        .collect();
    for (parameter, temporary) in parameters.iter().zip(temporaries) {
        let destination = frame.slot(*parameter);
        emitter.push(ldr_x(R9, SP, temporary));
        emitter.push(str_x(R9, SP, destination));
    }
}

/// `mov x<rd>, x<rm>` (the `orr` with `xzr`).
fn mov_x(rd: u32, rm: u32) -> u32 {
    0xAA00_03E0 | (rm << 16) | rd
}

/// `sub x<rd>, x<rn>, #imm12`.
fn sub_x_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
    0xD100_0000 | (imm12 << 10) | (rn << 5) | rd
}

/// `blr x<rn>`: call through a register, with the return address in `x30`.
fn blr(rn: u32) -> u32 {
    0xD63F_0000 | (rn << 5)
}

/// A direct call to another function of the module, through the call table in the context.
///
/// The arguments are copied to a block of slots in this frame, and `x0` points at the block. The
/// context is passed in `x1`. The call counts a frame against the depth limit first, and a call
/// past the limit traps. A trap in the callee leaves its status in the context and returns zero,
/// so the status is checked after the call, and a non-zero status returns zero from here too.
/// `results = host import(arguments)`: the arguments go to a block of the frame, the host is called through the
/// context with the context, the import, the argument block and the result block in x0 to x3, and the results are
/// read from a block the host fills. A failed host call leaves a status in the context, and the function returns
/// with it.
fn emit_host_call(
    import: u32,
    arguments: &[ValueId],
    results: &[ValueId],
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    let arguments_block = frame.block(arguments.len().max(1));
    let results_block = frame.block(results.len().max(1));
    store_arguments(arguments, arguments_block, frame, emitter);
    emitter.push(ldr_x(R9, SP, frame.context)); // x9 = the context
    emitter.push(mov_x(0, R9)); // x0 = the context
    emitter.push(movz_w(1, import & 0xFFFF)); // x1 = the import
    if import >> 16 != 0 {
        emitter.push(movk_w_high(1, import >> 16));
    }
    emitter.push(add_x_imm(2, SP, arguments_block)); // x2 = the argument block
    emitter.push(add_x_imm(3, SP, results_block)); // x3 = the result block
    emitter.push(ldr_x(R12, R9, HOST_CALL_OFFSET));
    emitter.push(blr(R12));
    // The context is reloaded, since the call may have used x9. A status left behind returns with zero.
    emitter.push(ldr_x(R9, SP, frame.context));
    emitter.push(ldr_x(R10, R9, STATUS_OFFSET));
    let propagate = emitter.words.len();
    emitter.push(0); // cbz past the return below, patched below
    emitter.push(movz_w(0, 0));
    epilogue(emitter);
    let skip = (emitter.words.len() - propagate) as u32;
    emitter.words[propagate] = cbz(true, R10) | (skip << 5);
    for (index, result) in results.iter().enumerate() {
        let destination = frame.slot(*result);
        emitter.push(ldr_x(R9, SP, results_block + 8 * index as u32));
        emitter.push(str_x(R9, SP, destination));
    }
    Ok(())
}

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
    // The table entry is loaded with an unsigned 12-bit offset, scaled by eight.
    let entry = 8 * function;
    if entry > 4095 * 8 {
        return Err(JitError::Unsupported(
            "a call to a function beyond the reach of the call table".into(),
        ));
    }
    let block = frame.block(arguments.len().max(1));
    store_arguments(arguments, block, frame, emitter);
    let destination = match result {
        Some(result) => frame.slot(result),
        None => frame.temporary(),
    };
    enter_callee(frame, emitter);
    // x0 = the argument block; x1 = the context; x12 = the callee's entry.
    emitter.push(add_x_imm(0, SP, block));
    emitter.push(mov_x(1, R9));
    emitter.push(ldr_x(R12, R9, FUNCTIONS_OFFSET));
    emitter.push(ldr_x(R12, R12, entry));
    emitter.push(blr(R12));
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

    // x11 = the index, from its low 32 bits; x9 = the context.
    emitter.push(ldr_w(R11, SP, operand_slot));
    emitter.push(ldr_x(R9, SP, context));
    // Out of range traps: cmp x11, [len]; b.lo ok.
    emitter.push(ldr_x(R10, R9, TABLE_LEN_OFFSET));
    emitter.push(cmp(true, R11, R10));
    guard_trap(frame, LO, STATUS_TABLE_OUT_OF_BOUNDS, emitter);
    // x10 = the slot's type, `-1` for a null slot.
    emitter.push(ldr_x(R12, R9, TABLE_TYPES_OFFSET));
    emitter.push(indexed_scaled(LOAD_X, R10, R12, R11));
    emitter.push(cmn_imm(true, R10, 1));
    guard_trap(frame, NE, STATUS_NULL_REFERENCE, emitter);
    // The slot's type against the call's: b.eq ok, else trap.
    emit_constant(expected, R13, emitter);
    emitter.push(cmp(true, R10, R13));
    guard_trap(frame, EQ, STATUS_INDIRECT_TYPE_MISMATCH, emitter);
    // x12 = the slot's entry, kept in a slot for the call.
    emitter.push(ldr_x(R12, R9, TABLE_ENTRIES_OFFSET));
    emitter.push(indexed_scaled(LOAD_X, R12, R12, R11));
    emitter.push(str_x(R12, SP, entry));

    enter_callee(frame, emitter);
    emitter.push(add_x_imm(0, SP, block));
    emitter.push(mov_x(1, R9));
    emitter.push(ldr_x(R12, SP, entry));
    emitter.push(blr(R12));
    leave_callee(frame, destination, emitter);
    Ok(())
}

/// Copy each argument into the block that the callee reads as its argument array.
fn store_arguments(arguments: &[ValueId], block: u32, frame: &mut Frame, emitter: &mut Emitter) {
    for (index, argument) in arguments.iter().enumerate() {
        let source = frame.slot(*argument);
        emitter.push(ldr_x(R9, SP, source));
        emitter.push(str_x(R9, SP, block + 8 * index as u32));
    }
}

/// Count one more active frame against the depth limit, and trap if that is past it. Leaves the
/// context in `x9`.
fn enter_callee(frame: &Frame, emitter: &mut Emitter) {
    // x9 = context; x10 = depth; x11 = limit; trap unless depth < limit.
    emitter.push(ldr_x(R9, SP, frame.context));
    emitter.push(ldr_x(R10, R9, DEPTH_OFFSET));
    emitter.push(ldr_x(R11, R9, DEPTH_LIMIT_OFFSET));
    emitter.push(cmp(true, R10, R11));
    guard_trap(frame, LO, STATUS_CALL_DEPTH, emitter);
    // depth += 1
    emitter.push(add_x_imm(R10, R10, 1));
    emitter.push(str_x(R10, R9, DEPTH_OFFSET));
}

/// After the callee returns: count the frame off, and if the callee left a status behind, return
/// zero without storing anything. Otherwise store the callee's result, which is in `x0`.
fn leave_callee(frame: &mut Frame, destination: u32, emitter: &mut Emitter) {
    // depth -= 1, then return zero if the callee left a status behind.
    emitter.push(ldr_x(R9, SP, frame.context));
    emitter.push(ldr_x(R10, R9, DEPTH_OFFSET));
    emitter.push(sub_x_imm(R10, R10, 1));
    emitter.push(str_x(R10, R9, DEPTH_OFFSET));
    emitter.push(ldr_x(R10, R9, STATUS_OFFSET));
    let propagate = emitter.words.len();
    emitter.push(0); // cbz past the return below, patched below
    emitter.push(movz_w(0, 0));
    epilogue(emitter);
    let skip = (emitter.words.len() - propagate) as u32;
    emitter.words[propagate] = cbz(true, R10) | (skip << 5);
    emitter.push(str_x(0, SP, destination));
}

/// A conditional branch that skips a trap with `status` when `condition` holds. The trap is emitted
/// in place, after the branch.
fn guard_trap(frame: &Frame, condition: u32, status: u32, emitter: &mut Emitter) {
    let at = emitter.words.len();
    emitter.push(0); // patched below
    trap(frame, status, emitter);
    let distance = (emitter.words.len() - at) as u32;
    emitter.words[at] = b_cond(condition) | (distance << 5);
}

/// `ldr x<rt>, [x<rn>, x<rm>, lsl #3]`: the base with the scaled-index bit set.
fn indexed_scaled(base: u32, rt: u32, rn: u32, rm: u32) -> u32 {
    base | 0x1000 | (rm << 16) | (rn << 5) | rt
}

/// `clz` of `rn` into `rd`, on 32 or 64 bits.
fn count_leading_zeros(wide: bool, rd: u32, rn: u32) -> u32 {
    let base = if wide { 0xDAC0_1000 } else { 0x5AC0_1000 };
    base | (rn << 5) | rd
}

/// A logical shift right by an immediate, on 64 bits: `lsr rd, rn, #shift`.
fn lsr_immediate(rd: u32, rn: u32, shift: u32) -> u32 {
    0xD340_FC00 | (shift << 16) | (rn << 5) | rd
}

/// `rorv rd, rn, rm`: a rotate right by a register, which takes the count modulo the width.
fn rotate_right(wide: bool, rd: u32, rn: u32, rm: u32) -> u32 {
    let base = if wide { 0x9AC0_2C00 } else { 0x1AC0_2C00 };
    base | (rm << 16) | (rn << 5) | rd
}

/// `result = clz(value)`, `ctz(value)` or `popcnt(value)` on 32 or 64 bits.
///
/// `clz` is an instruction, and `ctz` is a bit reversal followed by `clz`. `popcnt` is computed in
/// software, as on the other targets. It works on 64 bits throughout, and a 32-bit value is loaded
/// zero-extended, so its count is the same.
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
    if wide {
        emitter.push(ldr_x(R9, SP, source));
    } else {
        emitter.push(ldr_w(R9, SP, source)); // zero-extends into x9
    }
    match operation {
        IntUnary::Clz => emitter.push(count_leading_zeros(wide, R9, R9)),
        IntUnary::Ctz => {
            // rbit reverses the bits, so the trailing zeros become leading ones.
            let base = if wide { 0xDAC0_0000 } else { 0x5AC0_0000 };
            emitter.push(base | (R9 << 5) | R9);
            emitter.push(count_leading_zeros(wide, R9, R9));
        }
        IntUnary::Popcnt => {
            // x -= (x >> 1) & 0x5555...
            emitter.push(lsr_immediate(R10, R9, 1));
            emit_constant(0x5555_5555_5555_5555, R11, emitter);
            emitter.push(alu(true, AND, R10, R10, R11));
            emitter.push(alu(true, SUB, R9, R9, R10));
            // x = (x & 0x3333...) + ((x >> 2) & 0x3333...)
            emit_constant(0x3333_3333_3333_3333, R11, emitter);
            emitter.push(alu(true, AND, R10, R9, R11));
            emitter.push(lsr_immediate(R9, R9, 2));
            emitter.push(alu(true, AND, R9, R9, R11));
            emitter.push(alu(true, ADD, R9, R9, R10));
            // x = (x + (x >> 4)) & 0x0F0F...
            emitter.push(lsr_immediate(R10, R9, 4));
            emitter.push(alu(true, ADD, R9, R9, R10));
            emit_constant(0x0F0F_0F0F_0F0F_0F0F, R11, emitter);
            emitter.push(alu(true, AND, R9, R9, R11));
            // x * 0x0101... >> 56: the byte sums collect in the top byte.
            emit_constant(0x0101_0101_0101_0101, R11, emitter);
            emitter.push(mul(true, R9, R9, R11));
            emitter.push(lsr_immediate(R9, R9, 56));
        }
    }
    emitter.push(str_x(R9, SP, destination));
}

/// `result = left rotl count` or `left rotr count` on 32 or 64 bits. A rotate left is a rotate right
/// by the negated count, which `rorv` takes modulo the width.
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
        emitter.push(ldr_x(R9, SP, source));
    } else {
        emitter.push(ldr_w(R9, SP, source));
    }
    emitter.push(ldr_w(R10, SP, count));
    if rotate_left {
        // r10 = 0 - count, with register 31 as the zero register.
        emitter.push(alu(wide, SUB, R10, 31, R10));
    }
    emitter.push(rotate_right(wide, R9, R9, R10));
    emitter.push(str_x(R9, SP, destination));
}

/// `result = memory.grow(delta)` in place. The new size is the old size plus `delta` pages; it fails,
/// with -1, when it is past the reservation, which is the memory's maximum. A success stores the new
/// size in the context, where the caller reads it back, and returns the old size in pages.
fn memory_grow(result: ValueId, delta: ValueId, frame: &mut Frame, emitter: &mut Emitter) {
    let delta_slot = frame.slot(delta);
    let destination = frame.slot(result);
    emitter.push(ldr_x(R12, SP, frame.context)); // the context
    emitter.push(ldr_x(R13, R12, MEMORY_LENGTH_OFFSET)); // the old size in bytes
    emitter.push(ldr_w(R9, SP, delta_slot)); // the pages to add, zero-extended
    emitter.push(0xD340_0000 | (48 << 16) | (47 << 10) | (R9 << 5) | R9); // lsl x9, x9, #16
    emitter.push(alu(true, ADD, R9, R9, R13)); // x9 = the new size
    emitter.push(ldr_x(R11, R12, MEMORY_CAPACITY_OFFSET));
    emitter.push(cmp(true, R9, R11)); // the new size against the reservation
    let fail = emitter.new_label();
    emitter.branch(b_cond(HI), Target::Label(fail), Fixup::Branch19);
    emitter.push(str_x(R9, R12, MEMORY_LENGTH_OFFSET)); // the new size
    emitter.push(lsr_immediate(R10, R13, 16)); // the old size in pages
    let done = emitter.new_label();
    emitter.branch(B, Target::Label(done), Fixup::Branch26);
    emitter.bind(fail);
    emit_constant(u64::MAX, R10, emitter); // -1
    emitter.bind(done);
    emitter.push(str_x(R10, SP, destination));
}

/// The registers that hold values in a function that makes no call: x14 to x17. The encoder uses x9 to
/// x13 for its own work, and a leaf function never calls out, so these are free for the whole function.
const VALUE_REGISTERS: [u8; 4] = [14, 15, 16, 17];

impl Frame {
    /// The register a value lives in, if it has one.
    fn register(&self, value: ValueId) -> Option<u32> {
        self.registers.get(&value).copied().map(u32::from)
    }
}

/// The register holding `value`: its own register, or `scratch` after a load from its slot.
fn operand_register(frame: &mut Frame, value: ValueId, scratch: u32, emitter: &mut Emitter) -> u32 {
    match frame.register(value) {
        Some(register) => register,
        None => {
            let slot = frame.slot(value);
            emitter.push(ldr_x(scratch, SP, slot));
            scratch
        }
    }
}

/// The register a result is computed into: its own register, or x9 for a result that is stored to its
/// slot afterwards.
fn result_register(frame: &Frame, value: ValueId) -> u32 {
    frame.register(value).unwrap_or(R9)
}

/// Store a result computed into x9 to its slot, when the result has no register.
fn finish_result(frame: &mut Frame, value: ValueId, emitter: &mut Emitter) {
    if frame.register(value).is_none() {
        let slot = frame.slot(value);
        emitter.push(str_x(R9, SP, slot));
    }
}

/// Emit an instruction of [`is_register_instruction`].
fn emit_register_instruction(
    instruction: &IrInstr,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    match instruction {
        IrInstr::ConstI32 { result, value } => {
            let destination = result_register(frame, *result);
            let bits = *value as u32;
            emitter.push(movz_w(destination, bits & 0xFFFF));
            if bits >> 16 != 0 {
                emitter.push(movk_w_high(destination, bits >> 16));
            }
            finish_result(frame, *result, emitter);
        }
        IrInstr::ConstI64 { result, value } => {
            let destination = result_register(frame, *result);
            emit_constant(*value as u64, destination, emitter);
            finish_result(frame, *result, emitter);
        }
        IrInstr::LocalGet { result, local } => {
            let source = frame.slot(*local);
            let destination = result_register(frame, *result);
            emitter.push(ldr_x(destination, SP, source));
            finish_result(frame, *result, emitter);
        }
        IrInstr::LocalSet { local, value } => {
            let register = operand_register(frame, *value, R9, emitter);
            let destination = frame.slot(*local);
            emitter.push(str_x(register, SP, destination));
        }
        IrInstr::I32Add {
            result,
            left,
            right,
        } => register_alu(false, ADD, *result, *left, *right, frame, emitter),
        IrInstr::I32Sub {
            result,
            left,
            right,
        } => register_alu(false, SUB, *result, *left, *right, frame, emitter),
        IrInstr::I32And {
            result,
            left,
            right,
        } => register_alu(false, AND, *result, *left, *right, frame, emitter),
        IrInstr::I32Or {
            result,
            left,
            right,
        } => register_alu(false, ORR, *result, *left, *right, frame, emitter),
        IrInstr::I32Xor {
            result,
            left,
            right,
        } => register_alu(false, EOR, *result, *left, *right, frame, emitter),
        IrInstr::I64Add {
            result,
            left,
            right,
        } => register_alu(true, ADD, *result, *left, *right, frame, emitter),
        IrInstr::I64Sub {
            result,
            left,
            right,
        } => register_alu(true, SUB, *result, *left, *right, frame, emitter),
        IrInstr::I64And {
            result,
            left,
            right,
        } => register_alu(true, AND, *result, *left, *right, frame, emitter),
        IrInstr::I64Or {
            result,
            left,
            right,
        } => register_alu(true, ORR, *result, *left, *right, frame, emitter),
        IrInstr::I64Xor {
            result,
            left,
            right,
        } => register_alu(true, EOR, *result, *left, *right, frame, emitter),
        IrInstr::I32Mul {
            result,
            left,
            right,
        } => register_multiply(false, *result, *left, *right, frame, emitter),
        IrInstr::I64Mul {
            result,
            left,
            right,
        } => register_multiply(true, *result, *left, *right, frame, emitter),
        other => {
            return Err(JitError::Unsupported(format!(
                "the {} instruction in a register",
                instruction_name(other)
            )))
        }
    }
    Ok(())
}

/// `result = left op right` for a register-form ALU operation, on 32 or 64 bits.
fn register_alu(
    wide: bool,
    base: u32,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = operand_register(frame, left, R9, emitter);
    let right = operand_register(frame, right, R10, emitter);
    let destination = result_register(frame, result);
    emitter.push(alu(wide, base, destination, left, right));
    finish_result(frame, result, emitter);
}

/// `result = left * right` on 32 or 64 bits.
fn register_multiply(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let left = operand_register(frame, left, R9, emitter);
    let right = operand_register(frame, right, R10, emitter);
    let destination = result_register(frame, result);
    emitter.push(mul(wide, destination, left, right));
    finish_result(frame, result, emitter);
}

fn emit_instruction(
    instruction: &IrInstr,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    if is_register_instruction(instruction) {
        return emit_register_instruction(instruction, frame, emitter);
    }
    match instruction {
        IrInstr::ConstI32 { result, value } => {
            let slot = frame.slot(*result);
            let bits = *value as u32;
            emitter.push(movz_w(R9, bits & 0xFFFF));
            if bits >> 16 != 0 {
                emitter.push(movk_w_high(R9, bits >> 16));
            }
            emitter.push(str_x(R9, SP, slot));
        }
        IrInstr::ConstI64 { result, value } => {
            let slot = frame.slot(*result);
            let bits = *value as u64;
            emitter.push(movz_x(R9, (bits & 0xFFFF) as u32, 0));
            emitter.push(movk_x(R9, ((bits >> 16) & 0xFFFF) as u32, 1));
            emitter.push(movk_x(R9, ((bits >> 32) & 0xFFFF) as u32, 2));
            emitter.push(movk_x(R9, ((bits >> 48) & 0xFFFF) as u32, 3));
            emitter.push(str_x(R9, SP, slot));
        }
        IrInstr::ConstF32 { result, value } => {
            let slot = frame.slot(*result);
            store_constant(u64::from(*value), slot, emitter);
        }
        IrInstr::ConstF64 { result, value } => {
            let slot = frame.slot(*result);
            store_constant(*value, slot, emitter);
        }
        IrInstr::F32Add {
            result,
            left,
            right,
        } => float_binary(false, 0x2800, *result, *left, *right, frame, emitter),
        IrInstr::F32Sub {
            result,
            left,
            right,
        } => float_binary(false, 0x3800, *result, *left, *right, frame, emitter),
        IrInstr::F32Mul {
            result,
            left,
            right,
        } => float_binary(false, 0x0800, *result, *left, *right, frame, emitter),
        IrInstr::F32Div {
            result,
            left,
            right,
        } => float_binary(false, 0x1800, *result, *left, *right, frame, emitter),
        IrInstr::F64Add {
            result,
            left,
            right,
        } => float_binary(true, 0x2800, *result, *left, *right, frame, emitter),
        IrInstr::F64Sub {
            result,
            left,
            right,
        } => float_binary(true, 0x3800, *result, *left, *right, frame, emitter),
        IrInstr::F64Mul {
            result,
            left,
            right,
        } => float_binary(true, 0x0800, *result, *left, *right, frame, emitter),
        IrInstr::F64Div {
            result,
            left,
            right,
        } => float_binary(true, 0x1800, *result, *left, *right, frame, emitter),
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
        IrInstr::CallHost {
            import,
            arguments,
            results,
        } => emit_host_call(*import, arguments, results, frame, emitter)?,
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
        } => float_trunc_into(*operation, *result, *value, frame, emitter),
        IrInstr::MemorySize { result } => {
            // Fixed for the call, as on x86-64: the length in bytes, shifted right by 16 bits.
            let destination = frame.slot(*result);
            emitter.push(ldr_x(R9, SP, frame.context));
            emitter.push(ldr_x(R9, R9, MEMORY_LENGTH_OFFSET));
            emitter.push(0xD340_FC00 | (16 << 16) | (9 << 5) | 9); // lsr x9, x9, #16
            emitter.push(str_x(R9, SP, destination));
        }
        IrInstr::GlobalGet { result, global } => {
            let destination = frame.slot(*result);
            globals_element(*global, frame, emitter)?;
            emitter.push(ldr_x(R9, R12, 0));
            emitter.push(str_x(R9, SP, destination));
        }
        IrInstr::GlobalSet { global, value } => {
            let source = frame.slot(*value);
            globals_element(*global, frame, emitter)?;
            emitter.push(ldr_x(R9, SP, source));
            emitter.push(str_x(R9, R12, 0));
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
            emitter.push(ldr_x(R9, SP, source));
            emitter.push(str_x(R9, SP, local_slot));
            emitter.push(str_x(R9, SP, result_slot));
        }
        IrInstr::I32Add {
            result,
            left,
            right,
        } => binary(false, ADD, *result, *left, *right, frame, emitter),
        IrInstr::I32Sub {
            result,
            left,
            right,
        } => binary(false, SUB, *result, *left, *right, frame, emitter),
        IrInstr::I32And {
            result,
            left,
            right,
        } => binary(false, AND, *result, *left, *right, frame, emitter),
        IrInstr::I32Or {
            result,
            left,
            right,
        } => binary(false, ORR, *result, *left, *right, frame, emitter),
        IrInstr::I32Xor {
            result,
            left,
            right,
        } => binary(false, EOR, *result, *left, *right, frame, emitter),
        IrInstr::I64Add {
            result,
            left,
            right,
        } => binary(true, ADD, *result, *left, *right, frame, emitter),
        IrInstr::I64Sub {
            result,
            left,
            right,
        } => binary(true, SUB, *result, *left, *right, frame, emitter),
        IrInstr::I64And {
            result,
            left,
            right,
        } => binary(true, AND, *result, *left, *right, frame, emitter),
        IrInstr::I64Or {
            result,
            left,
            right,
        } => binary(true, ORR, *result, *left, *right, frame, emitter),
        IrInstr::I64Xor {
            result,
            left,
            right,
        } => binary(true, EOR, *result, *left, *right, frame, emitter),
        IrInstr::I32Mul {
            result,
            left,
            right,
        } => multiply(false, *result, *left, *right, frame, emitter),
        IrInstr::I64Mul {
            result,
            left,
            right,
        } => multiply(true, *result, *left, *right, frame, emitter),
        IrInstr::I32Shl {
            result,
            left,
            right,
        } => binary(false, LSLV, *result, *left, *right, frame, emitter),
        IrInstr::I32ShrU {
            result,
            left,
            right,
        } => binary(false, LSRV, *result, *left, *right, frame, emitter),
        IrInstr::I32ShrS {
            result,
            left,
            right,
        } => binary(false, ASRV, *result, *left, *right, frame, emitter),
        IrInstr::I64Shl {
            result,
            left,
            right,
        } => binary(true, LSLV, *result, *left, *right, frame, emitter),
        IrInstr::I64ShrU {
            result,
            left,
            right,
        } => binary(true, LSRV, *result, *left, *right, frame, emitter),
        IrInstr::I64ShrS {
            result,
            left,
            right,
        } => binary(true, ASRV, *result, *left, *right, frame, emitter),
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
        IrInstr::I32Eqz { result, value } => eqz(false, *result, *value, frame, emitter),
        IrInstr::I64Eqz { result, value } => eqz(true, *result, *value, frame, emitter),
        IrInstr::I32Compare {
            result,
            left,
            right,
            comparison,
        } => compare(false, *result, *left, *right, *comparison, frame, emitter),
        IrInstr::I64Compare {
            result,
            left,
            right,
            comparison,
        } => compare(true, *result, *left, *right, *comparison, frame, emitter),
        IrInstr::Select {
            result,
            condition,
            left,
            right,
        } => {
            // The result is `left` when the condition is non-zero, else `right`. The flags from
            // the comparison survive the loads that follow it.
            let wide = frame.is_wide(*result);
            let condition = frame.slot(*condition);
            let left = frame.slot(*left);
            let right = frame.slot(*right);
            let destination = frame.slot(*result);
            emitter.push(ldr_w(R9, SP, condition));
            emitter.push(cmp_imm(false, R9, 0));
            emitter.push(ldr_x(R9, SP, left));
            emitter.push(ldr_x(R10, SP, right));
            emitter.push(csel(wide, R9, R9, R10, NE));
            emitter.push(str_x(R9, SP, destination));
        }
        IrInstr::IntConvert {
            result,
            value,
            operation,
        } => {
            let source = frame.slot(*value);
            let destination = frame.slot(*result);
            match operation {
                // Keep the low 32 bits: a 32-bit load and a full store of the zero-extended word.
                IntConversion::I32WrapI64 => {
                    emitter.push(ldr_w(R9, SP, source));
                    emitter.push(str_x(R9, SP, destination));
                }
                // ldrsw: sign-extend the low word.
                IntConversion::I64ExtendI32S => {
                    emitter.push(ldr_x_sign_word(R9, SP, source));
                    emitter.push(str_x(R9, SP, destination));
                }
                IntConversion::I64ExtendI32U => {
                    emitter.push(ldr_w(R9, SP, source));
                    emitter.push(str_x(R9, SP, destination));
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
            // sxtb/sxth/sxtw: the low bits of the source, sign-extended.
            match operation {
                SignExtend::I32Extend8S => {
                    emitter.push(ldr_w(R9, SP, source));
                    emitter.push(0x1300_1C00 | (R9 << 5) | R9); // sxtb w9, w9
                    emitter.push(str_x(R9, SP, destination));
                }
                SignExtend::I32Extend16S => {
                    emitter.push(ldr_w(R9, SP, source));
                    emitter.push(0x1300_3C00 | (R9 << 5) | R9); // sxth w9, w9
                    emitter.push(str_x(R9, SP, destination));
                }
                SignExtend::I64Extend8S => {
                    emitter.push(ldr_x(R9, SP, source));
                    emitter.push(0x9340_1C00 | (R9 << 5) | R9); // sxtb x9, w9
                    emitter.push(str_x(R9, SP, destination));
                }
                SignExtend::I64Extend16S => {
                    emitter.push(ldr_x(R9, SP, source));
                    emitter.push(0x9340_3C00 | (R9 << 5) | R9); // sxth x9, w9
                    emitter.push(str_x(R9, SP, destination));
                }
                SignExtend::I64Extend32S => {
                    emitter.push(ldr_x(R9, SP, source));
                    emitter.push(0x9340_7C00 | (R9 << 5) | R9); // sxtw x9, w9
                    emitter.push(str_x(R9, SP, destination));
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
            // The bounds hint is ignored: every access is checked here.
            let (width, access) = match operation {
                // An f32 or f64 load moves the same bits as the integer load of that width.
                MemoryLoad::I32 | MemoryLoad::F32 | MemoryLoad::I64_32U => {
                    (4, indexed(LOAD_W, R9, R12, R9))
                }
                MemoryLoad::I64 | MemoryLoad::F64 => (8, indexed(LOAD_X, R9, R12, R9)),
                MemoryLoad::I32_8U | MemoryLoad::I64_8U => (1, indexed(LOAD_BYTE_W, R9, R12, R9)),
                MemoryLoad::I32_8S => (1, indexed(LOAD_BYTE_SIGNED_W, R9, R12, R9)),
                MemoryLoad::I64_8S => (1, indexed(LOAD_BYTE_SIGNED_X, R9, R12, R9)),
                MemoryLoad::I32_16U | MemoryLoad::I64_16U => (2, indexed(LOAD_HALF_W, R9, R12, R9)),
                MemoryLoad::I32_16S => (2, indexed(LOAD_HALF_SIGNED_W, R9, R12, R9)),
                MemoryLoad::I64_16S => (2, indexed(LOAD_HALF_SIGNED_X, R9, R12, R9)),
                MemoryLoad::I64_32S => (4, indexed(LOAD_WORD_SIGNED_X, R9, R12, R9)),
            };
            emit_address(*address, *offset, width, frame, emitter);
            emitter.push(access);
            let destination = frame.slot(*result);
            emitter.push(str_x(R9, SP, destination));
        }
        IrInstr::Store {
            address,
            value,
            offset,
            operation,
            bounds: _,
        } => {
            let (width, access) = match operation {
                MemoryStore::I32 | MemoryStore::F32 | MemoryStore::I64_32 => {
                    (4, indexed(STORE_W, R10, R12, R9))
                }
                MemoryStore::I64 | MemoryStore::F64 => (8, indexed(STORE_X, R10, R12, R9)),
                MemoryStore::I32_8 | MemoryStore::I64_8 => (1, indexed(STORE_BYTE, R10, R12, R9)),
                MemoryStore::I32_16 | MemoryStore::I64_16 => (2, indexed(STORE_HALF, R10, R12, R9)),
            };
            emit_address(*address, *offset, width, frame, emitter);
            let source = frame.slot(*value);
            emitter.push(ldr_x(R10, SP, source));
            emitter.push(access);
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

/// `ldrsw x<rt>, [sp, #imm]`: a 32-bit load, sign-extended to 64 bits.
/// `fmov s<vd>, w<rn>` or, when `wide`, `fmov d<vd>, x<rn>`: a slot's bits into a float register.
fn fmov_to_float(wide: bool, vd: u32, rn: u32) -> u32 {
    let base = if wide { 0x9E67_0000 } else { 0x1E27_0000 };
    base | (rn << 5) | vd
}

/// `fmov w<rd>, s<vn>` or `fmov x<rd>, d<vn>`: a float register's bits back into a slot. The
/// 32-bit form zeroes the upper half of `x<rd>`.
fn fmov_from_float(wide: bool, rd: u32, vn: u32) -> u32 {
    let base = if wide { 0x9E66_0000 } else { 0x1E26_0000 };
    base | (vn << 5) | rd
}

/// `fadd`, `fsub`, `fmul`, or `fdiv` (by `opcode`: 0x2800, 0x3800, 0x0800, 0x1800), on single or
/// double registers.
fn float_arithmetic(wide: bool, opcode: u32, vd: u32, vn: u32, vm: u32) -> u32 {
    let base = if wide { 0x1E60_0000 } else { 0x1E20_0000 };
    base | (vm << 16) | opcode | (vn << 5) | vd
}

/// `fcmp <vn>, <vm>`: sets the flags for the two floats. An unordered pair sets C and V and clears
/// N and Z, so no ordered condition holds for it.
fn float_compare(wide: bool, vn: u32, vm: u32) -> u32 {
    let base = if wide { 0x1E60_2000 } else { 0x1E20_2000 };
    base | (vm << 16) | (vn << 5)
}

/// `fneg` or, when `abs`, `fabs` on a single or double register. A sign-bit operation: exact on
/// every input, NaNs included.
fn float_sign(wide: bool, abs: bool, vd: u32, vn: u32) -> u32 {
    let base = match (wide, abs) {
        (false, false) => 0x1E21_4000,
        (false, true) => 0x1E20_C000,
        (true, false) => 0x1E61_4000,
        (true, true) => 0x1E60_C000,
    };
    base | (vn << 5) | vd
}

/// A constant's bits in register `rd`, as four 16-bit moves.
fn emit_constant(bits: u64, rd: u32, emitter: &mut Emitter) {
    emitter.push(movz_x(rd, (bits & 0xFFFF) as u32, 0));
    emitter.push(movk_x(rd, ((bits >> 16) & 0xFFFF) as u32, 1));
    emitter.push(movk_x(rd, ((bits >> 32) & 0xFFFF) as u32, 2));
    emitter.push(movk_x(rd, ((bits >> 48) & 0xFFFF) as u32, 3));
}

/// A constant's bits into a slot, through `x9`.
fn store_constant(bits: u64, slot: u32, emitter: &mut Emitter) {
    emit_constant(bits, R9, emitter);
    emitter.push(str_x(R9, SP, slot));
}

/// `result = left op right` on floats: both operands go through `v0` and `v1`, and the result's
/// bits go back to the slot.
fn float_binary(
    wide: bool,
    opcode: u32,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let destination = load_operands(left, right, result, frame, emitter);
    emitter.push(fmov_to_float(wide, 0, R9));
    emitter.push(fmov_to_float(wide, 1, R10));
    emitter.push(float_arithmetic(wide, opcode, 0, 0, 1));
    canonicalize_nan(wide, emitter);
    emitter.push(fmov_from_float(wide, R9, 0));
    emitter.push(str_x(R9, SP, destination));
}

/// A conversion from an integer register to a float register: `scvtf` for signed, `ucvtf` for
/// unsigned, from a 32-bit (`w`) or 64-bit (`x`) source, to a single or double. Both round to
/// nearest-even, as Micro's `as` does.
fn int_to_float(to_double: bool, signed: bool, from_64: bool, vd: u32, rn: u32) -> u32 {
    let base = match (to_double, signed, from_64) {
        (false, true, false) => 0x1E22_0000,
        (false, true, true) => 0x9E22_0000,
        (false, false, false) => 0x1E23_0000,
        (false, false, true) => 0x9E23_0000,
        (true, true, false) => 0x1E62_0000,
        (true, true, true) => 0x9E62_0000,
        (true, false, false) => 0x1E63_0000,
        (true, false, true) => 0x9E63_0000,
    };
    base | (rn << 5) | vd
}

/// `fcvt` between widths: to a double from a single when `to_double`, else to a single.
fn float_widen_or_narrow(to_double: bool, vd: u32, vn: u32) -> u32 {
    let base = if to_double { 0x1E22_C000 } else { 0x1E62_4000 };
    base | (vn << 5) | vd
}

/// `fcvtzs` or `fcvtzu` (`signed`), from a single or double (`wide_source`) to a 32-bit or 64-bit
/// integer register, rounding toward zero. The 32-bit result is zero-extended.
fn float_to_int(wide_source: bool, signed: bool, to_64: bool, rd: u32, vn: u32) -> u32 {
    let base = match (wide_source, signed, to_64) {
        (false, true, false) => 0x1E38_0000,
        (false, true, true) => 0x9E38_0000,
        (false, false, false) => 0x1E39_0000,
        (false, false, true) => 0x9E39_0000,
        (true, true, false) => 0x1E78_0000,
        (true, true, true) => 0x9E78_0000,
        (true, false, false) => 0x1E79_0000,
        (true, false, true) => 0x9E79_0000,
    };
    base | (vn << 5) | rd
}

/// A float to an integer, truncating, which traps as Micro does on a NaN, an infinity, or a
/// truncated value outside the target's range. The value is truncated with `frintz`, then the
/// bounds are checked with comparisons that a NaN fails, and `fcvtz*` converts exactly.
fn float_trunc_into(
    operation: FloatTrunc,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
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
    let (lo, hi) = match (signed, to_64) {
        (true, false) => (-2147483648.0f64, 2147483648.0f64),
        (false, false) => (0.0, 4294967296.0),
        (true, true) => (-9223372036854775808.0, 9223372036854775808.0),
        (false, true) => (0.0, 18446744073709551616.0),
    };
    let bits = |value: f64| -> u64 {
        if wide {
            value.to_bits()
        } else {
            u64::from((value as f32).to_bits())
        }
    };
    /// The `ge` and `gt` conditions: signed greater-or-equal and greater, which an unordered
    /// comparison fails, since it sets V and clears N.
    const GE: u32 = 10;
    const GT: u32 = 12;
    let source = frame.slot(value);
    let destination = frame.slot(result);
    emitter.push(ldr_x(R9, SP, source));
    emitter.push(fmov_to_float(wide, 0, R9));
    emitter.push(float_round(wide, FloatUnary::Trunc, 0, 0));
    // Trap unless t >= lo.
    emit_constant(bits(lo), R10, emitter);
    emitter.push(fmov_to_float(wide, 1, R10));
    emitter.push(float_compare(wide, 0, 1));
    guard_trap(frame, GE, STATUS_INVALID_CONVERSION, emitter);
    // Trap unless hi > t.
    emit_constant(bits(hi), R10, emitter);
    emitter.push(fmov_to_float(wide, 2, R10));
    emitter.push(float_compare(wide, 2, 0));
    guard_trap(frame, GT, STATUS_INVALID_CONVERSION, emitter);
    emitter.push(float_to_int(wide, signed, to_64, R9, 0));
    emitter.push(str_x(R9, SP, destination));
}

/// A conversion between widths, or from an integer to a float, with the result in `x9`'s slot.
/// A NaN from a width change is replaced by the canonical NaN, as Micro does.
///
/// A conversion from an unsigned 64-bit integer is `ucvtf`, which is correct on its own, unlike the
/// x86-64 form, so no halving is needed here.
fn float_convert_into(
    operation: FloatConversion,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    let source = frame.slot(value);
    let destination = frame.slot(result);
    emitter.push(ldr_x(R9, SP, source));
    let (to_double, signed, from_64) = match operation {
        FloatConversion::F32FromI32S => (false, true, false),
        FloatConversion::F32FromI32U => (false, false, false),
        FloatConversion::F32FromI64S => (false, true, true),
        FloatConversion::F32FromI64U => (false, false, true),
        FloatConversion::F64FromI32S => (true, true, false),
        FloatConversion::F64FromI32U => (true, false, false),
        FloatConversion::F64FromI64S => (true, true, true),
        FloatConversion::F64FromI64U => (true, false, true),
        FloatConversion::F32FromF64 => {
            emitter.push(fmov_to_float(true, 0, R9));
            emitter.push(float_widen_or_narrow(false, 0, 0));
            canonicalize_nan(false, emitter);
            emitter.push(fmov_from_float(false, R9, 0));
            emitter.push(str_x(R9, SP, destination));
            return Ok(());
        }
        FloatConversion::F64FromF32 => {
            emitter.push(fmov_to_float(false, 0, R9));
            emitter.push(float_widen_or_narrow(true, 0, 0));
            canonicalize_nan(true, emitter);
            emitter.push(fmov_from_float(true, R9, 0));
            emitter.push(str_x(R9, SP, destination));
            return Ok(());
        }
    };
    emitter.push(int_to_float(to_double, signed, from_64, 0, R9));
    emitter.push(fmov_from_float(to_double, R9, 0));
    emitter.push(str_x(R9, SP, destination));
    Ok(())
}

/// A reinterpretation copies the bits: the same value, in the other type's slot. A 32-bit value is
/// loaded at 32 bits, which zero-extends it.
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
        Reinterpret::I32FromF32 | Reinterpret::F32FromI32 => emitter.push(ldr_w(R9, SP, source)),
        Reinterpret::I64FromF64 | Reinterpret::F64FromI64 => emitter.push(ldr_x(R9, SP, source)),
    }
    emitter.push(str_x(R9, SP, destination));
}

/// The rounding instruction for `operation` on a single or double register: `frintn` (to nearest,
/// ties to even), `frintm` (down, floor), `frintp` (up, ceil), or `frintz` (toward zero, trunc).
/// Each keeps the sign of its operand, so a zero result is signed as Micro's is.
fn float_round(wide: bool, operation: FloatUnary, vd: u32, vn: u32) -> u32 {
    let base = match operation {
        FloatUnary::Nearest => 0x1E24_4000,
        FloatUnary::Floor => 0x1E25_4000,
        FloatUnary::Ceil => 0x1E24_C000,
        _ => 0x1E25_C000, // Trunc
    };
    let base = if wide { base + 0x0040_0000 } else { base };
    base | (vn << 5) | vd
}

/// `fsqrt <vd>, <vn>`, on a single or double register.
fn float_sqrt(wide: bool, vd: u32, vn: u32) -> u32 {
    let base = if wide { 0x1E61_C000 } else { 0x1E21_C000 };
    base | (vn << 5) | vd
}

/// `min` or, when `maximum`, `max`, with Micro's rules. An unordered comparison takes the canonical
/// NaN. Equal operands take the OR of their bits for `min` and the AND for `max`, which picks the
/// negative zero for `min` and the positive one for `max`. Otherwise `min` takes the smaller operand
/// and `max` the larger.
fn float_min_max(
    wide: bool,
    maximum: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    /// The `vs` condition, set by an unordered comparison, and the `lt` condition.
    const VS: u32 = 6;
    const LT: u32 = 11;
    let destination = load_operands(left, right, result, frame, emitter);
    emitter.push(fmov_to_float(wide, 0, R9));
    emitter.push(fmov_to_float(wide, 1, R10));
    emitter.push(float_compare(wide, 0, 1));
    let nan = emitter.new_label();
    let unequal = emitter.new_label();
    let less = emitter.new_label();
    let done = emitter.new_label();
    emitter.branch(b_cond(VS), Target::Label(nan), Fixup::Branch19);
    emitter.branch(b_cond(NE), Target::Label(unequal), Fixup::Branch19);
    let combine = if maximum { AND } else { ORR };
    emitter.push(alu(wide, combine, R9, R9, R10));
    emitter.branch(B, Target::Label(done), Fixup::Branch26);
    emitter.bind(unequal);
    emitter.branch(b_cond(LT), Target::Label(less), Fixup::Branch19);
    // Greater: `min` takes the right operand, `max` keeps the left.
    if !maximum {
        emitter.push(mov_x(R9, R10));
    }
    emitter.branch(B, Target::Label(done), Fixup::Branch26);
    emitter.bind(less);
    // Less: `max` takes the right operand, `min` keeps the left.
    if maximum {
        emitter.push(mov_x(R9, R10));
    }
    emitter.branch(B, Target::Label(done), Fixup::Branch26);
    emitter.bind(nan);
    let bits = if wide {
        0x7ff8_0000_0000_0000
    } else {
        u64::from(0x7fc0_0000u32)
    };
    emit_constant(bits, R9, emitter);
    emitter.bind(done);
    emitter.push(str_x(R9, SP, destination));
}

/// `copysign`: the left operand's magnitude with the right operand's sign, bit for bit.
fn float_copysign(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let destination = load_operands(left, right, result, frame, emitter);
    let (magnitude, sign) = if wide {
        (u64::MAX >> 1, 1u64 << 63)
    } else {
        (0x7fff_ffff, 0x8000_0000)
    };
    emit_constant(magnitude, R11, emitter);
    emitter.push(alu(wide, AND, R9, R9, R11));
    emit_constant(sign, R12, emitter);
    emitter.push(alu(wide, AND, R10, R10, R12));
    emitter.push(alu(wide, ORR, R9, R9, R10));
    emitter.push(str_x(R9, SP, destination));
}

/// Replace a NaN in `v0` with the canonical NaN that Micro produces. An unordered `fcmp` sets V,
/// so the replacement is skipped unless the value is a NaN.
fn canonicalize_nan(wide: bool, emitter: &mut Emitter) {
    /// The `vc` condition: no overflow, which an unordered comparison sets.
    const VC: u32 = 7;
    emitter.push(float_compare(wide, 0, 0));
    let guard = emitter.words.len();
    emitter.push(0); // b.vc past the replacement, patched below
    let bits = if wide {
        0x7ff8_0000_0000_0000
    } else {
        u64::from(0x7fc0_0000u32)
    };
    emit_constant(bits, R9, emitter);
    emitter.push(fmov_to_float(wide, 0, R9));
    let distance = (emitter.words.len() - guard) as u32;
    emitter.words[guard] = b_cond(VC) | (distance << 5);
}

/// A float comparison as a 0 or 1 in the result slot. `<` and `<=` compare the right operand with
/// the left, so that "greater" (which an unordered pair fails) is the wanted order.
fn float_compare_into(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    comparison: FloatComparison,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    const GE: u32 = 10;
    const GT: u32 = 12;
    let destination = load_operands(left, right, result, frame, emitter);
    match comparison {
        FloatComparison::Eq | FloatComparison::Ne => {
            emitter.push(fmov_to_float(wide, 0, R9));
            emitter.push(fmov_to_float(wide, 1, R10));
            emitter.push(float_compare(wide, 0, 1));
            let condition = if comparison == FloatComparison::Eq {
                EQ
            } else {
                NE
            };
            emitter.push(cset(false, R9, condition));
        }
        FloatComparison::Gt | FloatComparison::Ge => {
            let condition = if comparison == FloatComparison::Gt {
                GT
            } else {
                GE
            };
            emitter.push(fmov_to_float(wide, 0, R9));
            emitter.push(fmov_to_float(wide, 1, R10));
            emitter.push(float_compare(wide, 0, 1));
            emitter.push(cset(false, R9, condition));
        }
        FloatComparison::Lt | FloatComparison::Le => {
            let condition = if comparison == FloatComparison::Lt {
                GT
            } else {
                GE
            };
            emitter.push(fmov_to_float(wide, 0, R10));
            emitter.push(fmov_to_float(wide, 1, R9));
            emitter.push(float_compare(wide, 0, 1));
            emitter.push(cset(false, R9, condition));
        }
    }
    emitter.push(str_x(R9, SP, destination));
}

/// Negation and absolute value on a float slot.
fn float_unary_into(
    wide: bool,
    abs: bool,
    result: ValueId,
    value: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let source = frame.slot(value);
    let destination = frame.slot(result);
    emitter.push(ldr_x(R9, SP, source));
    emitter.push(fmov_to_float(wide, 0, R9));
    emitter.push(float_sign(wide, abs, 0, 0));
    emitter.push(fmov_from_float(wide, R9, 0));
    emitter.push(str_x(R9, SP, destination));
}

/// The unary floats the native code covers: negation and absolute value. The rest are refused.
fn float_unary_arm(
    wide: bool,
    result: ValueId,
    value: ValueId,
    operation: FloatUnary,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
    match operation {
        FloatUnary::Neg => float_unary_into(wide, false, result, value, frame, emitter),
        FloatUnary::Abs => float_unary_into(wide, true, result, value, frame, emitter),
        FloatUnary::Nearest | FloatUnary::Floor | FloatUnary::Ceil | FloatUnary::Trunc => {
            let source = frame.slot(value);
            let destination = frame.slot(result);
            emitter.push(ldr_x(R9, SP, source));
            emitter.push(fmov_to_float(wide, 0, R9));
            emitter.push(float_round(wide, operation, 0, 0));
            canonicalize_nan(wide, emitter);
            emitter.push(fmov_from_float(wide, R9, 0));
            emitter.push(str_x(R9, SP, destination));
        }
        FloatUnary::Sqrt => {
            let source = frame.slot(value);
            let destination = frame.slot(result);
            emitter.push(ldr_x(R9, SP, source));
            emitter.push(fmov_to_float(wide, 0, R9));
            emitter.push(float_sqrt(wide, 0, 0));
            canonicalize_nan(wide, emitter);
            emitter.push(fmov_from_float(wide, R9, 0));
            emitter.push(str_x(R9, SP, destination));
        }
    }
    Ok(())
}

fn ldr_x_sign_word(rt: u32, rn: u32, imm: u32) -> u32 {
    0xB980_0000 | ((imm / 4) << 10) | (rn << 5) | rt
}

/// Check that global `index` is inside the globals the caller provided, then leave the address of the
/// global's value in `x12`. An index past the end traps, so code called without its globals cannot
/// read past them.
fn globals_element(index: u32, frame: &Frame, emitter: &mut Emitter) -> Result<(), JitError> {
    if !frame.env.globals {
        return Err(JitError::Unsupported(
            "a global access without a globals array".into(),
        ));
    }
    if index >= 1 << 28 {
        return Err(JitError::Unsupported(
            "a global index past the encodable range".into(),
        ));
    }
    let offset = index * 8;
    emitter.push(ldr_x(R12, SP, frame.context)); // the context
    emitter.push(ldr_x(R13, R12, GLOBALS_LENGTH_OFFSET));
    emitter.push(movz_x(R11, index & 0xFFFF, 0));
    emitter.push(movk_x(R11, index >> 16, 1));
    emitter.push(cmp(true, R13, R11)); // length - index
    let inside = emitter.new_label();
    emitter.branch(b_cond(HI), Target::Label(inside), Fixup::Branch19);
    trap(frame, STATUS_GLOBAL_OUT_OF_BOUNDS, emitter);
    emitter.bind(inside);
    emitter.push(ldr_x(R12, R12, GLOBALS_OFFSET)); // the globals' base
    emitter.push(movz_x(R10, offset & 0xFFFF, 0));
    emitter.push(movk_x(R10, offset >> 16, 1));
    emitter.push(add_x(R12, R12, R10)); // the global's address
    Ok(())
}

/// Copy a value's slot to another, eight bytes at a time.
fn copy(from: ValueId, to: ValueId, frame: &mut Frame, emitter: &mut Emitter) {
    let source = frame.slot(from);
    let destination = frame.slot(to);
    emitter.push(ldr_x(R9, SP, source));
    emitter.push(str_x(R9, SP, destination));
}

/// Load the two operands into `x9` and `x10`, and return the destination slot.
fn load_operands(
    left: ValueId,
    right: ValueId,
    result: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> u32 {
    let left = frame.slot(left);
    let right = frame.slot(right);
    let destination = frame.slot(result);
    emitter.push(ldr_x(R9, SP, left));
    emitter.push(ldr_x(R10, SP, right));
    destination
}

/// `result = left op right`, as one register-register operation, at 32 or 64 bits.
fn binary(
    wide: bool,
    base: u32,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let destination = load_operands(left, right, result, frame, emitter);
    emitter.push(alu(wide, base, R9, R9, R10));
    emitter.push(str_x(R9, SP, destination));
}

fn multiply(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let destination = load_operands(left, right, result, frame, emitter);
    emitter.push(mul(wide, R9, R9, R10));
    emitter.push(str_x(R9, SP, destination));
}

fn eqz(wide: bool, result: ValueId, value: ValueId, frame: &mut Frame, emitter: &mut Emitter) {
    let source = frame.slot(value);
    let destination = frame.slot(result);
    emitter.push(ldr_x(R9, SP, source));
    emitter.push(cmp_imm(wide, R9, 0));
    emitter.push(cset(false, R9, EQ));
    emitter.push(str_x(R9, SP, destination));
}

fn compare(
    wide: bool,
    result: ValueId,
    left: ValueId,
    right: ValueId,
    comparison: IntComparison,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let destination = load_operands(left, right, result, frame, emitter);
    emitter.push(cmp(wide, R9, R10));
    emitter.push(cset(false, R9, condition(comparison)));
    emitter.push(str_x(R9, SP, destination));
}

/// Division and remainder, signed or unsigned, at 32 or 64 bits, with Wasm's traps.
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
    let destination = load_operands(left, right, result, frame, emitter);

    // A zero divisor is a trap: `cbnz x10, ok; trap; ok:`.
    let nonzero = emitter.new_label();
    emitter.branch(cbnz(wide, R10), Target::Label(nonzero), Fixup::Branch19);
    trap(frame, crate::STATUS_DIVIDE_BY_ZERO, emitter);
    emitter.bind(nonzero);

    let normal = emitter.new_label();
    if signed {
        // A divisor of -1 skips the division. Its remainder is zero, and the quotient of the
        // minimum value traps.
        emitter.push(cmn_imm(wide, R10, 1));
        emitter.branch(b_cond(NE), Target::Label(normal), Fixup::Branch19);
        if remainder {
            emitter.push(movz_x(R9, 0, 0));
            let done = emitter.new_label();
            emitter.branch(B, Target::Label(done), Fixup::Branch26);
            emitter.bind(normal);
            quotient(true, remainder, wide, emitter);
            emitter.bind(done);
        } else {
            // x11 = the minimum value of the operand's width.
            if wide {
                emitter.push(movz_x(R11, 0x8000, 3));
            } else {
                emitter.push(movz_w_high(R11, 0x8000));
            }
            emitter.push(cmp(wide, R9, R11));
            emitter.branch(b_cond(NE), Target::Label(normal), Fixup::Branch19);
            trap(frame, crate::STATUS_OVERFLOW, emitter);
            emitter.bind(normal);
            quotient(true, false, wide, emitter);
        }
    } else {
        emitter.bind(normal);
        quotient(false, remainder, wide, emitter);
    }
    emitter.push(str_x(R9, SP, destination));
}

/// The quotient of `x9 / x10` into `x9`, or the remainder into `x9` when `remainder` is set.
///
/// The remainder is `x9 - (x9 / x10) * x10`, with the quotient in `x11`.
fn quotient(signed: bool, remainder: bool, wide: bool, emitter: &mut Emitter) {
    let divide = if signed { SDIV } else { UDIV };
    if remainder {
        emitter.push(alu(wide, divide, R11, R9, R10));
        emitter.push(msub(wide, R9, R11, R10, R9));
    } else {
        emitter.push(alu(wide, divide, R9, R9, R10));
    }
}

/// Compute the address of an access of `width` bytes into `x9`, check the access against the
/// memory, and leave the memory base in `x12`.
///
/// The effective address is the 32-bit operand plus the 32-bit offset, computed in 64 bits, so it
/// cannot wrap. The check is `address + width <= memory_len`. A failed check traps before any
/// memory is touched.
fn emit_address(
    address: ValueId,
    offset: u32,
    width: u32,
    frame: &mut Frame,
    emitter: &mut Emitter,
) {
    let address = frame.slot(address);
    emitter.push(ldr_w(R9, SP, address)); // zero-extends into x9
    emitter.push(movz_x(R10, offset & 0xFFFF, 0));
    emitter.push(movk_x(R10, offset >> 16, 1));
    emitter.push(add_x(R9, R9, R10));
    emitter.push(add_x_imm(R11, R9, width));
    emitter.push(ldr_x(R12, SP, frame.context));
    emitter.push(ldr_x(R13, R12, MEMORY_LENGTH_OFFSET));
    emitter.push(cmp(true, R11, R13));
    let inside = emitter.new_label();
    emitter.branch(b_cond(LS), Target::Label(inside), Fixup::Branch19);
    trap(frame, crate::STATUS_MEMORY_OUT_OF_BOUNDS, emitter);
    emitter.bind(inside);
    emitter.push(ldr_x(R12, R12, MEMORY_BASE_OFFSET)); // the memory base
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
    fn a_constant_return_starts_with_the_frame_setup_and_ends_with_ret() {
        use tpt_wasm_ir::{BlockId, IrValue};
        use tpt_wasm_types::{FunctionType, ResultType};
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
        let first = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(first, 0xA9BF_7BFD, "stp x29, x30, [sp, #-16]!");
        let last = u32::from_le_bytes(code[code.len() - 4..].try_into().unwrap());
        assert_eq!(last, 0xD65F_03C0, "ret");
    }
}
