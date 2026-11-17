// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The machine IR: the stage between the verified IR and native code.
//!
//! The IR names values by SSA id and passes block parameters along edges. Native code has no
//! such names, so this stage gives every value a *slot*, a numbered storage location, and turns
//! the IR's edge arguments into explicit moves. A value keeps the slot numbered like its id; the
//! moves an edge needs go through fresh temporaries first, so an edge whose sources overlap its
//! destinations copies the old values, as the IR's parallel block-parameter semantics requires.
//!
//! A slot holds a 64-bit word. An `i32` value is held in the low 32 bits, and the operations that
//! produce one leave the rest of the word unspecified: every operation that reads an `i32` takes
//! only those 32 bits. An `i64` value is the whole word.
//!
//! This is the first stage of the pipeline `IR -> machine IR -> native code`. The slice here is the
//! `i32` and `i64` integer subsets: constants, locals, the wrapping arithmetic and bitwise
//! operators, shifts, rotations, comparisons, `eqz`, `select`, and signed and unsigned division and
//! remainder, which trap on a zero divisor (and signed division on overflow). The `i32` bit counts
//! and the `i32` loads and stores in memory 0 are included too, each access checked against the
//! memory's length. Also included are the calls: direct, indirect through the module's table, and
//! to host imports, plus the memory size and growth, and the globals. Anything else is refused with
//! [`JitError::Unsupported`] rather than approximated. The native encoders [`crate::mir_x86`], [`crate::mir_aarch64`] and [`crate::mir_riscv64`] read
//! this stage, and its results are checked against Micro both by interpretation here and by running
//! the generated code.

use std::collections::HashMap;

use tpt_wasm_ir::{
    BlockId, FloatComparison, FloatConversion, FloatTrunc, FloatUnary, IntComparison,
    IntConversion, IntUnary, IrFunction, IrInstr, IrModule, MemoryLoad, MemoryStore, Reinterpret,
    SignExtend, Terminator, ValueId,
};
use tpt_wasm_types::{Trap, ValueType};

use crate::{HostCalls, JitError};

/// A numbered storage location: a value, a local, or a temporary of this function.
pub type Slot = u32;

/// The integer operators with WebAssembly's wrapping and masking rules. The same operator exists
/// at both widths: [`MirOp::Bin`] is the `i32` form and [`MirOp::Bin64`] the `i64` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    ShrS,
    ShrU,
    Rotl,
    Rotr,
}

/// The division and remainder operators. They are kept apart from [`BinOp`] because they can trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivOp {
    DivS,
    DivU,
    RemS,
    RemU,
}

/// The width of a float operation. An `f32` lives in the low 32 bits of its slot, and an `f64` in the
/// whole word; the operations read and write exactly those bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Single,
    Double,
}

/// How a word of a value crosses a boundary, the host's or the globals array's: an `i32` or `f32` is
/// its low 32 bits, zero-extended, and an `i64` or `f64` is the whole word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordWidth {
    Low32,
    Full64,
}

/// The integer kinds a conversion reads or produces: a width, and whether the value is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntKind {
    Signed32,
    Unsigned32,
    Signed64,
    Unsigned64,
}

/// The `f64` binary operators. Arithmetic, `min` and `max` give the canonical NaN for a NaN result, as
/// Micro does; `copysign` keeps the payload of its first operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Copysign,
}

/// The `f64` unary operators. The sign operations keep a NaN's payload; the others give the canonical
/// NaN for a NaN result, as Micro does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatUnaryOp {
    Abs,
    Neg,
    Sqrt,
    Ceil,
    Floor,
    Trunc,
    /// Round to the nearest integer, ties to even.
    Nearest,
}

/// The float types the machine IR computes in. Each has its bit layout: the bits a word holds
/// (`MASK`), the sign bit (`SIGN`), and the canonical NaN Micro gives (`CANONICAL`). The operations
/// below are written once over this trait, so both precisions follow the same rules.
trait FloatBits:
    Copy
    + PartialOrd
    + std::ops::Add<Output = Self>
    + std::ops::Sub<Output = Self>
    + std::ops::Mul<Output = Self>
    + std::ops::Div<Output = Self>
{
    const MASK: u64;
    const SIGN: u64;
    const CANONICAL: u64;
    /// The value of the low bits of a slot word.
    fn from_word(word: i64) -> Self;
    /// The bits of the value, within `MASK`.
    fn to_word(self) -> u64;
    /// The value as an `f64`, exactly: both precisions convert without rounding.
    fn to_f64(self) -> f64;
    fn is_nan(self) -> bool;
    fn is_sign_negative(self) -> bool;
    fn is_sign_positive(self) -> bool;
    fn square_root(self) -> Self;
    fn round_up(self) -> Self;
    fn round_down(self) -> Self;
    fn round_toward_zero(self) -> Self;
    fn round_to_even(self) -> Self;
}

impl FloatBits for f64 {
    const MASK: u64 = u64::MAX;
    const SIGN: u64 = 0x8000_0000_0000_0000;
    const CANONICAL: u64 = 0x7ff8_0000_0000_0000;
    fn from_word(word: i64) -> Self {
        f64::from_bits(word as u64)
    }
    fn to_word(self) -> u64 {
        self.to_bits()
    }
    fn to_f64(self) -> f64 {
        self
    }
    fn is_nan(self) -> bool {
        f64::is_nan(self)
    }
    fn is_sign_negative(self) -> bool {
        f64::is_sign_negative(self)
    }
    fn is_sign_positive(self) -> bool {
        f64::is_sign_positive(self)
    }
    fn square_root(self) -> Self {
        f64::sqrt(self)
    }
    fn round_up(self) -> Self {
        f64::ceil(self)
    }
    fn round_down(self) -> Self {
        f64::floor(self)
    }
    fn round_toward_zero(self) -> Self {
        f64::trunc(self)
    }
    fn round_to_even(self) -> Self {
        f64::round_ties_even(self)
    }
}

impl FloatBits for f32 {
    const MASK: u64 = 0xFFFF_FFFF;
    const SIGN: u64 = 0x8000_0000;
    const CANONICAL: u64 = 0x7fc0_0000;
    fn from_word(word: i64) -> Self {
        f32::from_bits(word as u32)
    }
    fn to_word(self) -> u64 {
        u64::from(self.to_bits())
    }
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
    fn is_nan(self) -> bool {
        f32::is_nan(self)
    }
    fn is_sign_negative(self) -> bool {
        f32::is_sign_negative(self)
    }
    fn is_sign_positive(self) -> bool {
        f32::is_sign_positive(self)
    }
    fn square_root(self) -> Self {
        f32::sqrt(self)
    }
    fn round_up(self) -> Self {
        f32::ceil(self)
    }
    fn round_down(self) -> Self {
        f32::floor(self)
    }
    fn round_toward_zero(self) -> Self {
        f32::trunc(self)
    }
    fn round_to_even(self) -> Self {
        f32::round_ties_even(self)
    }
}

/// The word a float result is stored as: its bits, or the canonical NaN if it is a NaN, as Micro gives
/// every NaN from arithmetic.
fn canonical<T: FloatBits>(value: T) -> i64 {
    if value.is_nan() {
        T::CANONICAL as i64
    } else {
        value.to_word() as i64
    }
}

/// `min`, with Micro's rules: a NaN in either operand gives the canonical NaN, and two zeros of
/// different sign give the negative one. Otherwise the smaller operand.
fn float_min<T: FloatBits>(left: T, right: T) -> T {
    let zero = T::from_word(0);
    if left.is_nan() || right.is_nan() {
        T::from_word(T::CANONICAL as i64)
    } else if left == zero && right == zero {
        if left.is_sign_negative() || right.is_sign_negative() {
            T::from_word(T::SIGN as i64)
        } else {
            zero
        }
    } else if left < right {
        left
    } else {
        right
    }
}

/// `max`, with Micro's rules: a NaN in either operand gives the canonical NaN, and two zeros of
/// different sign give the positive one. Otherwise the larger operand.
fn float_max<T: FloatBits>(left: T, right: T) -> T {
    let zero = T::from_word(0);
    if left.is_nan() || right.is_nan() {
        T::from_word(T::CANONICAL as i64)
    } else if left == zero && right == zero {
        if left.is_sign_positive() || right.is_sign_positive() {
            zero
        } else {
            T::from_word(T::SIGN as i64)
        }
    } else if left > right {
        left
    } else {
        right
    }
}

