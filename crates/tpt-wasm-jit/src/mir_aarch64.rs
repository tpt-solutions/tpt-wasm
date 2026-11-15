// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! AArch64 code generated from the machine IR.
//!
//! This encoder reads a [`MirFunction`], not the IR, so the native path is `IR -> machine IR ->
//! AArch64`. It handles the same slice as the machine IR and follows the same design as
//! [`crate::mir_x86`]: every slot is one 64-bit word in the frame, and each operation loads its
//! operands into scratch registers, computes, and stores the result back. Nothing is kept in a
//! register across operations.
//!
//! The calling convention is AAPCS64. The function receives a pointer to its arguments in `x0` and
//! the context pointer in `x1`, and returns its result in `x0`. The code follows the other encoders'
//! protocol: a trap writes its status into the context and returns zero, and a call that finds a
//! status left by its callee returns zero without a result.
//!
//! Frame layout: slot `i` is the eight bytes at `sp + 8 * i`. Call areas are consecutive slots with
//! the entries ascending in memory, so an area's first slot is the pointer the callee reads from.
//! The frame pointer `x29` is set from `sp` on entry, and every epilogue restores `sp` from it.
//!
//! Scratch registers are `x9` to `x14`, and `d0` to `d3` for floats. The encodings are the standard
//! ones; each emitter names its instruction and the operation's rules.

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

/// Register numbers. `31` is `sp` where the instruction reads a base, and `xzr` where it reads a
/// register operand.
const X0: u32 = 0;
const X1: u32 = 1;
const X2: u32 = 2;
const X3: u32 = 3;
const X9: u32 = 9;
const X10: u32 = 10;
const X11: u32 = 11;
const X12: u32 = 12;
const X13: u32 = 13;
const X14: u32 = 14;
const ZR: u32 = 31;
const SP: u32 = 31;

/// Condition codes.
const EQ: u32 = 0;
const NE: u32 = 1;
const HS: u32 = 2;
const LO: u32 = 3;
const MI: u32 = 4;
const VS: u32 = 6;
const VC: u32 = 7;
const HI: u32 = 8;
const LS: u32 = 9;
const GE: u32 = 10;
const LT: u32 = 11;
const GT: u32 = 12;
const LE: u32 = 13;

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

/// The three-register form: `base` with `rm` in bits 20:16, `rn` in 9:5 and `rd` in 4:0.
fn dp(base: u32, rd: u32, rn: u32, rm: u32) -> u32 {
    base | (rm << 16) | (rn << 5) | rd
}

// Integer, 64-bit forms (`x`) and 32-bit forms (`w`) of the three-register operations.
const ADD_X: u32 = 0x8B00_0000;
const ADD_W: u32 = 0x0B00_0000;
const SUB_X: u32 = 0xCB00_0000;
const SUB_W: u32 = 0x4B00_0000;
const AND_X: u32 = 0x8A00_0000;
const AND_W: u32 = 0x0A00_0000;
const BIC_X: u32 = 0x8A20_0000;
const BIC_W: u32 = 0x0A20_0000;
const ORR_X: u32 = 0xAA00_0000;
const ORR_W: u32 = 0x2A00_0000;
const EOR_X: u32 = 0xCA00_0000;
const EOR_W: u32 = 0x4A00_0000;
const MUL_X: u32 = 0x9B00_7C00; // madd with a zero addend
const MUL_W: u32 = 0x1B00_7C00;
const LSLV_X: u32 = 0x9AC0_2000;
const LSLV_W: u32 = 0x1AC0_2000;
const LSRV_X: u32 = 0x9AC0_2400;
const LSRV_W: u32 = 0x1AC0_2400;
const ASRV_X: u32 = 0x9AC0_2800;
const ASRV_W: u32 = 0x1AC0_2800;
const RORV_X: u32 = 0x9AC0_2C00;
const RORV_W: u32 = 0x1AC0_2C00;
const UDIV_X: u32 = 0x9AC0_0800;
const UDIV_W: u32 = 0x1AC0_0800;
const SDIV_X: u32 = 0x9AC0_0C00;
const SDIV_W: u32 = 0x1AC0_0C00;
const CSEL_X: u32 = 0x9A80_0000;

/// `msub rd, rn, rm, ra`: `rd = ra - rn * rm`, the remainder from a quotient.
fn msub(wide: bool, rd: u32, rn: u32, rm: u32, ra: u32) -> u32 {
    let base = if wide { 0x9B00_8000 } else { 0x1B00_8000 };
    base | (rm << 16) | (ra << 10) | (rn << 5) | rd
}

/// `cmp rn, rm` (`subs` into the zero register).
fn cmp(wide: bool, rn: u32, rm: u32) -> u32 {
    let base = if wide { 0xEB00_001F } else { 0x6B00_001F };
    base | (rm << 16) | (rn << 5)
}

/// `cmp rn, #0`.
fn cmp_zero(wide: bool, rn: u32) -> u32 {
    let base = if wide { 0xF100_001F } else { 0x7100_001F };
    base | (rn << 5)
}

/// `cmn rn, #1`: sets the zero flag exactly when `rn` is minus one.
fn cmn_one(wide: bool, rn: u32) -> u32 {
    let base = if wide { 0xB100_041F } else { 0x3100_041F };
    base | (rn << 5)
}

/// `ccmp rn, rm, #nzcv, cond`: compares when `cond` holds, else sets the flags to `nzcv`.
fn ccmp(wide: bool, rn: u32, rm: u32, nzcv: u32, cond: u32) -> u32 {
    let base = if wide { 0xFA40_0000 } else { 0x7A40_0000 };
    base | (rm << 16) | (cond << 12) | (rn << 5) | nzcv
}

/// `cset rd, cond`: one if the condition holds, else zero, as `csinc rd, zr, zr, !cond`.
fn cset(wide: bool, rd: u32, cond: u32) -> u32 {
    let base = if wide { 0x9A9F_07E0 } else { 0x1A9F_07E0 };
    base | ((cond ^ 1) << 12) | rd
}

/// `clz rd, rn` and `rbit rd, rn`, counted over the whole register.
fn clz(wide: bool, rd: u32, rn: u32) -> u32 {
    let base = if wide { 0xDAC0_1000 } else { 0x5AC0_1000 };
    base | (rn << 5) | rd
}

fn rbit(wide: bool, rd: u32, rn: u32) -> u32 {
    let base = if wide { 0xDAC0_0000 } else { 0x5AC0_0000 };
    base | (rn << 5) | rd
}

