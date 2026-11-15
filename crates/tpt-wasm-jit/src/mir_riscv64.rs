// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! RISC-V (RV64GC, LP64) code generated from the machine IR.
//!
//! This encoder reads a [`MirFunction`], not the IR, so the native path is `IR -> machine IR -> RV64`.
//! It handles the same slice as the machine IR and follows the design of [`crate::mir_x86`] and
//! [`crate::mir_aarch64`]: every slot is one 64-bit word in the frame, and each operation loads its
//! operands into scratch registers, computes, and stores the result back.
//!
//! The calling convention is LP64. The function receives a pointer to its arguments in `a0` and the
//! context pointer in `a1`, and returns its result in `a0`. A trap writes its status into the context
//! and returns zero; a call that finds a status left by its callee returns zero without a result.
//!
//! Frame layout: slot `i` is the eight bytes at `sp + 8 * i`, and the call areas are consecutive slots
//! with ascending entries, as on AArch64. `s0` is the frame pointer; the prologue saves it and the
//! return address and points it at the caller's `sp`, and the epilogue restores `sp` from it.
//!
//! The encodings have short reaches, and the encoder works within them. A conditional branch reaches
//! 4 KiB, so every conditional jump is an inverted branch over a stub, and a block edge is a `jal`,
//! which reaches 1 MiB. Loads and stores reach 2 KiB from `sp`, so a larger offset is first built in a
//! register.

use tpt_wasm_ir::{FloatComparison, IntComparison, IntUnary};

use crate::mir::{
    BinOp, DivOp, FloatOp, FloatUnaryOp, IntKind, LoadOp, MirFunction, MirOp, MirTerm, Precision,
    Slot, StoreOp, WordWidth, CANONICAL_NAN_F64,
};
use crate::shared::{check_environment, trap_status, widest_call, Environment};
use crate::{
    JitError, STATUS_CALL_DEPTH, STATUS_DIVIDE_BY_ZERO, STATUS_GLOBAL_OUT_OF_BOUNDS,
    STATUS_INDIRECT_TYPE_MISMATCH, STATUS_INVALID_CONVERSION, STATUS_MEMORY_OUT_OF_BOUNDS,
    STATUS_NULL_REFERENCE, STATUS_OVERFLOW, STATUS_TABLE_OUT_OF_BOUNDS,
};

// Integer registers.
const ZERO: u32 = 0;
const RA: u32 = 1;
const SP: u32 = 2;
const S0: u32 = 8;
const A0: u32 = 10;
const A1: u32 = 11;
const A2: u32 = 12;
const A3: u32 = 13;
/// Scratch registers. `t0` and `t1` hold the operands; `t2` to `t5` are temporaries; `t6` is kept for
/// the address of a frame slot that is out of the immediate's reach.
const T0: u32 = 5;
const T1: u32 = 6;
const T2: u32 = 7;
const T3: u32 = 28;
const T4: u32 = 29;
const T5: u32 = 30;
const T6: u32 = 31;

/// The byte offsets of the context fields the generated code reads and writes.
const STATUS_OFFSET: u32 = std::mem::offset_of!(crate::Context, status) as u32;
const MEMORY_BASE_OFFSET: u32 = std::mem::offset_of!(crate::Context, memory) as u32;
const MEMORY_LENGTH_OFFSET: u32 = std::mem::offset_of!(crate::Context, memory_len) as u32;
const MEMORY_CAPACITY_OFFSET: u32 = std::mem::offset_of!(crate::Context, memory_capacity) as u32;
const DEPTH_OFFSET: u32 = std::mem::offset_of!(crate::Context, depth) as u32;
const DEPTH_LIMIT_OFFSET: u32 = std::mem::offset_of!(crate::Context, depth_limit) as u32;
const FUNCTIONS_OFFSET: u32 = std::mem::offset_of!(crate::Context, functions) as u32;
const TABLE_LEN_OFFSET: u32 = std::mem::offset_of!(crate::Context, table_len) as u32;
const TABLE_TYPES_OFFSET: u32 = std::mem::offset_of!(crate::Context, table_types) as u32;
const TABLE_ENTRIES_OFFSET: u32 = std::mem::offset_of!(crate::Context, table_entries) as u32;
const GLOBALS_OFFSET: u32 = std::mem::offset_of!(crate::Context, globals) as u32;
const GLOBALS_LENGTH_OFFSET: u32 = std::mem::offset_of!(crate::Context, globals_len) as u32;
const HOST_CALL_OFFSET: u32 = std::mem::offset_of!(crate::Context, host_call) as u32;

// ---- instruction encodings. Each returns one 32-bit word.

const OP_IMM: u32 = 0x13;
const OP_IMM_32: u32 = 0x1B;
const OP: u32 = 0x33;
const OP_32: u32 = 0x3B;
const LOAD: u32 = 0x03;
const STORE: u32 = 0x23;
const BRANCH: u32 = 0x63;
const JAL: u32 = 0x6F;
const JALR: u32 = 0x67;
const LUI: u32 = 0x37;
const OP_FP: u32 = 0x53;

