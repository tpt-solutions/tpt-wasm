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
    BasicBlock, BlockId, IntComparison, IntConversion, IrFunction, IrInstr, MemoryLoad,
    MemoryStore, SignExtend, Terminator, ValueId,
};
use tpt_wasm_types::ValueType;

use crate::x86::{check_supported, instruction_name, terminator_name};
use crate::{Context, JitError};

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

/// The largest frame the encoder emits.
const MAX_FRAME: u32 = 4096;

/// The byte offsets of the context fields the generated code uses.
const MEMORY_BASE_OFFSET: u32 = std::mem::offset_of!(Context, memory) as u32;
const MEMORY_LENGTH_OFFSET: u32 = std::mem::offset_of!(Context, memory_len) as u32;
const STATUS_OFFSET: u32 = std::mem::offset_of!(Context, status) as u32;

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

    /// The frame size: the slots, rounded up to 16 as AAPCS64 requires.
    fn size(&self) -> u32 {
        self.used.div_ceil(16) * 16
    }

    fn is_wide(&self, value: ValueId) -> bool {
        self.types.get(&value) == Some(&ValueType::I64)
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
pub(crate) fn encode(function: &IrFunction) -> Result<Vec<u8>, JitError> {
    check_supported(function)?;
    let blocks: HashMap<BlockId, &BasicBlock> = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect();

    let mut frame = Frame::new(function);
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

fn emit_instruction(
    instruction: &IrInstr,
    frame: &mut Frame,
    emitter: &mut Emitter,
) -> Result<(), JitError> {
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
                MemoryLoad::I32 | MemoryLoad::I64_32U => (4, indexed(LOAD_W, R9, R12, R9)),
                MemoryLoad::I64 => (8, indexed(LOAD_X, R9, R12, R9)),
                MemoryLoad::I32_8U | MemoryLoad::I64_8U => (1, indexed(LOAD_BYTE_W, R9, R12, R9)),
                MemoryLoad::I32_8S => (1, indexed(LOAD_BYTE_SIGNED_W, R9, R12, R9)),
                MemoryLoad::I64_8S => (1, indexed(LOAD_BYTE_SIGNED_X, R9, R12, R9)),
                MemoryLoad::I32_16U | MemoryLoad::I64_16U => (2, indexed(LOAD_HALF_W, R9, R12, R9)),
                MemoryLoad::I32_16S => (2, indexed(LOAD_HALF_SIGNED_W, R9, R12, R9)),
                MemoryLoad::I64_16S => (2, indexed(LOAD_HALF_SIGNED_X, R9, R12, R9)),
                MemoryLoad::I64_32S => (4, indexed(LOAD_WORD_SIGNED_X, R9, R12, R9)),
                other => return Err(JitError::Unsupported(format!("the {other:?} load"))),
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
                MemoryStore::I32 | MemoryStore::I64_32 => (4, indexed(STORE_W, R10, R12, R9)),
                MemoryStore::I64 => (8, indexed(STORE_X, R10, R12, R9)),
                MemoryStore::I32_8 | MemoryStore::I64_8 => (1, indexed(STORE_BYTE, R10, R12, R9)),
                MemoryStore::I32_16 | MemoryStore::I64_16 => (2, indexed(STORE_HALF, R10, R12, R9)),
                other => return Err(JitError::Unsupported(format!("the {other:?} store"))),
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
fn ldr_x_sign_word(rt: u32, rn: u32, imm: u32) -> u32 {
    0xB980_0000 | ((imm / 4) << 10) | (rn << 5) | rt
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
        let code = encode(&function).expect("compiles");
        let first = u32::from_le_bytes(code[0..4].try_into().unwrap());
        assert_eq!(first, 0xA9BF_7BFD, "stp x29, x30, [sp, #-16]!");
        let last = u32::from_le_bytes(code[code.len() - 4..].try_into().unwrap());
        assert_eq!(last, 0xD65F_03C0, "ret");
    }
}