/// `lsr rd, rn, #shift` and `lsl rd, rn, #shift` on 64 bits, as `ubfm`.
fn lsr_x(rd: u32, rn: u32, shift: u32) -> u32 {
    0xD340_FC00 | (shift << 16) | (rn << 5) | rd
}

fn lsl_x(rd: u32, rn: u32, shift: u32) -> u32 {
    0xD340_0000 | (((64 - shift) % 64) << 16) | ((63 - shift) << 10) | (rn << 5) | rd
}

/// `sxtb`, `sxth` and `sxtw` on 64 bits: sign-extend the low bits of `rn`.
fn sxt(bits: u32, rd: u32, rn: u32) -> u32 {
    let base = match bits {
        8 => 0x9340_1C00,
        16 => 0x9340_3C00,
        _ => 0x9340_7C00,
    };
    base | (rn << 5) | rd
}

/// `movz` and `movk` on 64 and 32 bits, with the halfword `hw` (shifted by `16 * hw`).
fn movz(wide: bool, rd: u32, imm16: u32, hw: u32) -> u32 {
    let base = if wide { 0xD280_0000 } else { 0x5280_0000 };
    base | (hw << 21) | (imm16 << 5) | rd
}

fn movk(wide: bool, rd: u32, imm16: u32, hw: u32) -> u32 {
    let base = if wide { 0xF280_0000 } else { 0x7280_0000 };
    base | (hw << 21) | (imm16 << 5) | rd
}

/// `movn x<rd>, #imm16`: all ones except the shifted halfword.
fn movn_x(rd: u32, imm16: u32, hw: u32) -> u32 {
    0x9280_0000 | (hw << 21) | (imm16 << 5) | rd
}

/// `add rd, sp, #imm` and `sub sp, sp, #imm`, with `imm` below 4096 bytes.
fn add_from_sp(rd: u32, imm: u32) -> u32 {
    0x9100_03E0 | (imm << 10) | rd
}

fn sub_sp(imm: u32) -> u32 {
    0xD100_03FF | (imm << 10)
}

/// `add rd, rn, #imm` on 64 bits.
fn add_x_imm(rd: u32, rn: u32, imm: u32) -> u32 {
    0x9100_0000 | (imm << 10) | (rn << 5) | rd
}

/// `sub rd, rn, #imm` on 64 bits.
fn sub_x_imm(rd: u32, rn: u32, imm: u32) -> u32 {
    0xD100_0000 | (imm << 10) | (rn << 5) | rd
}

/// A load or store with an unsigned, scaled offset. `base` is the instruction with a zero offset, and
/// `shift` is the log2 of the access size, which the offset must be a multiple of.
fn mem(base: u32, shift: u32, rt: u32, rn: u32, offset: u32) -> u32 {
    debug_assert_eq!(offset % (1 << shift), 0, "an unaligned access offset");
    base | ((offset >> shift) << 10) | (rn << 5) | rt
}

fn ldr_x(rt: u32, rn: u32, offset: u32) -> u32 {
    mem(0xF940_0000, 3, rt, rn, offset)
}

fn ldr_w(rt: u32, rn: u32, offset: u32) -> u32 {
    mem(0xB940_0000, 2, rt, rn, offset)
}

fn str_x(rt: u32, rn: u32, offset: u32) -> u32 {
    mem(0xF900_0000, 3, rt, rn, offset)
}

fn str_w(rt: u32, rn: u32, offset: u32) -> u32 {
    mem(0xB900_0000, 2, rt, rn, offset)
}

/// `ldr x<rt>, [x<rn>, x<rm>, lsl #3]`: an entry of a table of words.
fn ldr_x_indexed(rt: u32, rn: u32, rm: u32) -> u32 {
    0xF860_7800 | (rm << 16) | (rn << 5) | rt
}

// Floats. The double forms come first; the single forms share their low bits.
const FADD_D: u32 = 0x1E60_2800;
const FADD_S: u32 = 0x1E20_2800;
const FSUB_D: u32 = 0x1E60_3800;
const FSUB_S: u32 = 0x1E20_3800;
const FMUL_D: u32 = 0x1E60_0800;
const FMUL_S: u32 = 0x1E20_0800;
const FDIV_D: u32 = 0x1E60_1800;
const FDIV_S: u32 = 0x1E20_1800;
const FMAX_D: u32 = 0x1E60_4800;
const FMAX_S: u32 = 0x1E20_4800;
const FMIN_D: u32 = 0x1E60_5800;
const FMIN_S: u32 = 0x1E20_5800;
const FCMP_D: u32 = 0x1E60_2000;
const FCMP_S: u32 = 0x1E20_2000;
const FSQRT_D: u32 = 0x1E61_C000;
const FSQRT_S: u32 = 0x1E21_C000;
/// `frintn`, `frintp`, `frintm`, `frintz`: to nearest even, up, down, toward zero.
const FRINTN_D: u32 = 0x1E64_4000;
const FRINTP_D: u32 = 0x1E64_C000;
const FRINTM_D: u32 = 0x1E65_4000;
const FRINTZ_D: u32 = 0x1E65_C000;
const FRINTN_S: u32 = 0x1E24_4000;
const FRINTP_S: u32 = 0x1E24_C000;
const FRINTM_S: u32 = 0x1E25_4000;
const FRINTZ_S: u32 = 0x1E25_C000;
/// `fcvt d0, s0` and `fcvt s0, d0`.
const FCVT_DS: u32 = 0x1E22_C000;
const FCVT_SD: u32 = 0x1E62_4000;
/// `fmov d<rd>, x<rn>` and `fmov x<rd>, d<rn>`, and the 32-bit forms with `s` and `w`.
const FMOV_DX: u32 = 0x9E67_0000;
const FMOV_XD: u32 = 0x9E66_0000;
const FMOV_SW: u32 = 0x1E27_0000;
const FMOV_WS: u32 = 0x1E26_0000;
/// `scvtf` and `ucvtf` into a float from a word or a doubleword.
const SCVTF_DW: u32 = 0x1E62_0000;
const SCVTF_DX: u32 = 0x9E62_0000;
const SCVTF_SW: u32 = 0x1E22_0000;
const SCVTF_SX: u32 = 0x9E22_0000;
const UCVTF_DW: u32 = 0x1E63_0000;
const UCVTF_DX: u32 = 0x9E63_0000;
const UCVTF_SW: u32 = 0x1E23_0000;
const UCVTF_SX: u32 = 0x9E23_0000;
/// `fcvtzs` and `fcvtzu` from a double into a word or a doubleword, truncating toward zero.
const FCVTZS_WD: u32 = 0x1E78_0000;
const FCVTZS_XD: u32 = 0x9E78_0000;
const FCVTZU_WD: u32 = 0x1E79_0000;
const FCVTZU_XD: u32 = 0x9E79_0000;