/// The I-type form: a 12-bit immediate, `rs1`, `funct3`, `rd`, and the opcode.
fn i_type(imm: u32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
    ((imm & 0xFFF) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

/// The S-type form: a 12-bit immediate split across the store.
fn s_type(imm: u32, rs2: u32, rs1: u32, funct3: u32, opcode: u32) -> u32 {
    let imm = imm & 0xFFF;
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

fn addi(rd: u32, rs1: u32, imm: u32) -> u32 {
    i_type(imm, rs1, 0, rd, OP_IMM)
}

fn addiw(rd: u32, rs1: u32, imm: u32) -> u32 {
    i_type(imm, rs1, 0, rd, OP_IMM_32)
}

fn sltiu(rd: u32, rs1: u32, imm: u32) -> u32 {
    i_type(imm, rs1, 3, rd, OP_IMM)
}

fn xori(rd: u32, rs1: u32, imm: u32) -> u32 {
    i_type(imm, rs1, 4, rd, OP_IMM)
}

/// `slli`, `srli` and `srai`: 64-bit shifts by an immediate.
fn slli(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type(shamt, rs1, 1, rd, OP_IMM)
}

fn srli(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type(shamt, rs1, 5, rd, OP_IMM)
}

fn srai(rd: u32, rs1: u32, shamt: u32) -> u32 {
    i_type(0x400 | shamt, rs1, 5, rd, OP_IMM)
}

/// `lui rd, imm20`: the upper 20 bits, sign-extended from bit 31.
fn lui(rd: u32, imm20: u32) -> u32 {
    ((imm20 & 0xF_FFFF) << 12) | (rd << 7) | LUI
}

/// A load by `funct3`: `lb` 0, `lh` 1, `lw` 2, `ld` 3, `lbu` 4, `lhu` 5, `lwu` 6.
fn load(funct3: u32, rd: u32, rs1: u32, offset: u32) -> u32 {
    i_type(offset, rs1, funct3, rd, LOAD)
}

/// A store by `funct3`: `sb` 0, `sh` 1, `sw` 2, `sd` 3.
fn store(funct3: u32, rs2: u32, rs1: u32, offset: u32) -> u32 {
    s_type(offset, rs2, rs1, funct3, STORE)
}

/// A 64-bit register operation (`OP`), by `funct7` and `funct3`.
fn op(funct7: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(funct7, rs2, rs1, funct3, rd, OP)
}

/// A 32-bit register operation (`OP-32`), whose result is sign-extended.
fn op_w(funct7: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(funct7, rs2, rs1, funct3, rd, OP_32)
}

/// A floating-point operation (`OP-FP`), with its rounding mode in `rm`.
fn fop(funct7: u32, rm: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    r_type(funct7, rs2, rs1, rm, rd, OP_FP)
}

/// `jalr rd, imm(rs1)`.
fn jalr(rd: u32, rs1: u32, imm: u32) -> u32 {
    i_type(imm, rs1, 0, rd, JALR)
}

/// `ret`, which is `jalr zero, 0(ra)`.
const RET: u32 = 0x0000_8067;

// Branch funct3 values. The condition's inverse is `funct3 ^ 1` for each of these.
const BEQ: u32 = 0;
const BNE: u32 = 1;
const BLTU: u32 = 6;
const BGEU: u32 = 7;

// Integer operations, by `funct7` and `funct3`.
const ADD: (u32, u32) = (0x00, 0);
const SUB: (u32, u32) = (0x20, 0);
const SLL: (u32, u32) = (0x00, 1);
const SLT: (u32, u32) = (0x00, 2);
const SLTU: (u32, u32) = (0x00, 3);
const XOR: (u32, u32) = (0x00, 4);
const SRL: (u32, u32) = (0x00, 5);
const SRA: (u32, u32) = (0x20, 5);
const OR: (u32, u32) = (0x00, 6);
const AND: (u32, u32) = (0x00, 7);
const MUL: (u32, u32) = (0x01, 0);
const DIV: (u32, u32) = (0x01, 4);
const DIVU: (u32, u32) = (0x01, 5);
const REM: (u32, u32) = (0x01, 6);
const REMU: (u32, u32) = (0x01, 7);

/// The `OP` or `OP-32` form of an operation given as `(funct7, funct3)`.
fn alu(wide: bool, (funct7, funct3): (u32, u32), rd: u32, rs1: u32, rs2: u32) -> u32 {
    if wide {
        op(funct7, funct3, rd, rs1, rs2)
    } else {
        op_w(funct7, funct3, rd, rs1, rs2)
    }
}

// Floating-point operations. The `rm` field is 7 for the dynamic rounding mode, which is the default.
const DYN: u32 = 7;
const FADD_D: u32 = 0x01;
const FSUB_D: u32 = 0x05;
const FMUL_D: u32 = 0x09;
const FDIV_D: u32 = 0x0D;
const FSQRT_D: u32 = 0x2D;
const FADD_S: u32 = 0x00;
const FSUB_S: u32 = 0x04;
const FMUL_S: u32 = 0x08;
const FDIV_S: u32 = 0x0C;
const FSQRT_S: u32 = 0x2C;
const FMIN_MAX_D: u32 = 0x15;
const FMIN_MAX_S: u32 = 0x14;
const FCMP_D: u32 = 0x51;
const FCMP_S: u32 = 0x50;
const FSGNJ_D: u32 = 0x11;
const FCVT_D_S: u32 = 0x21;
const FCVT_S_D: u32 = 0x20;
const FCVT_W_D: u32 = 0x61;
const FCVT_D_W: u32 = 0x69;
const FCVT_S_W: u32 = 0x68;
const FMV_X_D: u32 = 0x71;
const FMV_D_X: u32 = 0x79;
const FMV_X_W: u32 = 0x70;
const FMV_W_X: u32 = 0x78;

/// The conversion variant in `rs2`: a signed or unsigned word or doubleword.
const CVT_W: u32 = 0;
const CVT_WU: u32 = 1;
const CVT_L: u32 = 2;
const CVT_LU: u32 = 3;

/// The rounding modes of `fcvt` to an integer, the field `rm`.
const RM_RNE: u32 = 0;
const RM_RTZ: u32 = 1;
const RM_RDN: u32 = 2;
const RM_RUP: u32 = 3;

/// A place a branch goes, and how its offset is stored.
#[derive(Clone, Copy)]
enum Fixup {
    /// A conditional branch: a 13-bit signed byte offset.
    Branch13,
    /// `jal`: a 21-bit signed byte offset.
    Jump21,
}

/// The call areas of a function: the first slot of the argument entries, of the result entries, and of
/// the indirect call's entry point, with the signature ids as the table stores them.
struct Calls {
    block: u32,
    results_block: u32,
    entry: u32,
    type_ids: Vec<i32>,
}

/// The emitter for one function: its words, the labels of its blocks and of the inline sequences, and
/// the branches to patch once every label is placed.
struct Asm {
    words: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, usize, Fixup)>,
    /// The frame slot that holds the context pointer.
    context: u32,
}

impl Asm {
    fn new(context: u32, blocks: usize) -> Self {
        Self {
            words: Vec::new(),
            labels: vec![None; blocks],
            fixups: Vec::new(),
            context,
        }
    }

    fn push(&mut self, word: u32) {
        self.words.push(word);
    }

    fn label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }

    fn bind(&mut self, label: usize) {
        self.labels[label] = Some(self.words.len());
    }

    /// A conditional branch on `rs1 funct3 rs2` to `label`.
    fn branch(&mut self, funct3: u32, rs1: u32, rs2: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::Branch13));
        self.push(r_type(0, rs2, rs1, funct3, 0, BRANCH));
    }

    /// An unconditional jump to `label`.
    fn jump(&mut self, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::Jump21));
        self.push(JAL);
    }

    /// Resolve every branch and return the code, little-endian.
    fn finish(mut self) -> Result<Vec<u8>, JitError> {
        for (position, label, fixup) in std::mem::take(&mut self.fixups) {
            let target = self.labels[label]
                .ok_or_else(|| JitError::Unsupported("a branch to an unbound label".into()))?;
            let distance = (target as i64 - position as i64) * 4;
            match fixup {
                Fixup::Branch13 => {
                    if !(-(1 << 12)..(1 << 12)).contains(&distance) {
                        return Err(JitError::Unsupported(
                            "a conditional branch beyond 4 KiB".into(),
                        ));
                    }
                    self.words[position] |= b_immediate(distance as u32);
                }
                Fixup::Jump21 => {
                    if !(-(1 << 20)..(1 << 20)).contains(&distance) {
                        return Err(JitError::Unsupported("a jump beyond 1 MiB".into()));
                    }
                    self.words[position] =
                        (self.words[position] & 0x7F) | j_immediate(distance as u32);
                }
            }
        }
        Ok(self
            .words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect())
    }

    /// Load a 64-bit constant into `rd`, as the constant's shape requires.
    fn load_constant(&mut self, rd: u32, value: i64) {
        if let Ok(small) = i32::try_from(value) {
            if (-2048..2048).contains(&small) {
                self.push(addi(rd, ZERO, small as u32));
            } else {
                // The upper 20 bits, rounded so that the signed low 12 bits fit.
                let hi = small.wrapping_add(0x800) >> 12;
                let lo = small.wrapping_sub(hi << 12);
                self.push(lui(rd, hi as u32));
                self.push(addiw(rd, rd, lo as u32));
            }
            return;
        }
        // Built from 11-bit pieces, so every immediate is positive and fits.
        let bits = value as u64;
        let pieces = [
            ((bits >> 53) & 0x7FF, 11),
            ((bits >> 42) & 0x7FF, 11),
            ((bits >> 31) & 0x7FF, 11),
            ((bits >> 20) & 0x7FF, 11),
            ((bits >> 9) & 0x7FF, 11),
            (bits & 0x1FF, 9),
        ];
        self.push(addi(rd, ZERO, pieces[0].0 as u32));
        for (piece, width) in &pieces[1..] {
            self.push(slli(rd, rd, *width));
            if *piece != 0 {
                self.push(i_type(*piece as u32, rd, 6, rd, OP_IMM)); // ori
            }
        }
    }

    /// Load or store `reg` at byte `offset` from `sp`. An offset within the immediate's reach is used
    /// directly; a larger one is built in a register first, which for a store is `t6`.
    fn frame_access(&mut self, reg: u32, offset: u32, store_it: bool) {
        if offset < 2048 {
            self.push(if store_it {
                store(3, reg, SP, offset)
            } else {
                load(3, reg, SP, offset)
            });
            return;
        }
        let base = if store_it { T6 } else { reg };
        self.load_constant(base, i64::from(offset));
        self.push(op(ADD.0, ADD.1, base, base, SP));
        self.push(if store_it {
            store(3, reg, base, 0)
        } else {
            load(3, reg, base, 0)
        });
    }

    /// `rd = sp + offset`.
    fn frame_address(&mut self, rd: u32, offset: u32) {
        if offset < 2048 {
            self.push(addi(rd, SP, offset));
        } else {
            self.load_constant(rd, i64::from(offset));
            self.push(op(ADD.0, ADD.1, rd, rd, SP));
        }
    }

    /// Load a slot's whole word into `reg`.
    fn load_slot(&mut self, reg: u32, slot: Slot) {
        self.frame_access(reg, 8 * slot, false);
    }

    /// Store `reg`, the whole word, into a slot.
    fn store_slot(&mut self, reg: u32, slot: Slot) {
        self.frame_access(reg, 8 * slot, true);
    }

    /// Load a word of `width` from a slot: a 32-bit value zero-extended, or the whole word.
    fn load_word(&mut self, reg: u32, slot: Slot, width: WordWidth) {
        match width {
            WordWidth::Low32 => {
                self.load_slot(reg, slot);
                self.zero_extend_low32(reg);
            }
            WordWidth::Full64 => self.load_slot(reg, slot),
        }
    }

    /// `reg` keeps only its low 32 bits, zero-extended.
    fn zero_extend_low32(&mut self, reg: u32) {
        self.push(slli(reg, reg, 32));
        self.push(srli(reg, reg, 32));
    }

    /// Load the context pointer into `reg`.
    fn load_context(&mut self, reg: u32) {
        let context = self.context;
        self.load_slot(reg, context);
    }

    /// The epilogue: `sp` is restored from `s0`, which holds the caller's `sp`, and the return address
    /// and the caller's `s0` are reloaded from below it.
    fn epilogue(&mut self) {
        self.push(addi(SP, S0, 0));
        self.push(load(3, RA, SP, (-8_i32) as u32));
        self.push(load(3, S0, SP, (-16_i32) as u32));
        self.push(RET);
    }

    /// Return zero from the function, which is what a trap and a failed call return.
    fn return_zero(&mut self) {
        self.push(addi(A0, ZERO, 0));
        self.epilogue();
    }

    /// Write `status` into the context and return zero: the trap sequence.
    fn raise(&mut self, status: u32) {
        self.load_context(T3);
        self.load_constant(T4, i64::from(status));
        self.push(store(3, T4, T3, STATUS_OFFSET));
        self.return_zero();
    }

    /// Trap with `status` when `rs1 funct3 rs2` holds; otherwise continue. The branch skips the trap
    /// sequence on the inverse condition, which is `funct3 ^ 1`.
    fn trap_when(&mut self, funct3: u32, rs1: u32, rs2: u32, status: u32) {
        let skip = self.label();
        self.branch(funct3 ^ 1, rs1, rs2, skip);
        self.raise(status);
        self.bind(skip);
    }

    /// Load a float slot into the float register `f`, through `t0`.
    fn float_in(&mut self, f: u32, slot: Slot, precision: Precision) {
        self.load_slot(T0, slot);
        self.push(match precision {
            Precision::Double => fop(FMV_D_X, 0, f, T0, 0),
            Precision::Single => fop(FMV_W_X, 0, f, T0, 0),
        });
    }

    /// Store the float register `f` into a slot as its bits. An `f32` is stored zero-extended.
    fn float_out(&mut self, f: u32, slot: Slot, precision: Precision) {
        match precision {
            Precision::Double => self.push(fop(FMV_X_D, 0, T0, f, 0)),
            Precision::Single => {
                self.push(fop(FMV_X_W, 0, T0, f, 0));
                self.zero_extend_low32(T0);
            }
        }
        self.store_slot(T0, slot);
    }

    /// Replace a NaN in `f` with the canonical NaN that Micro gives. A quiet compare of `f` with itself
    /// is zero exactly for a NaN, so the replacement is skipped otherwise.
    fn canonicalize(&mut self, f: u32, precision: Precision) {
        let (compare, bits) = match precision {
            Precision::Double => (FCMP_D, CANONICAL_NAN_F64 as i64),
            Precision::Single => (FCMP_S, 0x7fc0_0000),
        };
        self.push(fop(compare, 2, T0, f, f)); // feq t0, f, f
        let ordered = self.label();
        self.branch(BNE, T0, ZERO, ordered);
        self.load_constant(T0, bits);
        self.push(match precision {
            Precision::Double => fop(FMV_D_X, 0, f, T0, 0),
            Precision::Single => fop(FMV_W_X, 0, f, T0, 0),
        });
        self.bind(ordered);
    }

    /// Round the double in `f0` to an integer in the direction `rm` (an `fcvt` rounding mode), keeping
    /// the sign of the input, as the rounding operations do. A magnitude at or above `2^52` is already
    /// integral, and a NaN is left for the canonicalization; both are kept as they are.
    fn round_double(&mut self, rm: u32) {
        let done = self.label();
        self.push(fop(FSGNJ_D, 2, 2, 0, 0)); // fsgnjx f2, f0, f0: the magnitude
        self.load_constant(T1, 0x4330_0000_0000_0000); // 2^52
        self.push(fop(FMV_D_X, 0, 1, T1, 0));
        self.push(fop(FCMP_D, 1, T0, 2, 1)); // flt t0, |x|, 2^52
        self.branch(BEQ, T0, ZERO, done);
        self.push(fop(FCVT_W_D, rm, T0, 0, CVT_L)); // fcvt.l.d t0, f0
        self.push(fop(FCVT_D_W, DYN, 2, T0, CVT_L)); // fcvt.d.l f2, t0
        self.push(fop(FSGNJ_D, 0, 0, 2, 0)); // fsgnj.d f0, f2, f0: the integer, with x's sign
        self.bind(done);
    }

    /// Check an access of `width` bytes at the 32-bit address in the slot, trapping past the memory's
    /// end, and leave the address in `t5`. The end is computed in 64 bits, so it cannot wrap.
    fn address(&mut self, address: Slot, offset: u32, width: u32) {
        self.load_slot(T1, address);
        self.zero_extend_low32(T1);
        self.load_constant(T2, i64::from(offset));
        self.push(alu(true, ADD, T1, T1, T2));
        self.push(addi(T6, T1, width));
        self.load_context(T3);
        self.push(load(3, T4, T3, MEMORY_LENGTH_OFFSET));
        // Trap when the memory is shorter than the access's end.
        self.trap_when(BLTU, T4, T6, STATUS_MEMORY_OUT_OF_BOUNDS);
        self.push(load(3, T5, T3, MEMORY_BASE_OFFSET));
        self.push(alu(true, ADD, T5, T5, T1));
    }

    /// Write the arguments of a call to their entries, in ascending order.
    fn write_arguments(&mut self, args: &[Slot], calls: &Calls) {
        for (index, arg) in args.iter().enumerate() {
            self.load_slot(T0, *arg);
            self.store_slot(T0, calls.block + index as u32);
        }
    }

    /// Count one more frame against the depth limit, and trap if that is past it.
    fn count_frame(&mut self) {
        self.load_context(T3);
        self.push(load(3, T0, T3, DEPTH_OFFSET));
        self.push(load(3, T1, T3, DEPTH_LIMIT_OFFSET));
        self.trap_when(BGEU, T0, T1, STATUS_CALL_DEPTH);
        self.push(addi(T0, T0, 1));
        self.push(store(3, T0, T3, DEPTH_OFFSET));
    }

    /// Set `a0` to the pointer to the arguments and `a1` to the context.
    fn set_registers(&mut self, calls: &Calls) {
        self.frame_address(A0, 8 * calls.block);
        let context = self.context;
        self.load_slot(A1, context);
    }

    /// After a callee returns: count the frame off, and if it left a status, return zero without a
    /// result. Otherwise store the callee's result, if there is one.
    fn leave(&mut self, dst: &[Slot], calls: &Calls) {
        self.load_context(T3);
        self.push(load(3, T0, T3, DEPTH_OFFSET));
        self.push(addi(T0, T0, (-1_i32) as u32));
        self.push(store(3, T0, T3, DEPTH_OFFSET));
        self.push(load(3, T0, T3, STATUS_OFFSET));
        let ok = self.label();
        self.branch(BEQ, T0, ZERO, ok);
        self.return_zero();
        self.bind(ok);
        match dst {
            [] => {}
            // A single result is in a0.
            [only] => self.store_slot(A0, *only),
            // Two or more results were written to the argument block, entries ascending from its
            // pointer, so result `j` is slot `block + j`.
            _ => {
                for (index, slot) in dst.iter().enumerate() {
                    self.load_slot(T0, calls.block + index as u32);
                    self.store_slot(T0, *slot);
                }
            }
        }
    }

    /// `dst = function(args...)`, through the context's function table.
    fn call(&mut self, function: u32, args: &[Slot], dst: &[Slot], calls: &Calls) {
        self.write_arguments(args, calls);
        self.count_frame();
        self.set_registers(calls);
        self.load_context(T3);
        self.push(load(3, T0, T3, FUNCTIONS_OFFSET));
        self.load_constant(T1, i64::from(8 * function));
        self.push(alu(true, ADD, T0, T0, T1));
        self.push(load(3, T0, T0, 0));
        self.push(jalr(RA, T0, 0));
        self.leave(dst, calls);
    }

    /// `dst = table[index](args...)`: the index is checked against the table's length, the slot against
    /// null, and its function's signature against the one the call names, in that order and with the
    /// traps Micro gives. Then the slot's entry is called, through a frame slot, because the registers
    /// the call needs are set after the depth check.
    fn call_indirect(
        &mut self,
        type_index: u32,
        index: Slot,
        args: &[Slot],
        dst: &[Slot],
        calls: &Calls,
    ) {
        self.write_arguments(args, calls);
        // t1 = the index, zero-extended from its slot.
        self.load_slot(T1, index);
        self.zero_extend_low32(T1);
        self.load_context(T3);
        self.push(load(3, T2, T3, TABLE_LEN_OFFSET));
        self.trap_when(BGEU, T1, T2, STATUS_TABLE_OUT_OF_BOUNDS);
        // t4 = the slot's signature id, which is -1 for a null slot.
        self.push(load(3, T4, T3, TABLE_TYPES_OFFSET));
        self.push(slli(T5, T1, 3));
        self.push(alu(true, ADD, T5, T4, T5));
        self.push(load(3, T4, T5, 0));
        self.push(addi(T5, ZERO, (-1_i32) as u32));
        self.trap_when(BEQ, T4, T5, STATUS_NULL_REFERENCE);
        let expected = calls.type_ids[type_index as usize];
        self.load_constant(T5, i64::from(expected));
        self.trap_when(BNE, T4, T5, STATUS_INDIRECT_TYPE_MISMATCH);
        // The slot's entry point, kept in the entry slot while the registers are set.
        self.push(load(3, T4, T3, TABLE_ENTRIES_OFFSET));
        self.push(slli(T5, T1, 3));
        self.push(alu(true, ADD, T5, T4, T5));
        self.push(load(3, T4, T5, 0));
        self.store_slot(T4, calls.entry);
        self.count_frame();
        self.set_registers(calls);
        self.load_slot(T0, calls.entry);
        self.push(jalr(RA, T0, 0));
        self.leave(dst, calls);
    }

    /// Check that global `index` is inside the globals the caller provided, and leave the address of
    /// its entry in `t2`. An index past the end traps, so code called without its globals cannot read
    /// past them.
    fn global_address(&mut self, index: u32) {
        self.load_context(T3);
        self.load_constant(T1, i64::from(index));
        self.push(load(3, T4, T3, GLOBALS_LENGTH_OFFSET));
        self.trap_when(BGEU, T1, T4, STATUS_GLOBAL_OUT_OF_BOUNDS);
        self.push(load(3, T2, T3, GLOBALS_OFFSET));
        self.load_constant(T1, 8 * i64::from(index));
        self.push(alu(true, ADD, T2, T2, T1));
    }

    /// The host call: the arguments are written as words, the trampoline is called with the context,
    /// the import index and the two areas, and a failed call returns zero with its status. Then each
    /// result is read from its word.
    fn host_call(
        &mut self,
        import: u32,
        args: &[(Slot, WordWidth)],
        dsts: &[(Slot, WordWidth)],
        calls: &Calls,
    ) {
        for (index, (arg, width)) in args.iter().enumerate() {
            self.load_word(T0, *arg, *width);
            self.store_slot(T0, calls.block + index as u32);
        }
        let context = self.context;
        self.load_slot(A0, context);
        self.load_constant(A1, i64::from(import));
        self.frame_address(A2, 8 * calls.block);
        self.frame_address(A3, 8 * calls.results_block);
        self.push(load(3, T0, A0, HOST_CALL_OFFSET));
        self.push(jalr(RA, T0, 0));
        self.load_context(T3);
        self.push(load(3, T0, T3, STATUS_OFFSET));
        let ok = self.label();
        self.branch(BEQ, T0, ZERO, ok);
        self.return_zero();
        self.bind(ok);
        for (index, (dst, width)) in dsts.iter().enumerate() {
            self.load_word(T0, calls.results_block + index as u32, *width);
            self.store_slot(T0, *dst);
        }
    }

    /// Make `dst = clz(a)`, `ctz(a)` or `popcnt(a)` in `t0`, on 32 or 64 bits. The base instruction set
    /// has no count instructions, so the counts come from shifts, masks, and the population count.
    fn count(&mut self, op: IntUnary, wide: bool) {
        let bits = if wide { 64 } else { 32 };
        match op {
            IntUnary::Clz => {
                // Smear the top set bit down through every lower bit; the zeros are the width less the ones.
                let shifts: &[u32] = if wide {
                    &[1, 2, 4, 8, 16, 32]
                } else {
                    &[1, 2, 4, 8, 16]
                };
                for shift in shifts {
                    self.push(srli(T1, T0, *shift));
                    self.push(alu(true, OR, T0, T0, T1));
                }
                self.popcount();
                self.load_constant(T1, bits);
                self.push(alu(true, SUB, T0, T1, T0));
            }
            IntUnary::Ctz => {
                // Isolate the lowest set bit, x & -x, and count the ones below it. A zero input gives all
                // ones, which the 32-bit count masks to 32.
                self.push(alu(true, SUB, T1, ZERO, T0));
                self.push(alu(true, AND, T0, T0, T1));
                self.push(addi(T0, T0, (-1_i32) as u32));
                if !wide {
                    self.zero_extend_low32(T0);
                }
                self.popcount();
            }
            IntUnary::Popcnt => self.popcount(),
        }
    }

    /// The population count of `t0`, in `t0`: the divide-and-add sum over 2-, 4- and 8-bit fields, and
    /// the byte sums collected into the top byte by a multiplication.
    fn popcount(&mut self) {
        self.push(srli(T1, T0, 1));
        self.load_constant(T2, 0x5555_5555_5555_5555);
        self.push(alu(true, AND, T1, T1, T2));
        self.push(alu(true, SUB, T0, T0, T1));
        self.load_constant(T2, 0x3333_3333_3333_3333);
        self.push(alu(true, AND, T1, T0, T2));
        self.push(srli(T0, T0, 2));
        self.push(alu(true, AND, T0, T0, T2));
        self.push(alu(true, ADD, T0, T0, T1));
        self.push(srli(T1, T0, 4));
        self.push(alu(true, ADD, T0, T0, T1));
        self.load_constant(T2, 0x0F0F_0F0F_0F0F_0F0F);
        self.push(alu(true, AND, T0, T0, T2));
        self.load_constant(T2, 0x0101_0101_0101_0101);
        self.push(alu(true, MUL, T0, T0, T2));
        self.push(srli(T0, T0, 56));
    }

    /// `dst = a op b` on the words, with the operator's rules: `i32` operations take the low 32 bits and
    /// produce a sign-extended word, and the shifts and rotations take their counts modulo the width.
    /// The bitwise operators combine bits position by position, so they are the same at both widths.
    fn binary(&mut self, op: BinOp, wide: bool, dst: Slot, a: Slot, b: Slot) {
        self.load_slot(T0, a);
        self.load_slot(T1, b);
        match op {
            BinOp::Add => self.push(alu(wide, ADD, T0, T0, T1)),
            BinOp::Sub => self.push(alu(wide, SUB, T0, T0, T1)),
            BinOp::And => self.push(alu(true, AND, T0, T0, T1)),
            BinOp::Or => self.push(alu(true, OR, T0, T0, T1)),
            BinOp::Xor => self.push(alu(true, XOR, T0, T0, T1)),
            BinOp::Mul => self.push(alu(wide, MUL, T0, T0, T1)),
            BinOp::Shl => self.push(alu(wide, SLL, T0, T0, T1)),
            BinOp::ShrS => self.push(alu(wide, SRA, T0, T0, T1)),
            BinOp::ShrU => self.push(alu(wide, SRL, T0, T0, T1)),
            BinOp::Rotl | BinOp::Rotr => {
                // A left rotation by n is a left shift by n or'd with a right shift by -n, and a right
                // rotation swaps the two shifts.
                let (left, right) = if op == BinOp::Rotl {
                    (SLL, SRL)
                } else {
                    (SRL, SLL)
                };
                self.push(alu(wide, SUB, T3, ZERO, T1)); // t3 = -count
                self.push(alu(wide, left, T2, T0, T1));
                self.push(alu(wide, right, T4, T0, T3));
                // The 64-bit `or`: RISC-V has no 32-bit form, and the shifted halves are sign-extended
                // words, so the 64-bit combination is correct at either width.
                self.push(alu(true, OR, T0, T2, T4));
            }
        }
        self.store_slot(T0, dst);
    }
}