/// A binary float operation on two slot words, giving the result word.
fn float_bin_word<T: FloatBits>(op: FloatOp, a: i64, b: i64) -> i64 {
    let (x, y) = (T::from_word(a), T::from_word(b));
    match op {
        // The sign of the second operand, and the magnitude and payload of the first.
        FloatOp::Copysign => ((((a as u64) & !T::SIGN) | ((b as u64) & T::SIGN)) & T::MASK) as i64,
        FloatOp::Add => canonical(x + y),
        FloatOp::Sub => canonical(x - y),
        FloatOp::Mul => canonical(x * y),
        FloatOp::Div => canonical(x / y),
        FloatOp::Min => canonical(float_min(x, y)),
        FloatOp::Max => canonical(float_max(x, y)),
    }
}

/// A float comparison of two slot words. A NaN is unequal to everything, and below or above nothing.
fn float_compare_word<T: FloatBits>(cond: FloatComparison, a: i64, b: i64) -> bool {
    let (x, y) = (T::from_word(a), T::from_word(b));
    match cond {
        FloatComparison::Eq => x == y,
        FloatComparison::Ne => x != y,
        FloatComparison::Lt => x < y,
        FloatComparison::Gt => x > y,
        FloatComparison::Le => x <= y,
        FloatComparison::Ge => x >= y,
    }
}

/// A unary float operation on a slot word. The sign operations work on the bits, keeping a NaN's
/// payload; the others give the canonical NaN for a NaN result.
fn float_unary_word<T: FloatBits>(op: FloatUnaryOp, a: i64) -> i64 {
    let x = T::from_word(a);
    match op {
        FloatUnaryOp::Abs => ((a as u64) & !T::SIGN & T::MASK) as i64,
        FloatUnaryOp::Neg => (((a as u64) ^ T::SIGN) & T::MASK) as i64,
        FloatUnaryOp::Sqrt => canonical(x.square_root()),
        FloatUnaryOp::Ceil => canonical(x.round_up()),
        FloatUnaryOp::Floor => canonical(x.round_down()),
        FloatUnaryOp::Trunc => canonical(x.round_toward_zero()),
        FloatUnaryOp::Nearest => canonical(x.round_to_even()),
    }
}

/// The machine IR's unary float operator for an IR one.
fn float_unary_op(operation: FloatUnary) -> FloatUnaryOp {
    match operation {
        FloatUnary::Abs => FloatUnaryOp::Abs,
        FloatUnary::Neg => FloatUnaryOp::Neg,
        FloatUnary::Sqrt => FloatUnaryOp::Sqrt,
        FloatUnary::Ceil => FloatUnaryOp::Ceil,
        FloatUnary::Floor => FloatUnaryOp::Floor,
        FloatUnary::Trunc => FloatUnaryOp::Trunc,
        FloatUnary::Nearest => FloatUnaryOp::Nearest,
    }
}

/// The size of a Wasm page, in bytes.
pub const PAGE_BYTES: usize = 65_536;

/// The loads: how many bytes each reads, and how the bytes extend to a whole word. `Word` and the
/// unsigned forms zero-extend; the signed forms sign-extend. An `i32` load is the same as the `i64`
/// load of its width, since an `i32` is read through its low half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOp {
    /// Four bytes, zero-extended.
    Word,
    /// Four bytes, sign-extended.
    SignedWord,
    /// Eight bytes.
    Quad,
    SignedByte,
    UnsignedByte,
    SignedHalf,
    UnsignedHalf,
}

impl LoadOp {
    /// Bytes read by this load.
    pub fn width(self) -> u32 {
        match self {
            LoadOp::Quad => 8,
            LoadOp::Word | LoadOp::SignedWord => 4,
            LoadOp::SignedByte | LoadOp::UnsignedByte => 1,
            LoadOp::SignedHalf | LoadOp::UnsignedHalf => 2,
        }
    }
}

/// The stores: how many of the value's low bytes each writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOp {
    Quad,
    Word,
    Byte,
    Half,
}

impl StoreOp {
    /// Bytes written by this store.
    pub fn width(self) -> u32 {
        match self {
            StoreOp::Quad => 8,
            StoreOp::Word => 4,
            StoreOp::Byte => 1,
            StoreOp::Half => 2,
        }
    }
}

/// One machine operation. Every operation names its destination and its sources by slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirOp {
    /// `dst = memory[address + offset]`, read as `op` and extended to 32 bits. The address is the
    /// 32-bit operand plus the offset, without wrapping; an access past the memory traps.
    Load {
        op: LoadOp,
        dst: Slot,
        address: Slot,
        offset: u32,
    },
    /// `memory[address + offset] = value`, writing the low bytes `op` names.
    Store {
        op: StoreOp,
        address: Slot,
        value: Slot,
        offset: u32,
    },
    /// `dst = a op b` for an `i32` division or remainder. A zero divisor traps; so does signed
    /// division of the minimum value by `-1`.
    Divide {
        op: DivOp,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = a op b` for an `i64` division or remainder, with the same two traps.
    Divide64 {
        op: DivOp,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = value`, an `i32`.
    Const { dst: Slot, value: i32 },
    /// `dst = value`, an `i64`.
    Const64 { dst: Slot, value: i64 },
    /// `dst = src`: the whole word, so it serves both widths.
    Move { dst: Slot, src: Slot },
    /// `dst = a op b` for `i32`, with the operator's wrapping and shift-count rules.
    Bin {
        op: BinOp,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = a op b` for `i64`, with the same rules at 64 bits.
    Bin64 {
        op: BinOp,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = (a cond b) as i32`, comparing `i32` operands.
    Compare {
        cond: IntComparison,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = (a cond b) as i32`, comparing `i64` operands.
    Compare64 {
        cond: IntComparison,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = eqz(a)` as an `i32`, for an `i32` operand.
    Eqz { dst: Slot, a: Slot },
    /// `dst = eqz(a)` as an `i32`, for an `i64` operand.
    Eqz64 { dst: Slot, a: Slot },
    /// `dst = clz(a)`, `ctz(a)` or `popcnt(a)`, counted over the 32 bits of `a`.
    Unary { op: IntUnary, dst: Slot, a: Slot },
    /// `dst = a op b` for a float, the operands and the result being bit patterns.
    FloatBin {
        precision: Precision,
        op: FloatOp,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = (a cond b) as i32` for float operands: a comparison with a NaN is false, except `ne`.
    FloatCompare {
        precision: Precision,
        cond: FloatComparison,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = op(a)` for a float.
    FloatUnary {
        precision: Precision,
        op: FloatUnaryOp,
        dst: Slot,
        a: Slot,
    },
    /// `dst = the low 32 bits of src, zero-extended`: a reinterpretation or width change that keeps
    /// the value's bits and gives a canonical word.
    Narrow { dst: Slot, src: Slot },
    /// `dst = src`'s low `bits` bits, sign-extended to a whole word.
    SignExtend { dst: Slot, src: Slot, bits: u32 },
    /// `dst = the integer src, read as `from`, rounded to a float of precision `to``.
    IntToFloat {
        to: Precision,
        from: IntKind,
        dst: Slot,
        src: Slot,
    },
    /// `dst = the float src, converted to precision `to``; a NaN is the canonical one.
    FloatToFloat { to: Precision, dst: Slot, src: Slot },
    /// `dst = the float src, truncated to an integer of kind `to``. A NaN, or a value outside the
    /// kind's range once truncated, traps with `InvalidConversion`.
    FloatToInt {
        from: Precision,
        to: IntKind,
        dst: Slot,
        src: Slot,
    },
    /// `dst = clz(a)`, `ctz(a)` or `popcnt(a)` for `i64`, counted over the 64 bits of `a`.
    Unary64 { op: IntUnary, dst: Slot, a: Slot },
    /// `dst = memory size in pages`.
    MemorySize { dst: Slot },
    /// `dst = previous size in pages`, after growing by `delta` pages, or `-1` (leaving the memory
    /// as it was) when the new size would pass the reserved capacity.
    MemoryGrow { dst: Slot, delta: Slot },
    /// `dst = globals[index]`: the entry's low 32 bits zero-extended for an `i32` or `f32` global, the
    /// whole entry for an `i64` or `f64` one.
    GlobalGet {
        dst: Slot,
        index: u32,
        width: WordWidth,
    },
    /// `globals[index] = src`, in the same widths: the value's low 32 bits zero-extended for a 32-bit
    /// global, its whole word otherwise.
    GlobalSet {
        index: u32,
        src: Slot,
        width: WordWidth,
    },
    /// `dsts = function(args...)`: a call of a function of the module, by index. Its results, in order,
    /// are `dsts`, one slot each; a function with no result has none. The callee of a function with two
    /// or more results writes them to the argument block, which the caller sizes for them.
    Call {
        function: u32,
        args: Vec<Slot>,
        dst: Vec<Slot>,
    },
    /// `dst = table[index](args...)`: an indirect call through the module's one table. The slot's
    /// function must have the type `type_index` names, as [`Indirect`] records.
    CallIndirect {
        type_index: u32,
        index: Slot,
        args: Vec<Slot>,
        dst: Vec<Slot>,
    },
    /// `dsts = import(args...)`: a call of a host import, by its index in the module's imports. Each
    /// argument and result is a word, in the width of its type.
    CallHost {
        import: u32,
        args: Vec<(Slot, WordWidth)>,
        dsts: Vec<(Slot, WordWidth)>,
    },
    /// `dst = if cond != 0 { a } else { b }`: the whole word, so it serves both widths.
    Select {
        dst: Slot,
        cond: Slot,
        a: Slot,
        b: Slot,
    },
}

/// How a machine block ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirTerm {
    /// Continue at the block with this index.
    Jump(usize),
    /// Continue at `then_block` if `cond` is nonzero, else at `else_block`.
    Branch {
        cond: Slot,
        then_block: usize,
        else_block: usize,
    },
    /// Return the values in these slots, in order.
    Return(Vec<Slot>),
    /// Trap.
    Trap(Trap),
}

/// A straight-line run of operations followed by one terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirBlock {
    pub ops: Vec<MirOp>,
    pub term: MirTerm,
}

/// A function in machine IR. Its arguments arrive in `params`, its blocks are indexed from zero, and
/// it starts at `entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirFunction {
    pub params: Vec<Slot>,
    pub slots: u32,
    pub entry: usize,
    pub blocks: Vec<MirBlock>,
}