// Vector forms for the population count, on the low eight bytes of `v0`.
/// `cnt v0.8b, v0.8b`: the bits set in each byte.
const CNT_V0: u32 = 0x0E20_5800;
/// `addv b0, v0.8b`: the sum of the bytes.
const ADDV_B0: u32 = 0x0E31_B800;
/// `umov w<rd>, v0.b[0]`.
const UMOV_W_B0: u32 = 0x0E01_3C00;

// Control.
const B: u32 = 0x1400_0000;
const B_COND: u32 = 0x5400_0000;
const CBZ_X: u32 = 0xB400_0000;
const CBNZ_W: u32 = 0x3500_0000;
const BLR: u32 = 0xD63F_0000;
const RET: u32 = 0xD65F_03C0;
/// `stp x29, x30, [sp, #-16]!`, `mov x29, sp`, and the epilogue's `mov sp, x29` and
/// `ldp x29, x30, [sp], #16`.
const STP_FP_LR_PRE: u32 = 0xA9BF_7BFD;
const MOV_FP_SP: u32 = 0x9100_03FD;
const MOV_SP_FP: u32 = 0x9100_03BF;
const LDP_FP_LR_POST: u32 = 0xA8C1_7BFD;

/// How a branch's displacement is stored: `B` takes 26 bits from bit 0, and the conditional and
/// compare-and-branch forms take 19 bits from bit 5.
#[derive(Clone, Copy)]
enum Fixup {
    Branch26,
    Branch19,
}

/// The call areas of a function: the first slot of the argument entries, of the result entries, and
/// of the indirect call's entry point, with the signature ids as the table stores them.
struct Calls {
    block: u32,
    results_block: u32,
    entry: u32,
    type_ids: Vec<i32>,
}