impl Asm {
    /// `dst = a cond b` on the words. An `i32` comparison reads its operands from the low 32 bits, signed
    /// or unsigned as the comparison is, so each is extended from them first.
    fn compare(&mut self, cond: IntComparison, wide: bool, dst: Slot, a: Slot, b: Slot) {
        self.load_slot(T0, a);
        self.load_slot(T1, b);
        if !wide {
            let signed = matches!(
                cond,
                IntComparison::LtS | IntComparison::GeS | IntComparison::LeS | IntComparison::GtS
            );
            for reg in [T0, T1] {
                if signed {
                    self.push(addiw(reg, reg, 0));
                } else {
                    self.zero_extend_low32(reg);
                }
            }
        }
        match cond {
            IntComparison::Eq => {
                self.push(alu(true, XOR, T0, T0, T1));
                self.push(sltiu(T0, T0, 1));
            }
            IntComparison::Ne => {
                self.push(alu(true, XOR, T0, T0, T1));
                self.push(op(SLTU.0, SLTU.1, T0, ZERO, T0)); // sltu t0, zero, t0: t0 != 0
            }
            IntComparison::LtS => self.push(alu(true, SLT, T0, T0, T1)),
            IntComparison::GtS => self.push(alu(true, SLT, T0, T1, T0)),
            IntComparison::LeS => {
                self.push(alu(true, SLT, T0, T1, T0));
                self.push(xori(T0, T0, 1));
            }
            IntComparison::GeS => {
                self.push(alu(true, SLT, T0, T0, T1));
                self.push(xori(T0, T0, 1));
            }
            IntComparison::LtU => self.push(alu(true, SLTU, T0, T0, T1)),
            IntComparison::GtU => self.push(alu(true, SLTU, T0, T1, T0)),
            IntComparison::LeU => {
                self.push(alu(true, SLTU, T0, T1, T0));
                self.push(xori(T0, T0, 1));
            }
            IntComparison::GeU => {
                self.push(alu(true, SLTU, T0, T0, T1));
                self.push(xori(T0, T0, 1));
            }
        }
        self.store_slot(T0, dst);
    }