/// Why a machine-IR run ended without returning values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirError {
    /// The function trapped, with the trap it raised.
    Trap(Trap),
    /// A host call failed, or the run had no host to call. The native code reports this as a failed
    /// host call too.
    HostFailed,
}

/// Lower a verified IR function to machine IR.
///
/// # Errors
///
/// [`JitError::Unsupported`] for an instruction or a value type outside the slice.
pub fn lower(function: &IrFunction) -> Result<MirFunction, JitError> {
    let mut lowerer = Lowerer::new(function)?;
    for block in &function.blocks {
        let mut ops = Vec::new();
        for instruction in &block.instrs {
            lowerer.instruction(instruction, &mut ops)?;
        }
        let term = lowerer.terminator(&block.terminator, &mut ops)?;
        let index = lowerer.index[&block.id];
        lowerer.blocks[index] = Some(MirBlock { ops, term });
    }
    let blocks = lowerer
        .blocks
        .into_iter()
        .map(|block| block.expect("every block is lowered before the function is returned"))
        .collect();
    Ok(MirFunction {
        params: function.params.iter().map(|value| value.0).collect(),
        slots: lowerer.next_slot,
        entry: lowerer.index[&function.entry],
        blocks,
    })
}

struct Lowerer<'a> {
    function: &'a IrFunction,
    /// The machine-block index of each IR block.
    index: HashMap<BlockId, usize>,
    /// The machine blocks, filled in as they are lowered; edge blocks are appended.
    blocks: Vec<Option<MirBlock>>,
    /// The next unused slot, past every value id and temporary.
    next_slot: u32,
}

impl<'a> Lowerer<'a> {
    fn new(function: &'a IrFunction) -> Result<Self, JitError> {
        if function.blocks.is_empty() {
            return Err(JitError::Unsupported("a function with no blocks".into()));
        }
        if let Some(value) = function.values.iter().find(|value| {
            !matches!(
                value.value_type,
                ValueType::I32 | ValueType::I64 | ValueType::F32 | ValueType::F64
            )
        }) {
            return Err(JitError::Unsupported(format!(
                "a value of type {:?}",
                value.value_type
            )));
        }
        let index: HashMap<BlockId, usize> = function
            .blocks
            .iter()
            .enumerate()
            .map(|(position, block)| (block.id, position))
            .collect();
        let next_slot = function
            .values
            .iter()
            .map(|value| value.id.0 + 1)
            .max()
            .unwrap_or(0);
        Ok(Self {
            function,
            index,
            blocks: vec![None; function.blocks.len()],
            next_slot,
        })
    }

    fn temporary(&mut self) -> Slot {
        let slot = self.next_slot;
        self.next_slot += 1;
        slot
    }

    fn slot(value: ValueId) -> Slot {
        value.0
    }

    /// The type of `value`. Every value of a function is listed, so the lookup finds each one.
    fn value_type(&self, value: ValueId) -> Option<ValueType> {
        self.function
            .values
            .iter()
            .find(|candidate| candidate.id == value)
            .map(|candidate| candidate.value_type)
    }

    /// The width a word of `value` has where it crosses a boundary: an `i64` or `f64` whole, and an
    /// `i32` or `f32` in its low half.
    fn width_of(&self, value: ValueId) -> WordWidth {
        match self.value_type(value) {
            Some(ValueType::I64 | ValueType::F64) => WordWidth::Full64,
            _ => WordWidth::Low32,
        }
    }

    fn block_params(&self, block: BlockId) -> Result<Vec<ValueId>, JitError> {
        self.function
            .blocks
            .iter()
            .find(|candidate| candidate.id == block)
            .map(|candidate| candidate.params.clone())
            .ok_or_else(|| JitError::Unsupported(format!("a branch to block {}", block.0)))
    }

    /// The moves that carry `values` into `params`, in parallel: every source is copied to a
    /// temporary before any destination is written.
    fn edge_moves(&mut self, values: &[ValueId], params: &[ValueId], ops: &mut Vec<MirOp>) {
        let temporaries: Vec<Slot> = values
            .iter()
            .map(|value| {
                let temporary = self.temporary();
                ops.push(MirOp::Move {
                    dst: temporary,
                    src: Self::slot(*value),
                });
                temporary
            })
            .collect();
        for (temporary, param) in temporaries.into_iter().zip(params) {
            ops.push(MirOp::Move {
                dst: Self::slot(*param),
                src: temporary,
            });
        }
    }

    /// A new machine block that performs `moves` and then jumps to the IR block `target`.
    fn edge_block(&mut self, values: &[ValueId], target: BlockId) -> Result<usize, JitError> {
        let params = self.block_params(target)?;
        let mut ops = Vec::new();
        self.edge_moves(values, &params, &mut ops);
        let target_index = self.index[&target];
        self.blocks.push(Some(MirBlock {
            ops,
            term: MirTerm::Jump(target_index),
        }));
        Ok(self.blocks.len() - 1)
    }

    fn terminator(
        &mut self,
        terminator: &Terminator,
        ops: &mut Vec<MirOp>,
    ) -> Result<MirTerm, JitError> {
        match terminator {
            Terminator::Return(values) => Ok(MirTerm::Return(
                values.iter().map(|value| Self::slot(*value)).collect(),
            )),
            Terminator::Trap(trap) => Ok(MirTerm::Trap(trap.clone())),
            Terminator::Unreachable => Ok(MirTerm::Trap(Trap::Unreachable)),
            Terminator::Branch { target, values } => {
                let params = self.block_params(*target)?;
                self.edge_moves(values, &params, ops);
                Ok(MirTerm::Jump(self.index[target]))
            }
            Terminator::CondBranch {
                condition,
                then_target,
                then_values,
                else_target,
                else_values,
            } => {
                // Each arm carries its own values, so each gets its own edge block, unless it
                // carries none and can name its target directly.
                let then_block = if then_values.is_empty() {
                    self.index[then_target]
                } else {
                    self.edge_block(then_values, *then_target)?
                };
                let else_block = if else_values.is_empty() {
                    self.index[else_target]
                } else {
                    self.edge_block(else_values, *else_target)?
                };
                Ok(MirTerm::Branch {
                    cond: Self::slot(*condition),
                    then_block,
                    else_block,
                })
            }
        }
    }