/// The emitter for one function: its words, the labels of its blocks and of the inline sequences,
/// and the branches to patch once every label is placed.
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

    /// An unconditional branch to `label`.
    fn jump(&mut self, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::Branch26));
        self.push(B);
    }

    /// A branch to `label` whose instruction is `word` with its displacement left to patch: a
    /// conditional branch (`0x5400_0000 | cond`), or a compare-and-branch with its register.
    fn branch19(&mut self, word: u32, label: usize) {
        self.fixups.push((self.words.len(), label, Fixup::Branch19));
        self.push(word);
    }

    /// Resolve every branch and return the code, little-endian.
    fn finish(mut self) -> Result<Vec<u8>, JitError> {
        for (position, label, fixup) in std::mem::take(&mut self.fixups) {
            let target = self.labels[label]
                .ok_or_else(|| JitError::Unsupported("a branch to an unbound label".into()))?;
            let distance = target as i64 - position as i64;
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
        Ok(self
            .words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect())
    }

    /// Load a 64-bit constant into `rd`: a `movz`, then a `movk` for each other nonzero halfword.
    fn load_imm64(&mut self, rd: u32, value: u64) {
        if value == 0 {
            self.push(movz(true, rd, 0, 0));
            return;
        }
        let mut first = true;
        for hw in 0..4 {
            let part = ((value >> (16 * hw)) & 0xFFFF) as u32;
            if part == 0 {
                continue;
            }
            if first {
                self.push(movz(true, rd, part, hw));
                first = false;
            } else {
                self.push(movk(true, rd, part, hw));
            }
        }
    }

    /// Load a 32-bit constant into `rd` as a word, which zero-extends it into the register.
    fn load_imm32(&mut self, rd: u32, value: u32) {
        self.push(movz(false, rd, value & 0xFFFF, 0));
        if value >> 16 != 0 {
            self.push(movk(false, rd, value >> 16, 1));
        }
    }

    /// Load a slot's word into `rt`, the whole word.
    fn load_x(&mut self, rt: u32, slot: Slot) {
        self.push(ldr_x(rt, SP, 8 * slot));
    }

    /// Load a slot's low 32 bits into `rt`, zero-extended.
    fn load_w(&mut self, rt: u32, slot: Slot) {
        self.push(ldr_w(rt, SP, 8 * slot));
    }

    /// Store `rt`, the whole word, into a slot.
    fn store_x(&mut self, rt: u32, slot: Slot) {
        self.push(str_x(rt, SP, 8 * slot));
    }

    /// Load a word of `width` from a slot: a 32-bit value zero-extended, or the whole word.
    fn load_word(&mut self, rt: u32, slot: Slot, width: WordWidth) {
        match width {
            WordWidth::Low32 => self.load_w(rt, slot),
            WordWidth::Full64 => self.load_x(rt, slot),
        }
    }

    /// Load the context pointer into `x13`.
    fn load_context(&mut self) {
        self.push(ldr_x(X13, SP, 8 * self.context));
    }

    /// The epilogue: restore `sp` from the frame pointer, pop the frame pointer and link register,
    /// and return.
    fn epilogue(&mut self) {
        self.push(MOV_SP_FP);
        self.push(LDP_FP_LR_POST);
        self.push(RET);
    }

    /// Return zero from the function, which is what a trap and a failed call return.
    fn return_zero(&mut self) {
        self.push(movz(true, X0, 0, 0));
        self.epilogue();
    }

    /// Write `status` into the context and return zero: the trap sequence.
    fn raise(&mut self, status: u32) {
        self.load_context();
        self.load_imm64(X14, u64::from(status));
        self.push(str_x(X14, X13, STATUS_OFFSET));
        self.return_zero();
    }

    /// Trap with `status` when the flags satisfy `condition`; otherwise continue. The branch skips
    /// the trap sequence when the condition does not hold, so the sequence stays inline.
    fn trap_if(&mut self, condition: u32, status: u32) {
        let skip = self.label();
        self.branch19(B_COND | (condition ^ 1), skip);
        self.raise(status);
        self.bind(skip);
    }

    /// Compare the two float operands in `a` and `b`, of `precision`.
    fn fcmp(&mut self, precision: Precision, a: u32, b: u32) {
        let base = match precision {
            Precision::Double => FCMP_D,
            Precision::Single => FCMP_S,
        };
        self.push(base | (b << 16) | (a << 5));
    }

    /// Load a float slot into the float register `v`: through `x9` for an `f64`, and `w9` for an
    /// `f32`, which reads its low 32 bits.
    fn float_in(&mut self, v: u32, slot: Slot, precision: Precision) {
        match precision {
            Precision::Double => {
                self.load_x(X9, slot);
                self.push(FMOV_DX | (X9 << 5) | v);
            }
            Precision::Single => {
                self.load_w(X9, slot);
                self.push(FMOV_SW | (X9 << 5) | v);
            }
        }
    }

    /// Store the float register `v` into a slot as its bits. An `f32` is stored zero-extended.
    fn float_out(&mut self, v: u32, slot: Slot, precision: Precision) {
        match precision {
            Precision::Double => self.push(FMOV_XD | (v << 5) | X9),
            Precision::Single => self.push(FMOV_WS | (v << 5) | X9),
        }
        self.store_x(X9, slot);
    }

    /// Replace a NaN in `v` with the canonical NaN that Micro gives. An unordered compare of `v` with
    /// itself sets the overflow flag exactly for a NaN, so the replacement is skipped otherwise.
    fn canonicalize(&mut self, v: u32, precision: Precision) {
        self.fcmp(precision, v, v);
        let ordered = self.label();
        self.branch19(B_COND | VC, ordered);
        match precision {
            Precision::Double => {
                self.load_imm64(X9, CANONICAL_NAN_F64);
                self.push(FMOV_DX | (X9 << 5) | v);
            }
            Precision::Single => {
                self.load_imm32(X9, 0x7fc0_0000);
                self.push(FMOV_SW | (X9 << 5) | v);
            }
        }
        self.bind(ordered);
    }

    /// After a callee or the host returns: if it left a status in the context, return zero from this
    /// function, as a trap does; otherwise continue.
    fn return_if_status(&mut self) {
        self.load_context();
        self.push(ldr_x(X9, X13, STATUS_OFFSET));
        let ok = self.label();
        self.branch19(CBZ_X | X9, ok);
        self.return_zero();
        self.bind(ok);
    }

    /// Check an access of `width` bytes at the 32-bit address in `address` plus `offset`, trapping
    /// past the memory's end, and leave the address in `x12`. The end is computed in 64 bits, so
    /// it cannot wrap.
    fn address(&mut self, address: Slot, offset: u32, width: u32) {
        self.load_w(X10, address);
        self.load_imm64(X11, u64::from(offset));
        self.push(dp(ADD_X, X10, X10, X11));
        self.push(add_x_imm(X11, X10, width));
        self.load_context();
        self.push(ldr_x(X12, X13, MEMORY_LENGTH_OFFSET));
        self.push(cmp(true, X11, X12));
        self.trap_if(HI, STATUS_MEMORY_OUT_OF_BOUNDS);
        self.push(ldr_x(X12, X13, MEMORY_BASE_OFFSET));
        self.push(dp(ADD_X, X12, X12, X10));
    }

    /// The second half of a call: count the frame off, and if the callee left a status, return zero
    /// without a result. Otherwise store the callee's result, if there is one.
    fn leave(&mut self, dst: &[Slot], calls: &Calls) {
        self.load_context();
        self.push(ldr_x(X9, X13, DEPTH_OFFSET));
        self.push(sub_x_imm(X9, X9, 1));
        self.push(str_x(X9, X13, DEPTH_OFFSET));
        self.return_if_status();
        match dst {
            [] => {}
            // A single result is in x0.
            [only] => self.store_x(X0, *only),
            // Two or more results were written to the argument block, entry `j` at `8 * j` from its
            // pointer, which is slot `block + j`.
            _ => {
                for (index, slot) in dst.iter().enumerate() {
                    self.load_x(X9, calls.block + index as u32);
                    self.store_x(X9, *slot);
                }
            }
        }
    }

    /// Make the `i32` bit-count `op` of `a`, the 32-bit form, in `x9`.
    fn unary(&mut self, op: IntUnary, wide: bool, a: Slot) {
        if wide {
            self.load_x(X9, a);
        } else {
            self.load_w(X9, a);
        }
        match op {
            IntUnary::Clz => self.push(clz(wide, X9, X9)),
            IntUnary::Ctz => {
                self.push(rbit(wide, X9, X9));
                self.push(clz(wide, X9, X9));
            }
            IntUnary::Popcnt => {
                // The population count has no general-register form, so the value goes through the
                // vector unit: a per-byte count, summed across the eight bytes.
                self.push(FMOV_DX | (X9 << 5));
                self.push(CNT_V0);
                self.push(ADDV_B0);
                self.push(UMOV_W_B0 | X9);
            }
        }
    }

    /// `x9 = dividend op divisor` for `a` and `b` in `x9` and `x10` (or their 32-bit forms), trapping
    /// on a zero divisor, and on the signed overflow of the minimum by minus one.
    fn divide(&mut self, op: DivOp, wide: bool, minimum: u64) {
        let (udiv, sdiv) = if wide {
            (UDIV_X, SDIV_X)
        } else {
            (UDIV_W, SDIV_W)
        };
        self.push(cmp_zero(wide, X10));
        self.trap_if(EQ, STATUS_DIVIDE_BY_ZERO);
        match op {
            DivOp::DivU => self.push(dp(udiv, X9, X9, X10)),
            DivOp::RemU => {
                self.push(dp(udiv, X11, X9, X10));
                self.push(msub(wide, X9, X11, X10, X9));
            }
            DivOp::DivS => {
                // The minimum divided by minus one overflows, and it traps: the divisor is minus
                // one and the dividend the minimum.
                if wide {
                    self.load_imm64(X11, minimum);
                } else {
                    self.load_imm32(X11, minimum as u32);
                }
                self.push(cmn_one(wide, X10));
                self.push(ccmp(wide, X9, X11, 0, EQ));
                self.trap_if(EQ, STATUS_OVERFLOW);
                self.push(dp(sdiv, X9, X9, X10));
            }
            DivOp::RemS => {
                // The remainder of the minimum by minus one is zero, which the multiply-subtract gives
                // without trapping, as the hardware's quotient is the minimum.
                self.push(dp(sdiv, X11, X9, X10));
                self.push(msub(wide, X9, X11, X10, X9));
            }
        }
    }

    /// The machine-IR operation `op`, emitted from its slots.
    fn op(&mut self, op: &MirOp, calls: &Calls) {
        match op {
            MirOp::Call {
                function,
                args,
                dst,
            } => {
                self.write_arguments(args, calls);
                self.count_frame();
                self.set_registers(calls);
                self.load_context();
                self.push(ldr_x(X9, X13, FUNCTIONS_OFFSET));
                self.load_imm64(X10, u64::from(*function));
                self.push(ldr_x_indexed(X9, X9, X10));
                self.push(BLR | (X9 << 5));
                self.leave(dst, calls);
            }
            MirOp::CallIndirect {
                type_index,
                index,
                args,
                dst,
            } => {
                self.call_indirect(*type_index, *index, args, dst, calls);
            }
            MirOp::CallHost { import, args, dsts } => {
                for (index, (arg, width)) in args.iter().enumerate() {
                    self.load_word(X9, *arg, *width);
                    self.store_x(X9, calls.block + index as u32);
                }
                // The trampoline takes the context, the import index, the argument words and the
                // result words.
                let context = self.context;
                self.load_x(X0, context);
                self.load_imm64(X1, u64::from(*import));
                self.push(add_from_sp(X2, 8 * calls.block));
                self.push(add_from_sp(X3, 8 * calls.results_block));
                self.push(ldr_x(X9, X0, HOST_CALL_OFFSET));
                self.push(BLR | (X9 << 5));
                self.return_if_status();
                for (index, (dst, width)) in dsts.iter().enumerate() {
                    self.load_word(X9, calls.results_block + index as u32, *width);
                    self.store_x(X9, *dst);
                }
            }
            MirOp::GlobalGet { dst, index, width } => {
                self.global_address(*index);
                match width {
                    WordWidth::Low32 => self.push(ldr_w(X9, X12, 0)),
                    WordWidth::Full64 => self.push(ldr_x(X9, X12, 0)),
                }
                self.store_x(X9, *dst);
            }
            MirOp::GlobalSet { index, src, width } => {
                self.load_word(X9, *src, *width);
                self.global_address(*index);
                self.push(str_x(X9, X12, 0));
            }
            MirOp::MemorySize { dst } => {
                self.load_context();
                self.push(ldr_x(X9, X13, MEMORY_LENGTH_OFFSET));
                self.push(lsr_x(X9, X9, 16));
                self.store_x(X9, *dst);
            }
            MirOp::MemoryGrow { dst, delta } => {
                // The new length is the old one plus the pages in bytes. It is kept only if it is
                // within the capacity; otherwise the result is -1 and the length is left alone.
                self.load_context();
                self.push(ldr_x(X9, X13, MEMORY_LENGTH_OFFSET));
                self.load_w(X10, *delta);
                self.push(lsl_x(X10, X10, 16));
                self.push(dp(ADD_X, X10, X10, X9));
                self.push(ldr_x(X11, X13, MEMORY_CAPACITY_OFFSET));
                self.push(cmp(true, X10, X11));
                let fail = self.label();
                let done = self.label();
                self.branch19(B_COND | HI, fail);
                self.push(str_x(X10, X13, MEMORY_LENGTH_OFFSET));
                self.push(lsr_x(X9, X9, 16));
                self.jump(done);
                self.bind(fail);
                self.push(movn_x(X9, 0, 0));
                self.bind(done);
                self.store_x(X9, *dst);
            }
            MirOp::Load {
                op,
                dst,
                address,
                offset,
            } => {
                self.address(*address, *offset, op.width());
                // Each form fills the whole of the register: the 32-bit loads zero-extend, and the
                // signed forms sign-extend to 64 bits.
                let access = match op {
                    LoadOp::Quad => ldr_x(X9, X12, 0),
                    LoadOp::Word => ldr_w(X9, X12, 0),
                    LoadOp::SignedWord => mem(0xB980_0000, 2, X9, X12, 0),
                    LoadOp::SignedByte => mem(0x3980_0000, 0, X9, X12, 0),
                    LoadOp::UnsignedByte => mem(0x3940_0000, 0, X9, X12, 0),
                    LoadOp::SignedHalf => mem(0x7980_0000, 1, X9, X12, 0),
                    LoadOp::UnsignedHalf => mem(0x7940_0000, 1, X9, X12, 0),
                };
                self.push(access);
                self.store_x(X9, *dst);
            }
            MirOp::Store {
                op,
                address,
                value,
                offset,
            } => {
                self.address(*address, *offset, op.width());
                self.load_x(X9, *value);
                let access = match op {
                    StoreOp::Quad => str_x(X9, X12, 0),
                    StoreOp::Word => str_w(X9, X12, 0),
                    StoreOp::Byte => mem(0x3900_0000, 0, X9, X12, 0),
                    StoreOp::Half => mem(0x7900_0000, 1, X9, X12, 0),
                };
                self.push(access);
            }
            MirOp::Divide { op, dst, a, b } => {
                self.load_w(X9, *a);
                self.load_w(X10, *b);
                self.divide(*op, false, 0x8000_0000);
                self.store_x(X9, *dst);
            }
            MirOp::Divide64 { op, dst, a, b } => {
                self.load_x(X9, *a);
                self.load_x(X10, *b);
                self.divide(*op, true, i64::MIN as u64);
                self.store_x(X9, *dst);
            }
            MirOp::Const { dst, value } => {
                self.load_imm32(X9, *value as u32);
                self.store_x(X9, *dst);
            }
            MirOp::Const64 { dst, value } => {
                self.load_imm64(X9, *value as u64);
                self.store_x(X9, *dst);
            }
            MirOp::Move { dst, src } => {
                self.load_x(X9, *src);
                self.store_x(X9, *dst);
            }
            MirOp::Bin { op, dst, a, b } => {
                self.binary(*op, false, *a, *b);
                self.store_x(X9, *dst);
            }
            MirOp::Bin64 { op, dst, a, b } => {
                self.binary(*op, true, *a, *b);
                self.store_x(X9, *dst);
            }
            MirOp::Compare { cond, dst, a, b } => {
                self.load_w(X9, *a);
                self.load_w(X10, *b);
                self.push(cmp(false, X9, X10));
                self.push(cset(false, X9, condition(*cond)));
                self.store_x(X9, *dst);
            }
            MirOp::Compare64 { cond, dst, a, b } => {
                self.load_x(X9, *a);
                self.load_x(X10, *b);
                self.push(cmp(true, X9, X10));
                self.push(cset(true, X9, condition(*cond)));
                self.store_x(X9, *dst);
            }
            MirOp::Eqz { dst, a } => {
                self.load_w(X9, *a);
                self.push(cmp_zero(false, X9));
                self.push(cset(false, X9, EQ));
                self.store_x(X9, *dst);
            }
            MirOp::Eqz64 { dst, a } => {
                self.load_x(X9, *a);
                self.push(cmp_zero(true, X9));
                self.push(cset(true, X9, EQ));
                self.store_x(X9, *dst);
            }
            MirOp::Unary { op, dst, a } => {
                self.unary(*op, false, *a);
                self.store_x(X9, *dst);
            }
            MirOp::Unary64 { op, dst, a } => {
                self.unary(*op, true, *a);
                self.store_x(X9, *dst);
            }
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
            } => {
                self.float_in(0, *a, *precision);
                self.float_in(1, *b, *precision);
                self.fcmp(*precision, 0, 1);
                self.push(cset(false, X9, float_condition(*cond)));
                self.store_x(X9, *dst);
            }
            MirOp::FloatUnary {
                precision,
                op,
                dst,
                a,
            } => self.float_unary(*op, *precision, *dst, *a),
            MirOp::Narrow { dst, src } => {
                self.load_w(X9, *src);
                self.store_x(X9, *dst);
            }
            MirOp::SignExtend { dst, src, bits } => {
                self.load_x(X9, *src);
                self.push(sxt(*bits, X9, X9));
                self.store_x(X9, *dst);
            }
            MirOp::IntToFloat { to, from, dst, src } => {
                self.int_to_float(*to, *from, *dst, *src);
            }
            MirOp::FloatToFloat { to, dst, src } => {
                match to {
                    Precision::Single => {
                        self.float_in(0, *src, Precision::Double);
                        self.push(FCVT_SD);
                    }
                    Precision::Double => {
                        self.float_in(0, *src, Precision::Single);
                        self.push(FCVT_DS);
                    }
                }
                self.canonicalize(0, *to);
                self.float_out(0, *dst, *to);
            }
            MirOp::FloatToInt { from, to, dst, src } => {
                self.float_to_int(*from, *to, *dst, *src);
            }
            MirOp::Select { dst, cond, a, b } => {
                self.load_w(X11, *cond);
                self.load_x(X9, *a);
                self.load_x(X10, *b);
                self.push(cmp_zero(false, X11));
                self.push(CSEL_X | (X10 << 16) | (NE << 12) | (X9 << 5) | X9);
                self.store_x(X9, *dst);
            }
        }
    }
}