    /// `dst = a == 0`, for an `i32` read from its low 32 bits, or an `i64`.
    fn eqz(&mut self, wide: bool, dst: Slot, a: Slot) {
        self.load_slot(T0, a);
        if !wide {
            self.zero_extend_low32(T0);
        }
        self.push(sltiu(T0, T0, 1));
        self.store_slot(T0, dst);
    }

    /// `dst = clz`, `ctz` or `popcnt` of `a`, over 32 bits for `i32` and 64 for `i64`.
    fn unary(&mut self, op: IntUnary, wide: bool, dst: Slot, a: Slot) {
        self.load_slot(T0, a);
        if !wide {
            self.zero_extend_low32(T0);
        }
        self.count(op, wide);
        self.store_slot(T0, dst);
    }

    /// `dst = a op b` for a division or remainder. A zero divisor traps, and so does the signed division of
    /// the minimum by minus one; the remainder by minus one is zero, which the hardware gives without a trap.
    fn divide(&mut self, op: DivOp, wide: bool, dst: Slot, a: Slot, b: Slot) {
        self.load_slot(T0, a);
        self.load_slot(T1, b);
        // The divisor is zero for an `i32` when its low word is.
        if wide {
            self.trap_when(BEQ, T1, ZERO, STATUS_DIVIDE_BY_ZERO);
        } else {
            self.push(slli(T2, T1, 32));
            self.trap_when(BEQ, T2, ZERO, STATUS_DIVIDE_BY_ZERO);
        }
        if op == DivOp::DivS {
            // The minimum divided by minus one overflows: trap when the dividend is the minimum and the
            // divisor is minus one, both read at the width of the operation.
            let (dividend, divisor) = if wide {
                (T0, T1)
            } else {
                self.push(addiw(T3, T0, 0));
                self.push(addiw(T4, T1, 0));
                (T3, T4)
            };
            self.load_constant(T2, if wide { i64::MIN } else { i64::from(i32::MIN) });
            self.push(alu(true, XOR, T5, dividend, T2));
            self.push(sltiu(T5, T5, 1)); // t5: the dividend is the minimum
            self.push(addi(T6, divisor, 1));
            self.push(sltiu(T6, T6, 1)); // t6: the divisor is minus one
            self.push(alu(true, AND, T5, T5, T6));
            self.trap_when(BNE, T5, ZERO, STATUS_OVERFLOW);
        }
        let operation = match op {
            DivOp::DivS => DIV,
            DivOp::DivU => DIVU,
            DivOp::RemS => REM,
            DivOp::RemU => REMU,
        };
        self.push(alu(wide, operation, T0, T0, T1));
        self.store_slot(T0, dst);
    }