    fn instruction(&mut self, instruction: &IrInstr, ops: &mut Vec<MirOp>) -> Result<(), JitError> {
        use IrInstr as I;
        let bin = |op: BinOp, result: &ValueId, left: &ValueId, right: &ValueId| MirOp::Bin {
            op,
            dst: Self::slot(*result),
            a: Self::slot(*left),
            b: Self::slot(*right),
        };
        let bin64 = |op: BinOp, result: &ValueId, left: &ValueId, right: &ValueId| MirOp::Bin64 {
            op,
            dst: Self::slot(*result),
            a: Self::slot(*left),
            b: Self::slot(*right),
        };
        let divide = |op: DivOp, result: &ValueId, left: &ValueId, right: &ValueId| MirOp::Divide {
            op,
            dst: Self::slot(*result),
            a: Self::slot(*left),
            b: Self::slot(*right),
        };
        let float_bin = |precision: Precision,
                         op: FloatOp,
                         result: &ValueId,
                         left: &ValueId,
                         right: &ValueId| MirOp::FloatBin {
            precision,
            op,
            dst: Self::slot(*result),
            a: Self::slot(*left),
            b: Self::slot(*right),
        };
        let divide64 =
            |op: DivOp, result: &ValueId, left: &ValueId, right: &ValueId| MirOp::Divide64 {
                op,
                dst: Self::slot(*result),
                a: Self::slot(*left),
                b: Self::slot(*right),
            };
        let lowered = match instruction {
            I::ConstI32 { result, value } => MirOp::Const {
                dst: Self::slot(*result),
                value: *value,
            },
            I::ConstI64 { result, value } => MirOp::Const64 {
                dst: Self::slot(*result),
                value: *value,
            },
            I::Drop { .. } => return Ok(()),
            I::Select {
                result,
                condition,
                left,
                right,
            } => MirOp::Select {
                dst: Self::slot(*result),
                cond: Self::slot(*condition),
                a: Self::slot(*left),
                b: Self::slot(*right),
            },
            I::LocalGet { result, local } => MirOp::Move {
                dst: Self::slot(*result),
                src: Self::slot(*local),
            },
            I::LocalSet { local, value } => MirOp::Move {
                dst: Self::slot(*local),
                src: Self::slot(*value),
            },
            I::LocalTee {
                result,
                local,
                value,
            } => {
                ops.push(MirOp::Move {
                    dst: Self::slot(*local),
                    src: Self::slot(*value),
                });
                MirOp::Move {
                    dst: Self::slot(*result),
                    src: Self::slot(*value),
                }
            }
            I::I32Add {
                result,
                left,
                right,
            } => bin(BinOp::Add, result, left, right),
            I::I32Sub {
                result,
                left,
                right,
            } => bin(BinOp::Sub, result, left, right),
            I::I32Mul {
                result,
                left,
                right,
            } => bin(BinOp::Mul, result, left, right),
            I::I32And {
                result,
                left,
                right,
            } => bin(BinOp::And, result, left, right),
            I::I32Or {
                result,
                left,
                right,
            } => bin(BinOp::Or, result, left, right),
            I::I32Xor {
                result,
                left,
                right,
            } => bin(BinOp::Xor, result, left, right),
            I::I32DivS {
                result,
                left,
                right,
            } => divide(DivOp::DivS, result, left, right),
            I::I32DivU {
                result,
                left,
                right,
            } => divide(DivOp::DivU, result, left, right),
            I::I32RemS {
                result,
                left,
                right,
            } => divide(DivOp::RemS, result, left, right),
            I::I32RemU {
                result,
                left,
                right,
            } => divide(DivOp::RemU, result, left, right),
            I::I32Shl {
                result,
                left,
                right,
            } => bin(BinOp::Shl, result, left, right),
            I::I32ShrS {
                result,
                left,
                right,
            } => bin(BinOp::ShrS, result, left, right),
            I::I32ShrU {
                result,
                left,
                right,
            } => bin(BinOp::ShrU, result, left, right),
            I::I32Rotl {
                result,
                left,
                right,
            } => bin(BinOp::Rotl, result, left, right),
            I::I32Rotr {
                result,
                left,
                right,
            } => bin(BinOp::Rotr, result, left, right),
            I::I64Add {
                result,
                left,
                right,
            } => bin64(BinOp::Add, result, left, right),
            I::I64Sub {
                result,
                left,
                right,
            } => bin64(BinOp::Sub, result, left, right),
            I::I64Mul {
                result,
                left,
                right,
            } => bin64(BinOp::Mul, result, left, right),
            I::I64And {
                result,
                left,
                right,
            } => bin64(BinOp::And, result, left, right),
            I::I64Or {
                result,
                left,
                right,
            } => bin64(BinOp::Or, result, left, right),
            I::I64Xor {
                result,
                left,
                right,
            } => bin64(BinOp::Xor, result, left, right),
            I::I64Shl {
                result,
                left,
                right,
            } => bin64(BinOp::Shl, result, left, right),
            I::I64ShrS {
                result,
                left,
                right,
            } => bin64(BinOp::ShrS, result, left, right),
            I::I64ShrU {
                result,
                left,
                right,
            } => bin64(BinOp::ShrU, result, left, right),
            I::I64Rotl {
                result,
                left,
                right,
            } => bin64(BinOp::Rotl, result, left, right),
            I::I64Rotr {
                result,
                left,
                right,
            } => bin64(BinOp::Rotr, result, left, right),
            I::I64DivS {
                result,
                left,
                right,
            } => divide64(DivOp::DivS, result, left, right),
            I::I64DivU {
                result,
                left,
                right,
            } => divide64(DivOp::DivU, result, left, right),
            I::I64RemS {
                result,
                left,
                right,
            } => divide64(DivOp::RemS, result, left, right),
            I::I64RemU {
                result,
                left,
                right,
            } => divide64(DivOp::RemU, result, left, right),
            I::I32Compare {
                result,
                left,
                right,
                comparison,
            } => MirOp::Compare {
                cond: *comparison,
                dst: Self::slot(*result),
                a: Self::slot(*left),
                b: Self::slot(*right),
            },
            I::I64Compare {
                result,
                left,
                right,
                comparison,
            } => MirOp::Compare64 {
                cond: *comparison,
                dst: Self::slot(*result),
                a: Self::slot(*left),
                b: Self::slot(*right),
            },
            I::I32Eqz { result, value } => MirOp::Eqz {
                dst: Self::slot(*result),
                a: Self::slot(*value),
            },
            I::I64Eqz { result, value } => MirOp::Eqz64 {
                dst: Self::slot(*result),
                a: Self::slot(*value),
            },
            I::F64Min {
                result,
                left,
                right,
            } => float_bin(Precision::Double, FloatOp::Min, result, left, right),
            I::F64Max {
                result,
                left,
                right,
            } => float_bin(Precision::Double, FloatOp::Max, result, left, right),
            I::F64Copysign {
                result,
                left,
                right,
            } => float_bin(Precision::Double, FloatOp::Copysign, result, left, right),
            I::ConstF64 { result, value } => MirOp::Const64 {
                dst: Self::slot(*result),
                value: *value as i64,
            },
            I::F64Add {
                result,
                left,
                right,
            } => float_bin(Precision::Double, FloatOp::Add, result, left, right),
            I::F64Sub {
                result,
                left,
                right,
            } => float_bin(Precision::Double, FloatOp::Sub, result, left, right),
            I::F64Mul {
                result,
                left,
                right,
            } => float_bin(Precision::Double, FloatOp::Mul, result, left, right),
            I::F64Div {
                result,
                left,
                right,
            } => float_bin(Precision::Double, FloatOp::Div, result, left, right),
            I::F64Compare {
                result,
                left,
                right,
                comparison,
            } => MirOp::FloatCompare {
                precision: Precision::Double,
                cond: *comparison,
                dst: Self::slot(*result),
                a: Self::slot(*left),
                b: Self::slot(*right),
            },
            I::F32Compare {
                result,
                left,
                right,
                comparison,
            } => MirOp::FloatCompare {
                precision: Precision::Single,
                cond: *comparison,
                dst: Self::slot(*result),
                a: Self::slot(*left),
                b: Self::slot(*right),
            },
            I::F64Unary {
                result,
                value,
                operation,
            } => MirOp::FloatUnary {
                precision: Precision::Double,
                op: float_unary_op(*operation),
                dst: Self::slot(*result),
                a: Self::slot(*value),
            },
            I::F32Unary {
                result,
                value,
                operation,
            } => MirOp::FloatUnary {
                precision: Precision::Single,
                op: float_unary_op(*operation),
                dst: Self::slot(*result),
                a: Self::slot(*value),
            },
            I::F32Add {
                result,
                left,
                right,
            } => float_bin(Precision::Single, FloatOp::Add, result, left, right),
            I::F32Sub {
                result,
                left,
                right,
            } => float_bin(Precision::Single, FloatOp::Sub, result, left, right),
            I::F32Mul {
                result,
                left,
                right,
            } => float_bin(Precision::Single, FloatOp::Mul, result, left, right),
            I::F32Div {
                result,
                left,
                right,
            } => float_bin(Precision::Single, FloatOp::Div, result, left, right),
            I::F32Min {
                result,
                left,
                right,
            } => float_bin(Precision::Single, FloatOp::Min, result, left, right),
            I::F32Max {
                result,
                left,
                right,
            } => float_bin(Precision::Single, FloatOp::Max, result, left, right),
            I::F32Copysign {
                result,
                left,
                right,
            } => float_bin(Precision::Single, FloatOp::Copysign, result, left, right),
            // The bits zero-extended, so the word is the value Micro returns for an `f32`.
            // A wrap keeps the low half, which an `i32` reads, so it is a move of the word.
            I::IntConvert {
                result,
                value,
                operation,
            } => match operation {
                IntConversion::I32WrapI64 => MirOp::Move {
                    dst: Self::slot(*result),
                    src: Self::slot(*value),
                },
                IntConversion::I64ExtendI32S => MirOp::SignExtend {
                    dst: Self::slot(*result),
                    src: Self::slot(*value),
                    bits: 32,
                },
                IntConversion::I64ExtendI32U => MirOp::Narrow {
                    dst: Self::slot(*result),
                    src: Self::slot(*value),
                },
            },
            I::Reinterpret {
                result,
                value,
                operation,
            } => match operation {
                // An `i32` or `f32` word is canonical, so the upper half is cleared.
                Reinterpret::I32FromF32 | Reinterpret::F32FromI32 => MirOp::Narrow {
                    dst: Self::slot(*result),
                    src: Self::slot(*value),
                },
                Reinterpret::I64FromF64 | Reinterpret::F64FromI64 => MirOp::Move {
                    dst: Self::slot(*result),
                    src: Self::slot(*value),
                },
            },
            I::SignExtend {
                result,
                value,
                operation,
            } => MirOp::SignExtend {
                dst: Self::slot(*result),
                src: Self::slot(*value),
                bits: match operation {
                    SignExtend::I32Extend8S | SignExtend::I64Extend8S => 8,
                    SignExtend::I32Extend16S | SignExtend::I64Extend16S => 16,
                    SignExtend::I64Extend32S => 32,
                },
            },
            I::FloatConvert {
                result,
                value,
                operation,
            } => {
                let dst = Self::slot(*result);
                let src = Self::slot(*value);
                match operation {
                    FloatConversion::F32FromI32S => MirOp::IntToFloat {
                        to: Precision::Single,
                        from: IntKind::Signed32,
                        dst,
                        src,
                    },
                    FloatConversion::F32FromI32U => MirOp::IntToFloat {
                        to: Precision::Single,
                        from: IntKind::Unsigned32,
                        dst,
                        src,
                    },
                    FloatConversion::F32FromI64S => MirOp::IntToFloat {
                        to: Precision::Single,
                        from: IntKind::Signed64,
                        dst,
                        src,
                    },
                    FloatConversion::F32FromI64U => MirOp::IntToFloat {
                        to: Precision::Single,
                        from: IntKind::Unsigned64,
                        dst,
                        src,
                    },
                    FloatConversion::F64FromI32S => MirOp::IntToFloat {
                        to: Precision::Double,
                        from: IntKind::Signed32,
                        dst,
                        src,
                    },
                    FloatConversion::F64FromI32U => MirOp::IntToFloat {
                        to: Precision::Double,
                        from: IntKind::Unsigned32,
                        dst,
                        src,
                    },
                    FloatConversion::F64FromI64S => MirOp::IntToFloat {
                        to: Precision::Double,
                        from: IntKind::Signed64,
                        dst,
                        src,
                    },
                    FloatConversion::F64FromI64U => MirOp::IntToFloat {
                        to: Precision::Double,
                        from: IntKind::Unsigned64,
                        dst,
                        src,
                    },
                    FloatConversion::F32FromF64 => MirOp::FloatToFloat {
                        to: Precision::Single,
                        dst,
                        src,
                    },
                    FloatConversion::F64FromF32 => MirOp::FloatToFloat {
                        to: Precision::Double,
                        dst,
                        src,
                    },
                }
            }
            I::FloatTrunc {
                result,
                value,
                operation,
            } => {
                let dst = Self::slot(*result);
                let src = Self::slot(*value);
                let (from, to) = match operation {
                    FloatTrunc::I32FromF32S => (Precision::Single, IntKind::Signed32),
                    FloatTrunc::I32FromF32U => (Precision::Single, IntKind::Unsigned32),
                    FloatTrunc::I32FromF64S => (Precision::Double, IntKind::Signed32),
                    FloatTrunc::I32FromF64U => (Precision::Double, IntKind::Unsigned32),
                    FloatTrunc::I64FromF32S => (Precision::Single, IntKind::Signed64),
                    FloatTrunc::I64FromF32U => (Precision::Single, IntKind::Unsigned64),
                    FloatTrunc::I64FromF64S => (Precision::Double, IntKind::Signed64),
                    FloatTrunc::I64FromF64U => (Precision::Double, IntKind::Unsigned64),
                };
                MirOp::FloatToInt { from, to, dst, src }
            }
            I::ConstF32 { result, value } => MirOp::Const64 {
                dst: Self::slot(*result),
                value: i64::from(*value),
            },
            I::I64Unary {
                result,
                value,
                operation,
            } => MirOp::Unary64 {
                op: *operation,
                dst: Self::slot(*result),
                a: Self::slot(*value),
            },
            I::I32Unary {
                result,
                value,
                operation,
            } => MirOp::Unary {
                op: *operation,
                dst: Self::slot(*result),
                a: Self::slot(*value),
            },
            I::Call {
                function,
                arguments,
                results,
            } => {
                ops.push(MirOp::Call {
                    function: *function,
                    args: arguments.iter().map(|value| Self::slot(*value)).collect(),
                    dst: results.iter().map(|value| Self::slot(*value)).collect(),
                });
                return Ok(());
            }
            I::CallIndirect {
                type_index,
                table,
                operand,
                arguments,
                results,
            } => {
                if *table != 0 {
                    return Err(JitError::Unsupported("a call through another table".into()));
                }
                ops.push(MirOp::CallIndirect {
                    type_index: *type_index,
                    index: Self::slot(*operand),
                    args: arguments.iter().map(|value| Self::slot(*value)).collect(),
                    dst: results.iter().map(|value| Self::slot(*value)).collect(),
                });
                return Ok(());
            }
            I::CallHost {
                import,
                arguments,
                results,
            } => {
                // Each word crosses in the width of its type, as the host's convention states.
                ops.push(MirOp::CallHost {
                    import: *import,
                    args: arguments
                        .iter()
                        .map(|value| (Self::slot(*value), self.width_of(*value)))
                        .collect(),
                    dsts: results
                        .iter()
                        .map(|value| (Self::slot(*value), self.width_of(*value)))
                        .collect(),
                });
                return Ok(());
            }
            I::GlobalGet { result, global } => {
                ops.push(MirOp::GlobalGet {
                    dst: Self::slot(*result),
                    index: *global,
                    width: self.width_of(*result),
                });
                return Ok(());
            }
            I::GlobalSet { global, value } => {
                ops.push(MirOp::GlobalSet {
                    index: *global,
                    src: Self::slot(*value),
                    width: self.width_of(*value),
                });
                return Ok(());
            }
            I::MemorySize { result } => {
                ops.push(MirOp::MemorySize {
                    dst: Self::slot(*result),
                });
                return Ok(());
            }
            I::MemoryGrow { result, delta } => {
                ops.push(MirOp::MemoryGrow {
                    dst: Self::slot(*result),
                    delta: Self::slot(*delta),
                });
                return Ok(());
            }
            // The bounds hint is ignored: the machine IR checks every access, as the native code
            // does, so a hint that the access is in bounds cannot skip a check here.
            I::Load {
                result,
                address,
                offset,
                operation,
                bounds: _,
            } => {
                ops.push(MirOp::Load {
                    op: load_op(*operation)?,
                    dst: Self::slot(*result),
                    address: Self::slot(*address),
                    offset: *offset,
                });
                return Ok(());
            }
            I::Store {
                address,
                value,
                offset,
                operation,
                bounds: _,
            } => {
                ops.push(MirOp::Store {
                    op: store_op(*operation)?,
                    address: Self::slot(*address),
                    value: Self::slot(*value),
                    offset: *offset,
                });
                return Ok(());
            }
            other => {
                return Err(JitError::Unsupported(format!(
                    "{} in machine IR",
                    crate::shared::instruction_name(other)
                )))
            }
        };
        ops.push(lowered);
        Ok(())
    }
}