impl Asm {
    /// Write the arguments of a call to their entries, as whole words, in ascending order.
    fn write_arguments(&mut self, args: &[Slot], calls: &Calls) {
        for (index, arg) in args.iter().enumerate() {
            self.load_x(X9, *arg);
            self.store_x(X9, calls.block + index as u32);
        }
    }

    /// Count one more frame against the depth limit, and trap if that is past it.
    fn count_frame(&mut self) {
        self.load_context();
        self.push(ldr_x(X9, X13, DEPTH_OFFSET));
        self.push(ldr_x(X10, X13, DEPTH_LIMIT_OFFSET));
        self.push(cmp(true, X9, X10));
        self.trap_if(HS, STATUS_CALL_DEPTH);
        self.push(add_x_imm(X9, X9, 1));
        self.push(str_x(X9, X13, DEPTH_OFFSET));
    }

    /// Set `x0` to the pointer to the arguments and `x1` to the context, the callee's two arguments.
    fn set_registers(&mut self, calls: &Calls) {
        self.push(add_from_sp(X0, 8 * calls.block));
        let context = self.context;
        self.load_x(X1, context);
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
        // x10 = the index, zero-extended from its slot.
        self.load_w(X10, index);
        self.load_context();
        self.push(ldr_x(X11, X13, TABLE_LEN_OFFSET));
        self.push(cmp(true, X10, X11));
        self.trap_if(HS, STATUS_TABLE_OUT_OF_BOUNDS);
        // x9 = the slot's signature id, which is -1 for a null slot.
        self.push(ldr_x(X12, X13, TABLE_TYPES_OFFSET));
        self.push(ldr_x_indexed(X9, X12, X10));
        self.push(cmn_one(true, X9));
        self.trap_if(EQ, STATUS_NULL_REFERENCE);
        let expected = calls.type_ids[type_index as usize];
        self.load_imm64(X14, i64::from(expected) as u64);
        self.push(cmp(true, X9, X14));
        self.trap_if(NE, STATUS_INDIRECT_TYPE_MISMATCH);
        // The slot's entry point, kept in the entry slot while the registers are set.
        self.push(ldr_x(X12, X13, TABLE_ENTRIES_OFFSET));
        self.push(ldr_x_indexed(X9, X12, X10));
        self.store_x(X9, calls.entry);
        self.count_frame();
        self.set_registers(calls);
        self.load_x(X9, calls.entry);
        self.push(BLR | (X9 << 5));
        self.leave(dst, calls);
    }