    /// `dst = a op b` for a float: the arithmetic and the `min` and `max` rules, then the canonical NaN for
    /// a NaN result. `copysign` combines the bits, so a NaN keeps its payload.
    fn float_binary(&mut self, op: FloatOp, precision: Precision, dst: Slot, a: Slot, b: Slot) {
        if op == FloatOp::Copysign {
            self.load_slot(T0, a);
            self.load_slot(T1, b);
            match precision {
                Precision::Double => {
                    // The magnitude of `a` and the sign bit of `b`.
                    self.push(slli(T0, T0, 1));
                    self.push(srli(T0, T0, 1));
                    self.push(srli(T1, T1, 63));
                    self.push(slli(T1, T1, 63));
                }
                Precision::Single => {
                    self.push(slli(T0, T0, 33));
                    self.push(srli(T0, T0, 33));
                    self.push(slli(T1, T1, 32));
                    self.push(srli(T1, T1, 63));
                    self.push(slli(T1, T1, 31));
                }
            }
            self.push(alu(true, OR, T0, T0, T1));
            self.store_slot(T0, dst);
            return;
        }
        self.float_in(0, a, precision);
        self.float_in(1, b, precision);
        let double = precision == Precision::Double;
        let word = match op {
            FloatOp::Add => {
                if double {
                    FADD_D
                } else {
                    FADD_S
                }
            }
            FloatOp::Sub => {
                if double {
                    FSUB_D
                } else {
                    FSUB_S
                }
            }
            FloatOp::Mul => {
                if double {
                    FMUL_D
                } else {
                    FMUL_S
                }
            }
            FloatOp::Div => {
                if double {
                    FDIV_D
                } else {
                    FDIV_S
                }
            }
            _ => 0,
        };
        match op {
            // RISC-V's `fmin` and `fmax` return the other operand when one is a NaN, where Micro gives the
            // canonical NaN, so a NaN operand is checked for first. Otherwise they order `-0` below `+0`,
            // as Micro's rules do.
            FloatOp::Min | FloatOp::Max => {
                let compare = if double { FCMP_D } else { FCMP_S };
                self.push(fop(compare, 2, T0, 0, 0)); // feq t0, f0, f0
                self.push(fop(compare, 2, T1, 1, 1)); // feq t1, f1, f1
                self.push(alu(true, AND, T0, T0, T1));
                let ordered = self.label();
                let done = self.label();
                self.branch(BNE, T0, ZERO, ordered);
                let (bits, to_float) = if double {
                    (CANONICAL_NAN_F64 as i64, FMV_D_X)
                } else {
                    (0x7fc0_0000, FMV_W_X)
                };
                self.load_constant(T0, bits);
                self.push(fop(to_float, 0, 0, T0, 0));
                self.jump(done);
                self.bind(ordered);
                let minmax = if double { FMIN_MAX_D } else { FMIN_MAX_S };
                let mode = if op == FloatOp::Min { 0 } else { 1 };
                self.push(fop(minmax, mode, 0, 0, 1));
                self.bind(done);
            }
            FloatOp::Add | FloatOp::Sub | FloatOp::Mul | FloatOp::Div => {
                self.push(fop(word, DYN, 0, 0, 1));
            }
            FloatOp::Copysign => unreachable!("handled above"),
        }
        self.canonicalize(0, precision);
        self.float_out(0, dst, precision);
    }