/// The load for a Wasm integer load, or a refusal for a float load.
fn load_op(operation: MemoryLoad) -> Result<LoadOp, JitError> {
    match operation {
        MemoryLoad::I32 => Ok(LoadOp::Word),
        MemoryLoad::I32_8S => Ok(LoadOp::SignedByte),
        MemoryLoad::I32_8U => Ok(LoadOp::UnsignedByte),
        MemoryLoad::I32_16S => Ok(LoadOp::SignedHalf),
        MemoryLoad::I32_16U => Ok(LoadOp::UnsignedHalf),
        MemoryLoad::I64 => Ok(LoadOp::Quad),
        MemoryLoad::I64_8S => Ok(LoadOp::SignedByte),
        MemoryLoad::I64_8U => Ok(LoadOp::UnsignedByte),
        MemoryLoad::I64_16S => Ok(LoadOp::SignedHalf),
        MemoryLoad::I64_16U => Ok(LoadOp::UnsignedHalf),
        MemoryLoad::I64_32S => Ok(LoadOp::SignedWord),
        MemoryLoad::I64_32U => Ok(LoadOp::Word),
        // A float load moves the same bits as the integer load of its width.
        MemoryLoad::F64 => Ok(LoadOp::Quad),
        // A float load moves the same bits as the integer load of its width.
        MemoryLoad::F32 => Ok(LoadOp::Word),
    }
}