    /// Check that global `index` is inside the globals the caller provided, and leave the address of
    /// its entry in `x12`. An index past the end traps, so code called without its globals cannot read
    /// past them.
    fn global_address(&mut self, index: u32) {
        self.load_context();
        self.push(ldr_x(X11, X13, GLOBALS_LENGTH_OFFSET));
        self.load_imm64(X10, u64::from(index));
        self.push(cmp(true, X10, X11));
        self.trap_if(HS, STATUS_GLOBAL_OUT_OF_BOUNDS);
        self.push(ldr_x(X12, X13, GLOBALS_OFFSET));
        self.load_imm64(X10, 8 * u64::from(index));
        self.push(dp(ADD_X, X12, X12, X10));
    }

    /// `dst = a op b` on `x9` and `x10`: the 32-bit forms for `i32` and the 64-bit forms for `i64`.
    /// Shift and rotation counts are taken modulo the width, as the hardware takes them.
    fn binary(&mut self, op: BinOp, wide: bool, a: Slot, b: Slot) {
        if wide {
            self.load_x(X9, a);
            self.load_x(X10, b);
        } else {
            self.load_w(X9, a);
            self.load_w(X10, b);
        }
        let pick = |narrow: u32, long: u32| if wide { long } else { narrow };
        match op {
            BinOp::Add => self.push(dp(pick(ADD_W, ADD_X), X9, X9, X10)),
            BinOp::Sub => self.push(dp(pick(SUB_W, SUB_X), X9, X9, X10)),
            BinOp::And => self.push(dp(pick(AND_W, AND_X), X9, X9, X10)),
            BinOp::Or => self.push(dp(pick(ORR_W, ORR_X), X9, X9, X10)),
            BinOp::Xor => self.push(dp(pick(EOR_W, EOR_X), X9, X9, X10)),
            BinOp::Mul => self.push(dp(pick(MUL_W, MUL_X), X9, X9, X10)),
            BinOp::Shl => self.push(dp(pick(LSLV_W, LSLV_X), X9, X9, X10)),
            BinOp::ShrS => self.push(dp(pick(ASRV_W, ASRV_X), X9, X9, X10)),
            BinOp::ShrU => self.push(dp(pick(LSRV_W, LSRV_X), X9, X9, X10)),
            BinOp::Rotr => self.push(dp(pick(RORV_W, RORV_X), X9, X9, X10)),
            BinOp::Rotl => {
                // Rotating left by n is rotating right by the width minus n, taken modulo the width.
                self.push(dp(pick(SUB_W, SUB_X), X10, ZR, X10));
                self.push(dp(pick(RORV_W, RORV_X), X9, X9, X10));
            }
        }
    }