    /// `dst = op(a)` for a float. The sign operations work on the bits, so a NaN keeps its payload; the
    /// others give the canonical NaN for a NaN result, through the root or the rounding.
    fn float_unary(&mut self, op: FloatUnaryOp, precision: Precision, dst: Slot, a: Slot) {
        let double = precision == Precision::Double;
        match op {
            FloatUnaryOp::Abs | FloatUnaryOp::Neg => {
                self.load_slot(T0, a);
                match (op, double) {
                    (FloatUnaryOp::Abs, true) => {
                        self.push(slli(T0, T0, 1));
                        self.push(srli(T0, T0, 1));
                    }
                    (FloatUnaryOp::Abs, false) => {
                        self.push(slli(T0, T0, 33));
                        self.push(srli(T0, T0, 33));
                    }
                    (_, true) => {
                        self.push(addi(T1, ZERO, 1));
                        self.push(slli(T1, T1, 63));
                        self.push(alu(true, XOR, T0, T0, T1));
                    }
                    (_, false) => {
                        self.push(addi(T1, ZERO, 1));
                        self.push(slli(T1, T1, 31));
                        self.push(alu(true, XOR, T0, T0, T1));
                        self.zero_extend_low32(T0);
                    }
                }
                self.store_slot(T0, dst);
            }
            FloatUnaryOp::Sqrt => {
                self.float_in(0, a, precision);
                let word = if double { FSQRT_D } else { FSQRT_S };
                self.push(fop(word, DYN, 0, 0, 0));
                self.canonicalize(0, precision);
                self.float_out(0, dst, precision);
            }
            FloatUnaryOp::Ceil
            | FloatUnaryOp::Floor
            | FloatUnaryOp::Trunc
            | FloatUnaryOp::Nearest => {
                let rounding = match op {
                    FloatUnaryOp::Ceil => RM_RUP,
                    FloatUnaryOp::Floor => RM_RDN,
                    FloatUnaryOp::Trunc => RM_RTZ,
                    _ => RM_RNE,
                };
                self.float_in(0, a, precision);
                if double {
                    self.round_double(rounding);
                } else {
                    // Widening is exact, and the rounded value is an integer of at most 2^23 in magnitude,
                    // which narrowing keeps exactly.
                    self.push(fop(FCVT_D_S, DYN, 0, 0, 0));
                    self.round_double(rounding);
                    self.push(fop(FCVT_S_D, DYN, 0, 0, 1));
                }
                self.canonicalize(0, precision);
                self.float_out(0, dst, precision);
            }
        }
    }

    /// `dst = (float) src`: an integer of `from` converted to a float of precision `to`, rounded once by
    /// the conversion. The conversion reads the low 32 bits of a 32-bit source, signed or unsigned.
    fn int_to_float(&mut self, to: Precision, from: IntKind, dst: Slot, src: Slot) {
        self.load_slot(T0, src);
        let variant = match from {
            IntKind::Signed32 => CVT_W,
            IntKind::Unsigned32 => CVT_WU,
            IntKind::Signed64 => CVT_L,
            IntKind::Unsigned64 => CVT_LU,
        };
        let word = match to {
            Precision::Double => FCVT_D_W,
            Precision::Single => FCVT_S_W,
        };
        self.push(fop(word, DYN, 0, T0, variant));
        self.float_out(0, dst, to);
    }

    /// `dst = (float) src` between the precisions: `fcvt`, then the canonical NaN.
    fn float_to_float(&mut self, to: Precision, dst: Slot, src: Slot) {
        match to {
            Precision::Single => {
                self.float_in(0, src, Precision::Double);
                self.push(fop(FCVT_S_D, DYN, 0, 0, 1));
            }
            Precision::Double => {
                self.float_in(0, src, Precision::Single);
                self.push(fop(FCVT_D_S, DYN, 0, 0, 0));
            }
        }
        self.canonicalize(0, to);
        self.float_out(0, dst, to);
    }

    /// `dst = trunc(src)` as an integer of kind `to`, trapping as Micro does. The value is truncated first,
    /// so a NaN stays a NaN. A NaN traps, and so does a truncated value below the lower bound or at or
    /// above the upper one, both bounds exact in `f64`.
    fn float_to_int(&mut self, from: Precision, to: IntKind, dst: Slot, src: Slot) {
        self.float_in(0, src, from);
        if from == Precision::Single {
            self.push(fop(FCVT_D_S, DYN, 0, 0, 0));
        }
        self.round_double(RM_RTZ);
        // A NaN compares unequal to itself, so `feq` is zero and the conversion traps.
        self.push(fop(FCMP_D, 2, T0, 0, 0));
        self.trap_when(BEQ, T0, ZERO, STATUS_INVALID_CONVERSION);
        let (low, high, variant): (f64, f64, u32) = match to {
            IntKind::Signed32 => (-2_147_483_648.0, 2_147_483_648.0, CVT_W),
            IntKind::Unsigned32 => (0.0, 4_294_967_296.0, CVT_WU),
            IntKind::Signed64 => (
                -9_223_372_036_854_775_808.0,
                9_223_372_036_854_775_808.0,
                CVT_L,
            ),
            IntKind::Unsigned64 => (0.0, 18_446_744_073_709_551_616.0, CVT_LU),
        };
        // Trap below the lower bound: `flt t0, f0, low`.
        self.load_constant(T1, low.to_bits() as i64);
        self.push(fop(FMV_D_X, 0, 1, T1, 0));
        self.push(fop(FCMP_D, 1, T0, 0, 1));
        self.trap_when(BNE, T0, ZERO, STATUS_INVALID_CONVERSION);
        // Trap at or above the upper bound: `fle t0, high, f0`.
        self.load_constant(T1, high.to_bits() as i64);
        self.push(fop(FMV_D_X, 0, 1, T1, 0));
        self.push(fop(FCMP_D, 0, T0, 1, 0));
        self.trap_when(BNE, T0, ZERO, STATUS_INVALID_CONVERSION);
        self.push(fop(FCVT_W_D, RM_RTZ, T0, 0, variant));
        self.store_slot(T0, dst);
    }