/// The store for a Wasm integer store, or a refusal for a float store. A store writes the low bytes
/// of the value's word, so the `i64` store of a width is the `i32` store of that width.
fn store_op(operation: MemoryStore) -> Result<StoreOp, JitError> {
    match operation {
        MemoryStore::I32 => Ok(StoreOp::Word),
        MemoryStore::I32_8 | MemoryStore::I64_8 => Ok(StoreOp::Byte),
        MemoryStore::I32_16 | MemoryStore::I64_16 => Ok(StoreOp::Half),
        MemoryStore::I64_32 => Ok(StoreOp::Word),
        MemoryStore::I64 | MemoryStore::F64 => Ok(StoreOp::Quad),
        MemoryStore::F32 => Ok(StoreOp::Word),
    }
}

/// The signed value of the low `bits` bits of `value`, sign-extended to a whole word.
fn sign_extend(value: u64, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

/// The module's function table and the signatures a call through it checks, laid out as the native
/// code lays them out, so both backends make the same checks in the same order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Indirect {
    /// The function each slot names, or `None` for a null slot. Empty when the module has no table.
    pub table: Vec<Option<u32>>,
    /// The canonical id of each module type, by type index: the index of the first equal type.
    pub type_ids: Vec<u64>,
    /// The id of each function's type, by function index. A type that is not among the module's
    /// types has the id `u64::MAX`, which no type index names.
    pub function_types: Vec<u64>,
}

impl Indirect {
    /// The table and signatures of `module`.
    pub fn for_module(module: &IrModule) -> Self {
        Self {
            table: table_slots(module),
            type_ids: type_ids(module),
            function_types: function_type_ids(module),
        }
    }
}

/// The module's one table, laid out by slot: each slot names a function index, or is `None` for a
/// null slot. An imported table, or one too large to lay out, has no slots here.
pub fn table_slots(module: &IrModule) -> Vec<Option<u32>> {
    let table = module
        .tables
        .first()
        .filter(|_| module.tables.len() == 1)
        .filter(|table| table.import.is_none() && table.init.is_none())
        .filter(|table| {
            table
                .segments
                .iter()
                .all(|segment| segment.offset_expr.is_none())
        })
        .filter(|table| {
            let imports = module.imports.len() as u32;
            table.segments.iter().all(|segment| {
                segment
                    .elements
                    .iter()
                    .flatten()
                    .all(|index| *index >= imports)
            }) && table.init.flatten().is_none_or(|index| index >= imports)
        })
        .filter(|table| table.init_expr.is_none())
        .filter(|table| {
            table
                .segments
                .iter()
                .all(|segment| segment.entry_exprs.is_empty())
        })
        .filter(|table| table.min <= 1 << 20);
    match table {
        Some(table) => {
            let mut slots = vec![None; table.min as usize];
            for segment in &table.segments {
                for (offset, element) in segment.elements.iter().enumerate() {
                    if let Some(slot) = slots.get_mut(segment.offset as usize + offset) {
                        *slot = *element;
                    }
                }
            }
            slots
        }
        None => Vec::new(),
    }
}

/// The canonical id of each module type: the index of the first type equal to it.
pub fn type_ids(module: &IrModule) -> Vec<u64> {
    module
        .types
        .iter()
        .map(|kind| {
            module
                .types
                .iter()
                .position(|other| other == kind)
                .unwrap_or(0) as u64
        })
        .collect()
}

/// The id of each function's type, by function index.
pub fn function_type_ids(module: &IrModule) -> Vec<u64> {
    module
        .functions
        .iter()
        .map(|function| {
            module
                .types
                .iter()
                .position(|kind| *kind == function.function_type)
                .map_or(u64::MAX, |id| id as u64)
        })
        .collect()
}

/// Run function `entry` of `functions` on `arguments`, returning its results as words.
///
/// Each argument and result is a 64-bit word: an `i32` in the low half, an `i64` whole. `globals`
/// holds every global the module names, as compiled code reads them: an `i32` in the low half of
/// each 64-bit entry. `functions` are the module's functions by index, which is how calls name them,
/// and `indirect` is its table. `memory` is the module's linear memory, read and written in place; it
/// grows in place by `memory.grow`, up to `capacity` bytes. A call counts against `max_depth`, the
/// entry's own frame being the first, and a call past it traps with [`Trap::CallDepthExceeded`].
/// `budget` bounds the number of blocks entered, over every frame, so a loop that does not end is
/// reported as [`Trap::StepsExhausted`] rather than hanging.
///
/// # Errors
///
/// [`MirError::Trap`] when the function traps, or runs out of budget or depth, and
/// [`MirError::HostFailed`] when a host call fails.
#[allow(clippy::too_many_arguments)]
pub fn run(
    functions: &[MirFunction],
    entry: usize,
    arguments: &[i64],
    memory: &mut Vec<u8>,
    capacity: usize,
    globals: &mut [u64],
    host: Option<&mut dyn HostCalls>,
    indirect: &Indirect,
    max_depth: u64,
    budget: u64,
) -> Result<Vec<i64>, MirError> {
    let mut machine = Machine {
        functions,
        memory,
        capacity,
        globals,
        host,
        indirect,
        depth: 1,
        max_depth,
        entered: 0,
        budget,
    };
    machine
        .call(entry, arguments)
        .map_err(|failure| match failure {
            Failure::Trap(trap) => MirError::Trap(trap),
            Failure::Host => MirError::HostFailed,
        })
}

/// Why a call ended without results: a trap, or a host call that failed.
enum Failure {
    Trap(Trap),
    Host,
}

impl From<Trap> for Failure {
    fn from(trap: Trap) -> Self {
        Failure::Trap(trap)
    }
}

/// The state shared by the frames of one run.
struct Machine<'a, 'h> {
    functions: &'a [MirFunction],
    memory: &'a mut Vec<u8>,
    capacity: usize,
    globals: &'a mut [u64],
    /// The host, with its own lifetime: a trait object behind `&mut` is invariant in its lifetime, so
    /// it cannot share the lifetime of the other borrows.
    host: Option<&'h mut (dyn HostCalls + 'h)>,
    indirect: &'a Indirect,
    /// The frames active now, the entry's included.
    depth: u64,
    max_depth: u64,
    /// Blocks entered so far, over every frame.
    entered: u64,
    budget: u64,
}

impl Machine<'_, '_> {
    /// Call function `callee` with the words of `args` read from `slots`, returning its results. The
    /// frame is pushed only if the depth allows it, as the native call checks before it enters.
    fn enter(&mut self, callee: usize, args: &[Slot], slots: &[i64]) -> Result<Vec<i64>, Failure> {
        if self.depth >= self.max_depth {
            return Err(Trap::CallDepthExceeded.into());
        }
        let values: Vec<i64> = args.iter().map(|slot| slots[*slot as usize]).collect();
        self.depth += 1;
        let results = self.call(callee, &values)?;
        self.depth -= 1;
        Ok(results)
    }