    /// `dst = a op b` for a float: the arithmetic and the `min` and `max` rules, then the canonical
    /// NaN for a NaN result. `copysign` combines the bits, so a NaN keeps its payload.
    fn float_binary(&mut self, op: FloatOp, precision: Precision, dst: Slot, a: Slot, b: Slot) {
        let wide = precision == Precision::Double;
        if op == FloatOp::Copysign {
            // The sign of `b` and the magnitude and payload of `a`, combined on the bits.
            if wide {
                self.load_x(X9, a);
                self.load_x(X10, b);
                self.load_imm64(X11, 0x8000_0000_0000_0000);
            } else {
                self.load_w(X9, a);
                self.load_w(X10, b);
                self.load_imm32(X11, 0x8000_0000);
            }
            let (and, bic, orr) = if wide {
                (AND_X, BIC_X, ORR_X)
            } else {
                (AND_W, BIC_W, ORR_W)
            };
            self.push(dp(and, X10, X10, X11));
            self.push(dp(bic, X9, X9, X11));
            self.push(dp(orr, X9, X9, X10));
            self.store_x(X9, dst);
            return;
        }
        self.float_in(0, a, precision);
        self.float_in(1, b, precision);
        let word = match (op, wide) {
            (FloatOp::Add, true) => FADD_D,
            (FloatOp::Add, false) => FADD_S,
            (FloatOp::Sub, true) => FSUB_D,
            (FloatOp::Sub, false) => FSUB_S,
            (FloatOp::Mul, true) => FMUL_D,
            (FloatOp::Mul, false) => FMUL_S,
            (FloatOp::Div, true) => FDIV_D,
            (FloatOp::Div, false) => FDIV_S,
            // `fmin` and `fmax` order `-0` below `+0`, as Micro's rules do, and a NaN operand gives a
            // NaN that the canonicalization below replaces.
            (FloatOp::Min, true) => FMIN_D,
            (FloatOp::Min, false) => FMIN_S,
            (FloatOp::Max, true) => FMAX_D,
            (FloatOp::Max, false) => FMAX_S,
            (FloatOp::Copysign, _) => unreachable!("handled above"),
        };
        self.push(dp(word, 0, 0, 1));
        self.canonicalize(0, precision);
        self.float_out(0, dst, precision);
    }

    /// `dst = op(a)` for a float. The sign operations work on the bits, so a NaN keeps its payload; the
    /// others give the canonical NaN for a NaN result, through the rounding or root instruction.
    fn float_unary(&mut self, op: FloatUnaryOp, precision: Precision, dst: Slot, a: Slot) {
        let wide = precision == Precision::Double;
        match op {
            FloatUnaryOp::Abs | FloatUnaryOp::Neg => {
                let clear = op == FloatUnaryOp::Abs;
                if wide {
                    self.load_x(X9, a);
                    self.load_imm64(X10, 0x8000_0000_0000_0000);
                    self.push(dp(if clear { BIC_X } else { EOR_X }, X9, X9, X10));
                } else {
                    self.load_w(X9, a);
                    self.load_imm32(X10, 0x8000_0000);
                    self.push(dp(if clear { BIC_W } else { EOR_W }, X9, X9, X10));
                }
                self.store_x(X9, dst);
            }
            FloatUnaryOp::Sqrt
            | FloatUnaryOp::Ceil
            | FloatUnaryOp::Floor
            | FloatUnaryOp::Trunc
            | FloatUnaryOp::Nearest => {
                self.float_in(0, a, precision);
                let word = match (op, wide) {
                    (FloatUnaryOp::Sqrt, true) => FSQRT_D,
                    (FloatUnaryOp::Sqrt, false) => FSQRT_S,
                    (FloatUnaryOp::Ceil, true) => FRINTP_D,
                    (FloatUnaryOp::Ceil, false) => FRINTP_S,
                    (FloatUnaryOp::Floor, true) => FRINTM_D,
                    (FloatUnaryOp::Floor, false) => FRINTM_S,
                    (FloatUnaryOp::Trunc, true) => FRINTZ_D,
                    (FloatUnaryOp::Trunc, false) => FRINTZ_S,
                    (FloatUnaryOp::Nearest, true) => FRINTN_D,
                    (FloatUnaryOp::Nearest, false) => FRINTN_S,
                    (FloatUnaryOp::Abs | FloatUnaryOp::Neg, _) => {
                        unreachable!("handled above")
                    }
                };
                self.push(dp(word, 0, 0, 0));
                self.canonicalize(0, precision);
                self.float_out(0, dst, precision);
            }
        }
    }

    /// `dst = (float) src`: an integer of `from` converted to a float of precision `to`, rounded once
    /// by the hardware's conversion, which takes the 32-bit or 64-bit word as signed or unsigned.
    fn int_to_float(&mut self, to: Precision, from: IntKind, dst: Slot, src: Slot) {
        let wide = matches!(from, IntKind::Signed64 | IntKind::Unsigned64);
        if wide {
            self.load_x(X9, src);
        } else {
            self.load_w(X9, src);
        }
        let signed = matches!(from, IntKind::Signed32 | IntKind::Signed64);
        let word = match (to, signed, wide) {
            (Precision::Double, true, false) => SCVTF_DW,
            (Precision::Double, true, true) => SCVTF_DX,
            (Precision::Double, false, false) => UCVTF_DW,
            (Precision::Double, false, true) => UCVTF_DX,
            (Precision::Single, true, false) => SCVTF_SW,
            (Precision::Single, true, true) => SCVTF_SX,
            (Precision::Single, false, false) => UCVTF_SW,
            (Precision::Single, false, true) => UCVTF_SX,
        };
        self.push(word | (X9 << 5));
        self.float_out(0, dst, to);
    }