    /// `dst = a cond b` for float operands: a comparison with a NaN is false, except `ne`.
    fn float_compare(
        &mut self,
        precision: Precision,
        cond: FloatComparison,
        dst: Slot,
        a: Slot,
        b: Slot,
    ) {
        self.float_in(0, a, precision);
        self.float_in(1, b, precision);
        let compare = match precision {
            Precision::Double => FCMP_D,
            Precision::Single => FCMP_S,
        };
        match cond {
            FloatComparison::Eq => self.push(fop(compare, 2, T0, 0, 1)),
            FloatComparison::Ne => {
                self.push(fop(compare, 2, T0, 0, 1));
                self.push(xori(T0, T0, 1));
            }
            FloatComparison::Lt => self.push(fop(compare, 1, T0, 0, 1)),
            FloatComparison::Le => self.push(fop(compare, 0, T0, 0, 1)),
            FloatComparison::Gt => self.push(fop(compare, 1, T0, 1, 0)),
            FloatComparison::Ge => self.push(fop(compare, 0, T0, 1, 0)),
        }
        self.store_slot(T0, dst);
    }

    /// `dst = select`: the whole word of `a` when the `i32` condition's low word is nonzero, else `b`.
    fn select(&mut self, dst: Slot, cond: Slot, a: Slot, b: Slot) {
        self.load_slot(T2, cond);
        self.push(slli(T2, T2, 32));
        self.load_slot(T0, a);
        self.load_slot(T1, b);
        let keep = self.label();
        self.branch(BNE, T2, ZERO, keep);
        self.push(addi(T0, T1, 0));
        self.bind(keep);
        self.store_slot(T0, dst);
    }

    /// `dst = memory.grow(delta)`. The new size is kept only if it is within the capacity; otherwise the
    /// result is -1 and the size is left alone. The old size, in pages, is the result on success.
    fn memory_grow(&mut self, dst: Slot, delta: Slot) {
        self.load_context(T3);
        self.push(load(3, T4, T3, MEMORY_LENGTH_OFFSET));
        self.load_slot(T2, delta);
        self.zero_extend_low32(T2);
        self.push(slli(T2, T2, 16));
        self.push(alu(true, ADD, T2, T2, T4));
        self.push(load(3, T1, T3, MEMORY_CAPACITY_OFFSET));
        let fail = self.label();
        // A capacity below the new size fails: `bltu cap, new`.
        self.branch(BLTU, T1, T2, fail);
        self.push(store(3, T2, T3, MEMORY_LENGTH_OFFSET));
        self.push(srli(T0, T4, 16));
        let done = self.label();
        self.jump(done);
        self.bind(fail);
        self.push(addi(T0, ZERO, (-1_i32) as u32));
        self.bind(done);
        self.store_slot(T0, dst);
    }

    /// `dst = memory.size`, in pages.
    fn memory_size(&mut self, dst: Slot) {
        self.load_context(T3);
        self.push(load(3, T4, T3, MEMORY_LENGTH_OFFSET));
        self.push(srli(T0, T4, 16));
        self.store_slot(T0, dst);
    }

    /// The `op` of one machine operation, emitted from its slots.
    fn op(&mut self, op: &MirOp, calls: &Calls) {
        match op {
            MirOp::Call {
                function,
                args,
                dst,
            } => self.call(*function, args, dst, calls),
            MirOp::CallIndirect {
                type_index,
                index,
                args,
                dst,
            } => self.call_indirect(*type_index, *index, args, dst, calls),
            MirOp::CallHost { import, args, dsts } => self.host_call(*import, args, dsts, calls),
            MirOp::GlobalGet { dst, index, width } => {
                self.global_address(*index);
                match width {
                    WordWidth::Low32 => self.push(load(6, T0, T2, 0)),
                    WordWidth::Full64 => self.push(load(3, T0, T2, 0)),
                }
                self.store_slot(T0, *dst);
            }
            MirOp::GlobalSet { index, src, width } => {
                self.global_address(*index);
                self.load_word(T0, *src, *width);
                self.push(store(3, T0, T2, 0));
            }
            MirOp::MemorySize { dst } => self.memory_size(*dst),
            MirOp::MemoryGrow { dst, delta } => self.memory_grow(*dst, *delta),
            MirOp::Load {
                op,
                dst,
                address,
                offset,
            } => {
                self.address(*address, *offset, op.width());
                // Each form fills the whole word: `lwu` and `lhu`/`lbu` zero-extend, and the signed forms
                // sign-extend.
                let access = match op {
                    LoadOp::Quad => load(3, T0, T5, 0),
                    LoadOp::Word => load(6, T0, T5, 0),
                    LoadOp::SignedWord => load(2, T0, T5, 0),
                    LoadOp::SignedByte => load(0, T0, T5, 0),
                    LoadOp::UnsignedByte => load(4, T0, T5, 0),
                    LoadOp::SignedHalf => load(1, T0, T5, 0),
                    LoadOp::UnsignedHalf => load(5, T0, T5, 0),
                };
                self.push(access);
                self.store_slot(T0, *dst);
            }
            MirOp::Store {
                op,
                address,
                value,
                offset,
            } => {
                self.address(*address, *offset, op.width());
                self.load_slot(T0, *value);
                self.push(match op {
                    StoreOp::Quad => store(3, T0, T5, 0),
                    StoreOp::Word => store(2, T0, T5, 0),
                    StoreOp::Byte => store(0, T0, T5, 0),
                    StoreOp::Half => store(1, T0, T5, 0),
                });
            }
            MirOp::Divide { op, dst, a, b } => self.divide(*op, false, *dst, *a, *b),
            MirOp::Divide64 { op, dst, a, b } => self.divide(*op, true, *dst, *a, *b),
            MirOp::Const { dst, value } => {
                self.load_constant(T0, i64::from(*value));
                self.store_slot(T0, *dst);
            }
            MirOp::Const64 { dst, value } => {
                self.load_constant(T0, *value);
                self.store_slot(T0, *dst);
            }
            MirOp::Move { dst, src } => {
                self.load_slot(T0, *src);
                self.store_slot(T0, *dst);
            }
            MirOp::Bin { op, dst, a, b } => self.binary(*op, false, *dst, *a, *b),
            MirOp::Bin64 { op, dst, a, b } => self.binary(*op, true, *dst, *a, *b),
            MirOp::Compare { cond, dst, a, b } => self.compare(*cond, false, *dst, *a, *b),
            MirOp::Compare64 { cond, dst, a, b } => self.compare(*cond, true, *dst, *a, *b),
            MirOp::Eqz { dst, a } => self.eqz(false, *dst, *a),
            MirOp::Eqz64 { dst, a } => self.eqz(true, *dst, *a),
            MirOp::Unary { op, dst, a } => self.unary(*op, false, *dst, *a),
            MirOp::Unary64 { op, dst, a } => self.unary(*op, true, *dst, *a),
            MirOp::FloatBin {
                precision,
                op,
                dst,
                a,
                b,
            } => self.float_binary(*op, *precision, *dst, *a, *b),
            MirOp::FloatCompare {
                precision,
                cond,
                dst,
                a,
                b,
            } => self.float_compare(*precision, *cond, *dst, *a, *b),
            MirOp::FloatUnary {
                precision,
                op,
                dst,
                a,
            } => self.float_unary(*op, *precision, *dst, *a),
            MirOp::Narrow { dst, src } => {
                self.load_slot(T0, *src);
                self.zero_extend_low32(T0);
                self.store_slot(T0, *dst);
            }
            MirOp::SignExtend { dst, src, bits } => {
                self.load_slot(T0, *src);
                match bits {
                    8 => {
                        self.push(slli(T0, T0, 56));
                        self.push(srai(T0, T0, 56));
                    }
                    16 => {
                        self.push(slli(T0, T0, 48));
                        self.push(srai(T0, T0, 48));
                    }
                    _ => self.push(addiw(T0, T0, 0)),
                }
                self.store_slot(T0, *dst);
            }
            MirOp::IntToFloat { to, from, dst, src } => self.int_to_float(*to, *from, *dst, *src),
            MirOp::FloatToFloat { to, dst, src } => self.float_to_float(*to, *dst, *src),
            MirOp::FloatToInt { from, to, dst, src } => self.float_to_int(*from, *to, *dst, *src),
            MirOp::Select { dst, cond, a, b } => self.select(*dst, *cond, *a, *b),
        }
    }
}