    fn call(&mut self, index: usize, arguments: &[i64]) -> Result<Vec<i64>, Failure> {
        let functions = self.functions;
        let function = &functions[index];
        let mut slots = vec![0_i64; function.slots as usize];
        for (slot, value) in function.params.iter().zip(arguments) {
            slots[*slot as usize] = *value;
        }
        let mut block = function.entry;
        loop {
            self.entered += 1;
            if self.entered > self.budget {
                return Err(Trap::StepsExhausted.into());
            }
            for op in &function.blocks[block].ops {
                match op {
                    MirOp::Call {
                        function: callee,
                        args,
                        dst,
                    } => {
                        let results = self.enter(*callee as usize, args, &slots)?;
                        for (dst, value) in dst.iter().zip(&results) {
                            slots[*dst as usize] = *value;
                        }
                    }
                    MirOp::CallIndirect {
                        type_index,
                        index,
                        args,
                        dst,
                    } => {
                        // The checks come in Micro's order: the index is in the table, the slot is
                        // not null, and the slot's function has the signature the call names.
                        let slot = slots[*index as usize] as u32 as usize;
                        let entry = match self.indirect.table.get(slot) {
                            None => return Err(Trap::TableOutOfBounds.into()),
                            Some(entry) => *entry,
                        };
                        let Some(callee) = entry else {
                            return Err(Trap::NullReference.into());
                        };
                        let actual = self.indirect.function_types.get(callee as usize).copied();
                        let expected = self.indirect.type_ids.get(*type_index as usize).copied();
                        if actual.is_none() || actual != expected {
                            return Err(Trap::IndirectCallTypeMismatch.into());
                        }
                        let results = self.enter(callee as usize, args, &slots)?;
                        for (dst, value) in dst.iter().zip(&results) {
                            slots[*dst as usize] = *value;
                        }
                    }
                    MirOp::CallHost { import, args, dsts } => {
                        // The host is called as the native trampoline calls it: each argument as its
                        // word in its width, the memory as the module's, and the results read back in
                        // their widths. A failed call, or no host, fails the run.
                        let Some(host) = self.host.as_deref_mut() else {
                            return Err(Failure::Host);
                        };
                        let words: Vec<i64> = args
                            .iter()
                            .map(|(slot, width)| narrow(*width, slots[*slot as usize]))
                            .collect();
                        let (_, returns) = host.arity(*import);
                        let mut results = vec![0_i64; returns];
                        if !host.call(*import, &words, &mut results, Some(&mut self.memory[..])) {
                            return Err(Failure::Host);
                        }
                        for ((dst, width), word) in dsts.iter().zip(&results) {
                            slots[*dst as usize] = narrow(*width, *word);
                        }
                    }
                    // The caller provides every global the module names, as the native code
                    // requires; a validated module names only those, so an index is in range.
                    MirOp::GlobalGet { dst, index, width } => {
                        slots[*dst as usize] = narrow(*width, self.globals[*index as usize] as i64);
                    }
                    MirOp::GlobalSet { index, src, width } => {
                        self.globals[*index as usize] = narrow(*width, slots[*src as usize]) as u64;
                    }
                    other => execute(other, &mut slots, &mut *self.memory, self.capacity)?,
                }
            }
            match &function.blocks[block].term {
                MirTerm::Jump(next) => block = *next,
                MirTerm::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    block = if slots[*cond as usize] as i32 != 0 {
                        *then_block
                    } else {
                        *else_block
                    };
                }
                MirTerm::Return(values) => {
                    return Ok(values.iter().map(|slot| slots[*slot as usize]).collect());
                }
                MirTerm::Trap(trap) => return Err(trap.clone().into()),
            }
        }
    }
}

/// A word narrowed to its width: a 32-bit value is its low half, zero-extended, and a whole word stays
/// whole.
fn narrow(width: WordWidth, word: i64) -> i64 {
    match width {
        WordWidth::Low32 => i64::from(word as u32),
        WordWidth::Full64 => word,
    }
}

/// The byte offset an access of `width` bytes touches, as WebAssembly computes it: the 32-bit
/// operand plus the offset, with no wrap. The access traps unless it lies wholly inside the memory.
fn effective_address(
    address: i32,
    offset: u32,
    width: u32,
    memory_len: usize,
) -> Result<usize, Trap> {
    let start = u64::from(address as u32) + u64::from(offset);
    if start + u64::from(width) > memory_len as u64 {
        Err(Trap::MemoryOutOfBounds)
    } else {
        Ok(start as usize)
    }
}

/// Execute one operation, or return the trap it raises. Only division and memory access trap, and
/// an operation that traps writes nothing.
fn execute(
    op: &MirOp,
    slots: &mut [i64],
    memory: &mut Vec<u8>,
    capacity: usize,
) -> Result<(), Trap> {
    // The `i32` operands of the slot at `slot`: its low 32 bits.
    let i32_at = |slots: &[i64], slot: Slot| slots[slot as usize] as i32;
    match op {
        MirOp::Const { dst, value } => slots[*dst as usize] = i64::from(*value),
        MirOp::Const64 { dst, value } => slots[*dst as usize] = *value,
        MirOp::Move { dst, src } => slots[*dst as usize] = slots[*src as usize],
        // Calls are run by `Machine::call`, which owns the frames; a call never reaches here.
        MirOp::Call { .. }
        | MirOp::CallIndirect { .. }
        | MirOp::CallHost { .. }
        | MirOp::GlobalGet { .. }
        | MirOp::GlobalSet { .. } => {
            unreachable!("calls are run by Machine::call")
        }
        MirOp::MemorySize { dst } => {
            slots[*dst as usize] = i64::from((memory.len() / PAGE_BYTES) as i32)
        }
        MirOp::MemoryGrow { dst, delta } => {
            let old_pages = memory.len() / PAGE_BYTES;
            // The page count is unsigned, so a negative operand is a large request, not a
            // shrink; the sum is taken in 64 bits, which cannot overflow for a 32-bit count.
            let pages = u64::from(slots[*delta as usize] as u32);
            let new_length = memory.len() as u64 + pages * PAGE_BYTES as u64;
            slots[*dst as usize] = if new_length > capacity as u64 {
                -1
            } else {
                memory.resize(new_length as usize, 0);
                old_pages as i64
            };
        }
        MirOp::Load {
            op,
            dst,
            address,
            offset,
        } => {
            let width = op.width() as usize;
            let at = effective_address(i32_at(slots, *address), *offset, op.width(), memory.len())?;
            let mut raw = [0_u8; 8];
            raw[..width].copy_from_slice(&memory[at..at + width]);
            let unsigned = u64::from_le_bytes(raw);
            slots[*dst as usize] = match op {
                LoadOp::Quad => unsigned as i64,
                LoadOp::Word | LoadOp::UnsignedByte | LoadOp::UnsignedHalf => unsigned as i64,
                LoadOp::SignedWord | LoadOp::SignedByte | LoadOp::SignedHalf => {
                    sign_extend(unsigned, op.width() * 8)
                }
            };
        }
        MirOp::Store {
            op,
            address,
            value,
            offset,
        } => {
            let width = op.width() as usize;
            let at = effective_address(i32_at(slots, *address), *offset, op.width(), memory.len())?;
            let bytes = slots[*value as usize].to_le_bytes();
            memory[at..at + width].copy_from_slice(&bytes[..width]);
        }
        MirOp::Divide { op, dst, a, b } => {
            let (x, y) = (i32_at(slots, *a), i32_at(slots, *b));
            slots[*dst as usize] = i64::from(divide(*op, x, y)?);
        }
        MirOp::Divide64 { op, dst, a, b } => {
            let (x, y) = (slots[*a as usize], slots[*b as usize]);
            slots[*dst as usize] = divide64(*op, x, y)?;
        }
        MirOp::Bin { op, dst, a, b } => {
            let (x, y) = (i32_at(slots, *a), i32_at(slots, *b));
            slots[*dst as usize] = i64::from(match op {
                BinOp::Add => x.wrapping_add(y),
                BinOp::Sub => x.wrapping_sub(y),
                BinOp::Mul => x.wrapping_mul(y),
                BinOp::And => x & y,
                BinOp::Or => x | y,
                BinOp::Xor => x ^ y,
                // `wrapping_sh*` reduce the count modulo the width, as WebAssembly does.
                BinOp::Shl => x.wrapping_shl(y as u32),
                BinOp::ShrS => x.wrapping_shr(y as u32),
                BinOp::ShrU => ((x as u32).wrapping_shr(y as u32)) as i32,
                // `rotate_left` reduces the count modulo the width, as WebAssembly does.
                BinOp::Rotl => (x as u32).rotate_left(y as u32) as i32,
                BinOp::Rotr => (x as u32).rotate_right(y as u32) as i32,
            });
        }
        MirOp::Bin64 { op, dst, a, b } => {
            let (x, y) = (slots[*a as usize], slots[*b as usize]);
            slots[*dst as usize] = match op {
                BinOp::Add => x.wrapping_add(y),
                BinOp::Sub => x.wrapping_sub(y),
                BinOp::Mul => x.wrapping_mul(y),
                BinOp::And => x & y,
                BinOp::Or => x | y,
                BinOp::Xor => x ^ y,
                // The counts reduce modulo 64, as WebAssembly does for `i64`.
                BinOp::Shl => x.wrapping_shl(y as u32),
                BinOp::ShrS => x.wrapping_shr(y as u32),
                BinOp::ShrU => ((x as u64).wrapping_shr(y as u32)) as i64,
                BinOp::Rotl => (x as u64).rotate_left(y as u32) as i64,
                BinOp::Rotr => (x as u64).rotate_right(y as u32) as i64,
            };
        }
        MirOp::Compare { cond, dst, a, b } => {
            let (x, y) = (i32_at(slots, *a), i32_at(slots, *b));
            let holds = compare(*cond, i64::from(x), i64::from(y), 32);
            slots[*dst as usize] = i64::from(holds);
        }
        MirOp::Compare64 { cond, dst, a, b } => {
            let (x, y) = (slots[*a as usize], slots[*b as usize]);
            slots[*dst as usize] = i64::from(compare(*cond, x, y, 64));
        }
        MirOp::Eqz { dst, a } => slots[*dst as usize] = i64::from(i32_at(slots, *a) == 0),
        MirOp::Eqz64 { dst, a } => slots[*dst as usize] = i64::from(slots[*a as usize] == 0),
        MirOp::FloatBin {
            precision,
            op,
            dst,
            a,
            b,
        } => {
            let (x, y) = (slots[*a as usize], slots[*b as usize]);
            slots[*dst as usize] = match precision {
                Precision::Single => float_bin_word::<f32>(*op, x, y),
                Precision::Double => float_bin_word::<f64>(*op, x, y),
            };
        }
        MirOp::FloatCompare {
            precision,
            cond,
            dst,
            a,
            b,
        } => {
            let (x, y) = (slots[*a as usize], slots[*b as usize]);
            let holds = match precision {
                Precision::Single => float_compare_word::<f32>(*cond, x, y),
                Precision::Double => float_compare_word::<f64>(*cond, x, y),
            };
            slots[*dst as usize] = i64::from(holds);
        }
        MirOp::FloatUnary {
            precision,
            op,
            dst,
            a,
        } => {
            let x = slots[*a as usize];
            slots[*dst as usize] = match precision {
                Precision::Single => float_unary_word::<f32>(*op, x),
                Precision::Double => float_unary_word::<f64>(*op, x),
            };
        }
        MirOp::Narrow { dst, src } => slots[*dst as usize] = i64::from(slots[*src as usize] as u32),
        MirOp::SignExtend { dst, src, bits } => {
            slots[*dst as usize] = sign_extend(slots[*src as usize] as u64, *bits);
        }
        MirOp::IntToFloat { to, from, dst, src } => {
            slots[*dst as usize] = int_to_float_word(*to, *from, slots[*src as usize]);
        }
        MirOp::FloatToFloat { to, dst, src } => {
            let word = slots[*src as usize];
            slots[*dst as usize] = match to {
                Precision::Single => canonical(f64::from_word(word) as f32),
                Precision::Double => canonical(f32::from_word(word) as f64),
            };
        }
        MirOp::FloatToInt { from, to, dst, src } => {
            slots[*dst as usize] = float_to_int_word(*from, *to, slots[*src as usize])?;
        }
        MirOp::Unary64 { op, dst, a } => {
            let x = slots[*a as usize] as u64;
            slots[*dst as usize] = match op {
                // A zero input has 64 leading and trailing zeros, as WebAssembly defines.
                IntUnary::Clz => i64::from(x.leading_zeros()),
                IntUnary::Ctz => i64::from(x.trailing_zeros()),
                IntUnary::Popcnt => i64::from(x.count_ones()),
            };
        }
        MirOp::Unary { op, dst, a } => {
            let x = slots[*a as usize] as u32;
            slots[*dst as usize] = i64::from(match op {
                // A zero input has 32 leading and trailing zeros, as WebAssembly defines.
                IntUnary::Clz => x.leading_zeros() as i32,
                IntUnary::Ctz => x.trailing_zeros() as i32,
                IntUnary::Popcnt => x.count_ones() as i32,
            });
        }
        MirOp::Select { dst, cond, a, b } => {
            slots[*dst as usize] = if i32_at(slots, *cond) != 0 {
                slots[*a as usize]
            } else {
                slots[*b as usize]
            };
        }
    }
    Ok(())
}