    /// `dst = trunc(src)` as an integer of kind `to`, trapping as Micro does. The value is truncated
    /// toward zero first, so a NaN stays a NaN, and an infinity is out of range. A NaN traps, and so
    /// does a truncated value below the lower bound or at or above the upper one, both bounds exact in
    /// `f64`. An unsigned 64-bit value at or above `2^63` has that subtracted before the conversion, and
    /// the top bit set afterwards.
    fn float_to_int(&mut self, from: Precision, to: IntKind, dst: Slot, src: Slot) {
        self.float_in(0, src, from);
        if from == Precision::Single {
            // Widening is exact, so the bounds below hold for an `f32` source too.
            self.push(FCVT_DS);
        }
        self.push(FRINTZ_D);
        self.fcmp(Precision::Double, 0, 0);
        self.trap_if(VS, STATUS_INVALID_CONVERSION);
        let (low, high): (f64, f64) = match to {
            IntKind::Signed32 => (-2_147_483_648.0, 2_147_483_648.0),
            IntKind::Unsigned32 => (0.0, 4_294_967_296.0),
            IntKind::Signed64 => (-9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0),
            IntKind::Unsigned64 => (0.0, 18_446_744_073_709_551_616.0),
        };
        self.load_imm64(X11, low.to_bits());
        self.push(FMOV_DX | (X11 << 5) | 1);
        self.fcmp(Precision::Double, 0, 1);
        self.trap_if(LT, STATUS_INVALID_CONVERSION);
        self.load_imm64(X11, high.to_bits());
        self.push(FMOV_DX | (X11 << 5) | 2);
        self.fcmp(Precision::Double, 0, 2);
        self.trap_if(GE, STATUS_INVALID_CONVERSION);
        match to {
            IntKind::Signed32 => self.push(FCVTZS_WD | X9),
            IntKind::Unsigned32 => self.push(FCVTZU_WD | X9),
            IntKind::Signed64 => self.push(FCVTZS_XD | X9),
            IntKind::Unsigned64 => {
                // Values from `2^63` up are shifted down by `2^63` first, and the bit put back after.
                let small = self.label();
                let done = self.label();
                self.load_imm64(X11, 9_223_372_036_854_775_808.0_f64.to_bits());
                self.push(FMOV_DX | (X11 << 5) | 3);
                self.fcmp(Precision::Double, 0, 3);
                self.branch19(B_COND | LT, small);
                self.push(dp(FSUB_D, 0, 0, 3));
                self.push(FCVTZU_XD | X9);
                self.load_imm64(X10, 1 << 63);
                self.push(dp(EOR_X, X9, X9, X10));
                self.jump(done);
                self.bind(small);
                self.push(FCVTZU_XD | X9);
                self.bind(done);
            }
        }
        self.store_x(X9, dst);
    }
}

/// The condition code for an integer comparison.
fn condition(cond: IntComparison) -> u32 {
    match cond {
        IntComparison::Eq => EQ,
        IntComparison::Ne => NE,
        IntComparison::LtS => LT,
        IntComparison::GeS => GE,
        IntComparison::LeS => LE,
        IntComparison::GtS => GT,
        IntComparison::LtU => LO,
        IntComparison::GeU => HS,
        IntComparison::LeU => LS,
        IntComparison::GtU => HI,
    }
}

/// The condition code for a float comparison. After `fcmp`, an unordered pair sets the flags so that
/// only `ne` holds, which is how a NaN compares.
fn float_condition(cond: FloatComparison) -> u32 {
    match cond {
        FloatComparison::Eq => EQ,
        FloatComparison::Ne => NE,
        FloatComparison::Lt => MI,
        FloatComparison::Gt => GT,
        FloatComparison::Le => LS,
        FloatComparison::Ge => GE,
    }
}

/// Encode a machine-IR function for AArch64, returning the code bytes.
///
/// # Errors
///
/// [`JitError::Unsupported`] for an empty function, a frame too large
/// for the encoder's immediate offsets, or anything [`check_environment`] refuses.
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
    // Every slot is eight bytes, and the frame is rounded to 16 as AAPCS64 requires. The offsets the
    // encoder uses are immediates below 4096 bytes, so a larger frame is refused.
    let frame = (8 * (calls.entry + 1)).div_ceil(16) * 16;
    if frame > 4095 {
        return Err(JitError::Unsupported(
            "a frame too large for the AArch64 encoder".into(),
        ));
    }
    let context = function.slots + 1;
    let mut asm = Asm::new(context, function.blocks.len());

    // stp x29, x30, [sp, #-16]!; mov x29, sp; sub sp, sp, #frame. Then every slot starts at zero, as
    // the machine IR does, so a declared local read before a write is zero.
    asm.push(STP_FP_LR_PRE);
    asm.push(MOV_FP_SP);
    asm.push(sub_sp(frame));
    for slot in 0..function.slots {
        asm.store_x(ZR, slot);
    }
    // The argument pointer and the context pointer are kept in their slots, and each parameter is
    // read from the argument entry it names.
    asm.store_x(X0, function.slots);
    asm.store_x(X1, context);
    for (index, slot) in function.params.iter().enumerate() {
        asm.load_x(X9, function.slots);
        asm.push(ldr_x(X9, X9, 8 * index as u32));
        asm.store_x(X9, *slot);
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
                // The condition is an `i32`, so its low half decides the branch.
                asm.load_w(X9, *cond);
                asm.branch19(CBNZ_W | X9, *then_block);
                asm.jump(*else_block);
            }
            MirTerm::Return(values) => match values.as_slice() {
                [] => asm.return_zero(),
                [value] => {
                    asm.load_x(X0, *value);
                    asm.epilogue();
                }
                // Several values go to the argument block, entry `j` at `8 * j`, and the return
                // register is then zero.
                values => {
                    asm.load_x(X9, function.slots);
                    for (index, value) in values.iter().enumerate() {
                        asm.load_x(X10, *value);
                        asm.push(str_x(X10, X9, 8 * index as u32));
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
    fn a_constant_loads_each_halfword_it_needs() {
        let mut asm = Asm::new(0, 0);
        asm.load_imm64(X9, 0x1234_0000_0000_5678);
        assert_eq!(
            asm.words.len(),
            2,
            "a movz for the low halfword and a movk for the high"
        );
        let mut asm = Asm::new(0, 0);
        asm.load_imm64(X9, 0);
        assert_eq!(asm.words, vec![movz(true, X9, 0, 0)]);
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
        assert_eq!(
            word(0),
            STP_FP_LR_PRE,
            "the frame pointer and link register are saved"
        );
        assert_eq!(word(code.len() - 4), RET, "the function ends with ret");
    }
}