/// Encode a machine-IR function for RV64, returning the code bytes.
///
/// # Errors
///
/// [`JitError::Unsupported`] for an empty function, a jump or a
/// conditional branch past its reach, or anything [`check_environment`] refuses.
pub(crate) fn encode(function: &MirFunction, env: &Environment) -> Result<Vec<u8>, JitError> {
    if function.blocks.is_empty() {
        return Err(JitError::Unsupported(
            "a machine function with no blocks".into(),
        ));
    }
    check_environment(function, env)?;
    let widest = widest_call(function);
    let calls = Calls {
        block: function.slots + 2,
        results_block: function.slots + 2 + widest,
        entry: function.slots + 2 + 2 * widest,
        // Only the ids of types an indirect call names are compared, and those fit, as checked above.
        type_ids: env
            .type_ids
            .iter()
            .map(|id| i32::try_from(*id).unwrap_or(0))
            .collect(),
    };
    // Every slot is eight bytes, and the frame is rounded to 16 as the LP64 convention requires.
    let frame = (8 * (calls.entry + 1)).div_ceil(16) * 16;
    let context = function.slots + 1;
    let mut asm = Asm::new(context, function.blocks.len());

    // Save the caller's s0 and return address below sp, point s0 at the caller's sp, then reserve the
    // frame. The frame size goes through a register, since it may not fit an immediate.
    asm.push(addi(SP, SP, (-16_i32) as u32));
    asm.push(store(3, S0, SP, 0));
    asm.push(store(3, RA, SP, 8));
    asm.push(addi(S0, SP, 16));
    // The frame is reserved in steps of at most 2032 bytes, each an `addi sp, sp, -n` that keeps sp
    // 16-byte aligned, so each step fits the immediate.
    let mut remaining = frame;
    while remaining > 0 {
        let step = remaining.min(2032);
        asm.push(addi(SP, SP, (-(step as i32)) as u32));
        remaining -= step;
    }

    // Every slot starts at zero, as the machine IR does, so a declared local read before a write is zero.
    for slot in 0..function.slots {
        asm.store_slot(ZERO, slot);
    }
    // The argument pointer and the context pointer are kept in their slots, and each parameter is read
    // from the argument entry it names.
    asm.store_slot(A0, function.slots);
    asm.store_slot(A1, context);
    for (index, slot) in function.params.iter().enumerate() {
        asm.load_slot(T0, function.slots);
        asm.load_constant(T1, 8 * index as i64);
        asm.push(alu(true, ADD, T0, T0, T1));
        asm.push(load(3, T0, T0, 0));
        asm.store_slot(T0, *slot);
    }
    asm.jump(function.entry);

    for (index, block) in function.blocks.iter().enumerate() {
        asm.bind(index);
        for op in &block.ops {
            asm.op(op, &calls);
        }
        match &block.term {
            MirTerm::Jump(target) => asm.jump(*target),
            MirTerm::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // The condition is an `i32`, so its low word decides the branch.
                let else_path = asm.label();
                asm.load_slot(T0, *cond);
                asm.zero_extend_low32(T0);
                asm.branch(BEQ, T0, ZERO, else_path);
                asm.jump(*then_block);
                asm.bind(else_path);
                asm.jump(*else_block);
            }
            MirTerm::Return(values) => match values.as_slice() {
                [] => asm.return_zero(),
                [value] => {
                    asm.load_slot(A0, *value);
                    asm.epilogue();
                }
                // Several values go to the argument block, entry `j` at `8 * j`, and the return
                // register is then zero.
                values => {
                    asm.load_slot(T1, function.slots);
                    for (index, value) in values.iter().enumerate() {
                        asm.load_slot(T0, *value);
                        asm.load_constant(T2, i64::from(8 * index as u32));
                        asm.push(alu(true, ADD, T2, T1, T2));
                        asm.push(store(3, T0, T2, 0));
                    }
                    asm.return_zero();
                }
            },
            MirTerm::Trap(trap) => {
                let status = trap_status(trap).ok_or_else(|| {
                    JitError::Unsupported(format!("a {trap:?} trap in machine code"))
                })?;
                asm.raise(status);
            }
        }
    }
    asm.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_offsets_are_word_aligned() {
        for offset in [
            STATUS_OFFSET,
            MEMORY_BASE_OFFSET,
            MEMORY_LENGTH_OFFSET,
            MEMORY_CAPACITY_OFFSET,
            DEPTH_OFFSET,
            DEPTH_LIMIT_OFFSET,
            FUNCTIONS_OFFSET,
            TABLE_LEN_OFFSET,
            TABLE_TYPES_OFFSET,
            TABLE_ENTRIES_OFFSET,
            GLOBALS_OFFSET,
            GLOBALS_LENGTH_OFFSET,
            HOST_CALL_OFFSET,
        ] {
            assert_eq!(
                offset % 8,
                0,
                "a context field at {offset} is not word-aligned"
            );
        }
    }

    #[test]
    fn a_small_constant_is_one_instruction() {
        let mut asm = Asm::new(0, 0);
        asm.load_constant(T0, -5);
        assert_eq!(asm.words, vec![addi(T0, ZERO, (-5_i32) as u32)]);
    }

    #[test]
    fn a_constant_return_starts_with_the_frame_setup_and_ends_with_ret() {
        use crate::mir::{MirBlock, MirFunction, MirOp, MirTerm};
        let function = MirFunction {
            params: Vec::new(),
            slots: 1,
            entry: 0,
            blocks: vec![MirBlock {
                ops: vec![MirOp::Const { dst: 0, value: 7 }],
                term: MirTerm::Return(vec![0]),
            }],
        };
        let env = Environment {
            memory_grow: false,
            callable: &[],
            indirect: false,
            type_ids: &[],
            globals: false,
            host: false,
            sse41: true,
        };
        let code = encode(&function, &env).expect("a constant return encodes");
        let word = |at: usize| u32::from_le_bytes(code[at..at + 4].try_into().expect("four bytes"));
        // addi sp, sp, -16 is the first word: the prologue saves s0 and the return address below sp.
        assert_eq!(
            word(0),
            addi(SP, SP, (-16_i32) as u32),
            "the prologue reserves the save area"
        );
        assert_eq!(word(code.len() - 4), RET, "the function ends with ret");
    }
}