/// The canonical NaN of an `f64` result: Micro gives every NaN produced by arithmetic this bit
/// pattern, so the native code's own NaN is replaced with it.
pub const CANONICAL_NAN_F64: u64 = 0x7ff8_0000_0000_0000;

/// An integer of `from` as a float of precision `to`, rounded once from the integer, as Rust's `as`
/// rounds. The result is the float's bits, zero-extended.
fn int_to_float_word(to: Precision, from: IntKind, word: i64) -> i64 {
    match to {
        Precision::Single => {
            let value = match from {
                IntKind::Signed32 => word as i32 as f32,
                IntKind::Unsigned32 => word as u32 as f32,
                IntKind::Signed64 => word as f32,
                IntKind::Unsigned64 => word as u64 as f32,
            };
            i64::from(value.to_bits())
        }
        Precision::Double => {
            let value = match from {
                IntKind::Signed32 => word as i32 as f64,
                IntKind::Unsigned32 => word as u32 as f64,
                IntKind::Signed64 => word as f64,
                IntKind::Unsigned64 => word as u64 as f64,
            };
            value.to_bits() as i64
        }
    }
}

/// A float of precision `from` truncated to an integer of kind `to`, with Micro's rules: a NaN, or a
/// truncated value outside the kind's range, is an invalid conversion.
fn float_to_int_word(from: Precision, to: IntKind, word: i64) -> Result<i64, Trap> {
    match from {
        Precision::Single => trunc_to_int::<f32>(word, to),
        Precision::Double => trunc_to_int::<f64>(word, to),
    }
}

fn trunc_to_int<T: FloatBits>(word: i64, to: IntKind) -> Result<i64, Trap> {
    let truncated = T::from_word(word).round_toward_zero().to_f64();
    // The bounds are exact in both precisions, so the comparison is the same in either.
    let (low, high) = match to {
        IntKind::Signed32 => (-2_147_483_648.0, 2_147_483_648.0),
        IntKind::Unsigned32 => (0.0, 4_294_967_296.0),
        IntKind::Signed64 => (-9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0),
        IntKind::Unsigned64 => (0.0, 18_446_744_073_709_551_616.0),
    };
    if truncated.is_nan() || truncated < low || truncated >= high {
        return Err(Trap::InvalidConversion);
    }
    Ok(match to {
        IntKind::Signed32 => i64::from(truncated as i32),
        IntKind::Unsigned32 => i64::from(truncated as u32 as i32),
        IntKind::Signed64 => truncated as i64,
        IntKind::Unsigned64 => truncated as u64 as i64,
    })
}

/// Whether the comparison holds for the operands, read at `bits` width: the `i32` operands are
/// passed sign-extended, and an unsigned comparison reads them as unsigned at that width.
fn compare(cond: IntComparison, x: i64, y: i64, bits: u32) -> bool {
    let (ux, uy) = if bits == 32 {
        (u64::from(x as u32), u64::from(y as u32))
    } else {
        (x as u64, y as u64)
    };
    match cond {
        IntComparison::Eq => x == y,
        IntComparison::Ne => x != y,
        IntComparison::LtS => x < y,
        IntComparison::LtU => ux < uy,
        IntComparison::GtS => x > y,
        IntComparison::GtU => ux > uy,
        IntComparison::LeS => x <= y,
        IntComparison::LeU => ux <= uy,
        IntComparison::GeS => x >= y,
        IntComparison::GeU => ux >= uy,
    }
}

/// The division and remainder rules of WebAssembly: a zero divisor traps, signed division of the
/// minimum value by `-1` traps, and the remainder by `-1` is zero rather than a trap.
fn divide(op: DivOp, x: i32, y: i32) -> Result<i32, Trap> {
    if y == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(match op {
        DivOp::DivS => {
            if x == i32::MIN && y == -1 {
                return Err(Trap::IntegerOverflow);
            }
            x / y
        }
        DivOp::DivU => ((x as u32) / (y as u32)) as i32,
        DivOp::RemS => x.wrapping_rem(y),
        DivOp::RemU => ((x as u32) % (y as u32)) as i32,
    })
}

/// [`divide`] at 64 bits.
fn divide64(op: DivOp, x: i64, y: i64) -> Result<i64, Trap> {
    if y == 0 {
        return Err(Trap::IntegerDivisionByZero);
    }
    Ok(match op {
        DivOp::DivS => {
            if x == i64::MIN && y == -1 {
                return Err(Trap::IntegerOverflow);
            }
            x / y
        }
        DivOp::DivU => ((x as u64) / (y as u64)) as i64,
        DivOp::RemS => x.wrapping_rem(y),
        DivOp::RemU => ((x as u64) % (y as u64)) as i64,
    })
}
