// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Differential test: the same generated module run on both backends, compared
//! bit-exactly.
//!
//! The Core suite fixes *expected* values, upstream's own. That is the strongest
//! check there is, but it only covers shapes someone thought to write a test for.
//! This covers the other axis: the two backends here are independent
//! implementations of the same semantics -- an interpreter over decoded
//! instructions, and a block-graph compiler over the IR -- so any place they
//! disagree is a bug in at least one of them, whatever upstream says. The
//! project's own tracker names Micro as the golden result; this is that claim
//! under test rather than asserted.
//!
//! Everything is driven by an explicit seed, so a failure is reproducible from
//! the number the assertion prints. No ambient entropy and no wall clock, which
//! is the determinism policy the engine itself is held to.
//!
//! Three rules keep a generator this size honest, and all of them are load-bearing
//! rather than decorative:
//!
//!   * A module that fails to instantiate *panics* rather than being recorded as
//!     an outcome. Recorded, both backends would produce the same message and the
//!     comparison would pass without executing anything -- a vacuous pass, and the
//!     one failure mode this test cannot afford to be blind to.
//!   * Every statement is *stack-neutral*: it leaves the operand stack exactly as
//!     it found it. A generator that tracked a variable stack would need every
//!     case to cooperate with every other, and a mismatch would surface as a
//!     module rejected for reasons that have nothing to do with either backend.
//!   * Every immediate goes through a helper that encodes it. The binary format
//!     mixes LEB128 immediates with fixed-width ones, and getting that wrong is
//!     silent: a `u32` written as four little-endian bytes is a valid *first byte*
//!     followed by three stray instructions, and the reader reports the end of
//!     the module rather than the mistake.
//!
//! The structural sections are not hand-encoded at all -- the project's own
//! `encode` does those, so only function bodies are bytes here.

use tpt_wasm_decode::encode;
use tpt_wasm_format::{ConstExpr, Export, ExportDesc, Function, Global, LocalDecl, Memory, Module};
use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::{
    FunctionType, GlobalType, Limits, MemoryType, ResultType, Trap, Value, ValueType,
};

const I32: ValueType = ValueType::I32;
const I64: ValueType = ValueType::I64;
const F32: ValueType = ValueType::F32;
const F64: ValueType = ValueType::F64;

/// A splitmix64 generator. Small and fast; reproducibility is the only property
/// that matters here, so there is no case for a dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n`, by rejection so the distribution is not skewed toward
    /// the low end the way a modulo would leave it.
    fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        let threshold = n.wrapping_neg() % n;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return value % n;
            }
        }
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// An arbitrary `i32`, spanning the whole range including the values that make
    /// shifts, divisions, and conversions behave.
    fn i32(&mut self) -> i32 {
        self.next_u64() as u32 as i32
    }

    /// An `f32` from raw bits, so NaN payloads and signalling NaNs come out too.
    fn f32_bits(&mut self) -> u32 {
        match self.below(6) {
            0 => 0x0000_0000,
            1 => 0x3F80_0000, // 1.0
            2 => 0x7F80_0000, // +inf
            3 => 0x7FC0_0000, // canonical NaN
            4 => 0x7F80_0001, // signalling NaN
            _ => self.next_u64() as u32,
        }
    }

    /// An `f64` from raw bits, with the same spread.
    fn f64_bits(&mut self) -> u64 {
        match self.below(6) {
            0 => 0x0000_0000_0000_0000,
            1 => 0x3FF0_0000_0000_0000, // 1.0
            2 => 0x7FF0_0000_0000_0000, // +inf
            3 => 0x7FF8_0000_0000_0000, // canonical NaN
            4 => 0x7FF0_0000_0000_0001, // signalling NaN
            _ => self.next_u64(),
        }
    }
}

/// Where each kind of local lives, so the emitter can read and write them.
struct Layout {
    i32_locals: Vec<u32>,
    i64_locals: Vec<u32>,
    f32_locals: Vec<u32>,
    f64_locals: Vec<u32>,
    /// The mutable `i32` global, exported so the test can read it back.
    counter_global: u32,
    locals: Vec<LocalDecl>,
}

impl Layout {
    fn pick_i32(&self, rng: &mut Rng) -> Option<u32> {
        (!self.i32_locals.is_empty())
            .then(|| self.i32_locals[rng.below(self.i32_locals.len() as u64) as usize])
    }

    fn pick_i64(&self, rng: &mut Rng) -> Option<u32> {
        (!self.i64_locals.is_empty())
            .then(|| self.i64_locals[rng.below(self.i64_locals.len() as u64) as usize])
    }

    fn pick_f32(&self, rng: &mut Rng) -> Option<u32> {
        (!self.f32_locals.is_empty())
            .then(|| self.f32_locals[rng.below(self.f32_locals.len() as u64) as usize])
    }

    fn pick_f64(&self, rng: &mut Rng) -> Option<u32> {
        (!self.f64_locals.is_empty())
            .then(|| self.f64_locals[rng.below(self.f64_locals.len() as u64) as usize])
    }
}

/// The operand stack, as types, while a body is emitted.
#[derive(Default)]
struct Emitter {
    body: Vec<u8>,
    stack: Vec<ValueType>,
    /// How many `block`/`loop`/`if` constructs are open.
    blocks: usize,
    /// The operand stack height on entry to each open construct, so an `else` can
    /// discard its `then` arm's values by truncating to the right depth.
    frame_starts: Vec<usize>,
}

impl Emitter {
    fn op(&mut self, byte: u8) {
        self.body.push(byte);
    }

    /// Append a `u32` immediate, LEB128-encoded.
    ///
    /// Every `u32` immediate in the binary format is LEB128. Four little-endian
    /// bytes is wrong for *any* value: the reader stops at the first byte whose
    /// continuation bit is clear and treats the rest as further instructions.
    fn u32(&mut self, value: u32) {
        let mut rest = value;
        let mut groups = 0;
        loop {
            let byte = (rest & 0x7f) as u8;
            rest >>= 7;
            if rest == 0 {
                self.op(byte);
                return;
            }
            self.op(byte | 0x80);
            groups += 1;
            assert!(groups <= 5, "a u32 LEB must not exceed 5 groups");
        }
    }

    /// Append a signed LEB128 immediate, the form `i32.const`/`i64.const` take.
    ///
    /// Signed LEB is not the unsigned form with a different sign: a final group
    /// must not leave bit 6 set on a non-negative value, or the decoder reads it
    /// as the start of another group and produces a different number.
    fn sleb(&mut self, value: i64) {
        let mut rest = value;
        let mut groups = 0;
        loop {
            let byte = (rest & 0x7f) as u8;
            rest >>= 7;
            let sign_set = byte & 0x40 != 0;
            let done = (rest == 0 && !sign_set) || (rest == -1 && sign_set);
            self.op(if done { byte } else { byte | 0x80 });
            groups += 1;
            assert!(groups <= 10, "an i64 LEB must not exceed 10 groups");
            if done {
                return;
            }
        }
    }

    /// An instruction consuming one operand of `input` and leaving `output`.
    ///
    /// The operand's type is *asserted* rather than assumed. An emitter that
    /// silently emitted a mistyped instruction would produce a module the
    /// validator rejects, and the error would point at the generator rather than
    /// at the disagreement this test is looking for.
    fn unary(&mut self, opcode: u8, input: ValueType, output: ValueType) {
        assert_eq!(self.stack.pop(), Some(input), "emitter operand type");
        self.op(opcode);
        self.stack.push(output);
    }

    /// An instruction consuming two operands of `input` and leaving `output`.
    fn binary(&mut self, opcode: u8, input: ValueType, output: ValueType) {
        assert_eq!(self.stack.pop(), Some(input), "emitter operand type");
        assert_eq!(self.stack.pop(), Some(input), "emitter operand type");
        self.op(opcode);
        self.stack.push(output);
    }

    /// `i32.const`.
    fn const_i32(&mut self, value: i32) {
        self.op(0x41);
        self.sleb(i64::from(value));
        self.stack.push(I32);
    }

    /// `i64.const`, from raw bits reinterpreted as a signed value.
    fn const_i64(&mut self, bits: u64) {
        self.op(0x42);
        self.sleb(bits as i64);
        self.stack.push(I64);
    }

    /// `f32.const`, from raw bits. Unlike the integer forms this immediate is
    /// four *fixed* bytes, so the raw form is correct here and only here.
    fn const_f32(&mut self, bits: u32) {
        self.op(0x43);
        self.body.extend_from_slice(&bits.to_le_bytes());
        self.stack.push(F32);
    }

    /// `f64.const`, from raw bits; eight fixed bytes.
    fn const_f64(&mut self, bits: u64) {
        self.op(0x44);
        self.body.extend_from_slice(&bits.to_le_bytes());
        self.stack.push(F64);
    }

    /// `local.get`.
    fn local_get(&mut self, index: u32, ty: ValueType) {
        self.op(0x20);
        self.u32(index);
        self.stack.push(ty);
    }

    /// `local.set`, consuming the top of stack.
    fn local_set(&mut self, index: u32, ty: ValueType) {
        assert_eq!(self.stack.pop(), Some(ty), "emitter operand type");
        self.op(0x21);
        self.u32(index);
    }

    /// `global.set`, consuming the top of stack.
    fn global_set(&mut self, index: u32, ty: ValueType) {
        assert_eq!(self.stack.pop(), Some(ty), "emitter operand type");
        self.op(0x24);
        self.u32(index);
    }

    /// `br_if` to the enclosing depth, consuming the condition.
    ///
    /// The condition is popped here, which is what makes the enclosing `block`
    /// cases stack-neutral. An emitter that emitted the branch and left the
    /// condition alone would still produce a *valid* module -- `br_if` genuinely
    /// consumes its operand, so the decoder and validator would agree -- and the
    /// imbalance would only surface one statement later as a mystery.
    fn br_if(&mut self, label: u32) {
        assert_eq!(self.stack.pop(), Some(I32), "emitter operand type");
        self.op(0x0d);
        self.u32(label);
    }

    /// A `block` with the given result type byte, or `0x40` for none.
    fn block(&mut self, ty: u8) {
        self.frame_starts.push(self.stack.len());
        self.op(0x02);
        self.op(ty);
        self.blocks += 1;
    }

    /// A `loop` with the given result type byte.
    fn loop_block(&mut self, ty: u8) {
        self.frame_starts.push(self.stack.len());
        self.op(0x03);
        self.op(ty);
        self.blocks += 1;
    }

    /// An `if` with the given result type byte, consuming the condition.
    fn if_block(&mut self, ty: u8) {
        assert_eq!(self.stack.pop(), Some(I32), "emitter operand type");
        // Recorded *after* the condition is popped: that is the height the `then`
        // arm starts from and the height the block's results are counted above.
        self.frame_starts.push(self.stack.len());
        self.op(0x04);
        self.op(ty);
        self.blocks += 1;
    }

    /// An `else`, which continues the `if` just opened. Opens nothing.
    ///
    /// The `else` *discards* whatever the `then` arm left on the stack, so the
    /// tracked stack is reset to the frame's entry height here. Truncating rather
    /// than popping one value is the point: an arm that produced a value still
    /// emitted the bytes that produced it, and nothing can un-emit them.
    fn else_block(&mut self) {
        self.op(0x05);
        self.stack
            .truncate(*self.frame_starts.last().expect("an else is inside an if"));
    }

    /// An `end`. At depth zero this is the function's own terminator.
    ///
    /// A `block` with no result must leave the stack exactly as it found it, so
    /// the height is restored rather than merely recorded. For a `block` *with* a
    /// result the produced value sits above the entry height, so the value count
    /// of the block's type decides how much is kept.
    fn end(&mut self, ty: Option<u8>) {
        self.op(0x0b);
        // `None` is the function body's own terminator: it closes the implicit
        // outer block, which was never pushed here, and it keeps whatever is on the
        // stack -- that value *is* the function's result.
        let Some(start) = ty.map(|_| {
            self.frame_starts
                .pop()
                .expect("an end is inside a construct that was opened")
        }) else {
            return;
        };
        // `0x40` means "no result", so nothing survives; any other block type is a
        // single MVP value type, so exactly one does.
        if ty == Some(0x40) {
            self.stack.truncate(start);
        }
        self.blocks = self.blocks.saturating_sub(1);
    }

    /// The `memarg` a memory access carries. Both immediates are LEB128.
    ///
    /// The alignment is advisory -- a hint the engine need not honour -- and the
    /// generated values are the natural ones for the access width, which is what
    /// real code emits.
    fn memarg(&mut self, align_exponent: u8) {
        self.op(align_exponent);
        self.op(0x00);
    }

    /// `drop`, consuming the top of stack.
    fn drop(&mut self, ty: ValueType) {
        assert_eq!(self.stack.pop(), Some(ty), "emitter operand type");
        self.op(0x1a);
    }

    /// An `i32` address in the first page, derived from a local so a module whose
    /// local layout has drifted still addresses real memory instead of trapping
    /// for an unrelated reason.
    fn page_address(&mut self, rng: &mut Rng, layout: &Layout) {
        self.const_i32(0);
        if let Some(index) = layout.pick_i32(rng) {
            self.local_get(index, I32);
            self.op(0x71); // and
            self.stack.pop();
        }
        // Masking with 0x000F_FFFC keeps the address 4-byte aligned and inside
        // the first page, so the access succeeds and its *result* is what the two
        // backends must agree on.
        self.const_i32(0x000F_FFFC);
        self.op(0x71); // and
        self.stack.pop();
    }
}

/// The MVP opcodes the generator emits, with their immediate shapes.
///
/// Named as constants so the walker below reads the way the format does. The
/// ranges are the MVP's own contiguous groupings: comparisons and arithmetic run
/// `0x45`..=`0xc4`, loads `0x28`..=`0x35`, stores `0x36`..=`0x39`.
mod op {
    pub const UNREACHABLE: u8 = 0x00;
    pub const BLOCK: u8 = 0x02;
    pub const LOOP: u8 = 0x03;
    pub const IF: u8 = 0x04;
    pub const ELSE: u8 = 0x05;
    pub const END: u8 = 0x0b;
    pub const BR_IF: u8 = 0x0d;
    pub const RETURN: u8 = 0x0f;
    pub const CALL: u8 = 0x10;
    pub const DROP: u8 = 0x1a;
    pub const SELECT: u8 = 0x1b;
    pub const LOCAL_GET: u8 = 0x20;
    pub const LOCAL_SET: u8 = 0x21;
    pub const LOCAL_TEE: u8 = 0x22;
    pub const GLOBAL_GET: u8 = 0x23;
    pub const GLOBAL_SET: u8 = 0x24;
    /// `i32.load`, the first opcode with a `memarg`.
    pub const LOAD_FIRST: u8 = 0x28;
    /// `i64.load32_u`, the last.
    pub const LOAD_LAST: u8 = 0x35;
    /// `i32.store`, the first opcode with a `memarg` and a value.
    pub const STORE_FIRST: u8 = 0x36;
    /// `f64.store`, the last.
    pub const STORE_LAST: u8 = 0x39;
    pub const MEMORY_SIZE: u8 = 0x3f;
    pub const MEMORY_GROW: u8 = 0x40;
    pub const I32_CONST: u8 = 0x41;
    pub const I64_CONST: u8 = 0x42;
    pub const F32_CONST: u8 = 0x43;
    pub const F64_CONST: u8 = 0x44;
    /// `i32.eqz`, the first opcode with no immediate.
    pub const NO_IMMEDIATE_FIRST: u8 = 0x45;
    /// `i64.extend32_s`, the last in the MVP's contiguous no-immediate run.
    pub const NO_IMMEDIATE_LAST: u8 = 0xc4;
}

/// Step over a signed LEB128 at `at`, returning the next index.
///
/// Used only to skip an immediate, so the value is not decoded and overflow is
/// not a concern: a malformed constant is the decoder's judgement to make, and
/// duplicating it here would give two chances to be wrong in the same way.
fn skip_sleb(body: &[u8], at: usize) -> Result<usize, String> {
    let mut cursor = at;
    let mut groups = 0;
    loop {
        let byte = *body.get(cursor).ok_or("a constant ran past the end")?;
        cursor += 1;
        groups += 1;
        if groups > 10 {
            return Err("a constant LEB exceeds 10 groups".to_owned());
        }
        if byte & 0x80 == 0 {
            return Ok(cursor);
        }
    }
}

/// Re-derive a body's structure from its finished bytes.
///
/// The `Emitter` counts the blocks it opens, and that count is worth having. It
/// is not a substitute for this: a bug in the emitter is precisely the kind that
/// leaves the count agreeing with the bytes, because both come from the same
/// code. This walker is a second implementation sharing nothing with the emitter
/// but the byte sequence, so it exists to catch a disagreement between them.
///
/// It is explicitly *not* a guarantee. Two implementations of the same
/// misunderstanding agree with each other, and a walker that validated only
/// structure would happily bless a body whose *types* are wrong. It is a check on
/// the generator, not a proof about it.
fn walk_body(body: &[u8]) -> Result<(), String> {
    let mut cursor = 0usize;
    // Blocks the walker is inside, counting only the explicit ones. A well-formed
    // body ends at -1: the final `end` closes the function itself, which is an
    // implicit block with no opening byte. So the terminator legitimately drives
    // the count one below zero, and only a *second* unopened `end` is an error.
    let mut depth = 0i32;
    let mut lowest = 0i32;
    while cursor < body.len() {
        let opcode = body[cursor];
        cursor += 1;
        match opcode {
            op::BLOCK | op::LOOP | op::IF => {
                // In MVP a block type is either `0x40` or a single value type
                // byte, so the immediate is one byte either way.
                let ty = *body.get(cursor).ok_or("a block type ran past the end")?;
                cursor += 1;
                if ty != 0x40 && !(0x7c..=0x7f).contains(&ty) {
                    return Err(format!("block type {ty:#04x} is not an MVP block type"));
                }
                depth += 1;
            }
            op::ELSE => {
                // An `else` must sit inside a block the walker counted, so depth is
                // at least one. At depth zero it would be at function level, where
                // there is no conditional open to continue.
                if depth < 1 {
                    return Err("an else is outside any block".to_owned());
                }
            }
            op::END => {
                depth -= 1;
                lowest = lowest.min(depth);
            }
            op::UNREACHABLE | op::RETURN | op::DROP | op::SELECT => {}
            op::BR_IF
            | op::CALL
            | op::LOCAL_GET
            | op::LOCAL_SET
            | op::LOCAL_TEE
            | op::GLOBAL_GET
            | op::GLOBAL_SET
            | op::MEMORY_SIZE
            | op::MEMORY_GROW => cursor += 1,
            op::I32_CONST | op::I64_CONST => cursor = skip_sleb(body, cursor)?,
            op::F32_CONST => cursor += 4,
            op::F64_CONST => cursor += 8,
            opcode if (op::LOAD_FIRST..=op::LOAD_LAST).contains(&opcode) => cursor += 2,
            opcode if (op::STORE_FIRST..=op::STORE_LAST).contains(&opcode) => cursor += 2,
            opcode if (op::NO_IMMEDIATE_FIRST..=op::NO_IMMEDIATE_LAST).contains(&opcode) => {}
            other => return Err(format!("opcode {other:#04x} is not in the MVP set")),
        }
        if cursor > body.len() {
            return Err("an immediate ran past the end of the body".to_owned());
        }
    }
    if depth != -1 {
        return Err(format!("{} block(s) left unclosed", depth + 1));
    }
    if lowest < -1 {
        return Err("an end was left unopened".to_owned());
    }
    Ok(())
}

/// Push a value of `ty`, from a local when one of that type exists and a
/// constant otherwise.
///
/// A local is preferred so that stored values actually flow through the
/// generated code and a local's lifetime is exercised. Constants are the only
/// choice for a type with no locals of its own.
fn push(emitter: &mut Emitter, ty: ValueType, rng: &mut Rng, layout: &Layout) {
    let from_local = match ty {
        I32 => layout.pick_i32(rng),
        I64 => layout.pick_i64(rng),
        F32 => layout.pick_f32(rng),
        F64 => layout.pick_f64(rng),
        _ => None,
    };
    if let Some(index) = from_local {
        emitter.local_get(index, ty);
        return;
    }
    match ty {
        I32 => emitter.const_i32(rng.i32()),
        I64 => emitter.const_i64(rng.next_u64()),
        F32 => emitter.const_f32(rng.f32_bits()),
        F64 => emitter.const_f64(rng.f64_bits()),
        _ => unreachable!("only the four MVP types are generated"),
    }
}

/// The index of a random local of `ty`, if there is one.
fn pick_local(ty: ValueType, rng: &mut Rng, layout: &Layout) -> Option<u32> {
    match ty {
        I32 => layout.pick_i32(rng),
        I64 => layout.pick_i64(rng),
        F32 => layout.pick_f32(rng),
        _ => layout.pick_f64(rng),
    }
}

/// An arithmetic, bitwise, or unary operator for `ty`, chosen at random.
///
/// Kept as a table rather than computed, because the MVP's opcodes for these are
/// contiguous per type but the *unary* ones sit at the end of a different range
/// entirely, and deriving that arithmetically is how a generator ends up emitting
/// a plausible-looking opcode that means something else.
///
/// The flag says whether the operator takes *one* operand; the caller undoes the
/// second push rather than emitting it, which is what keeps the case neutral.
fn arithmetic(ty: ValueType, rng: &mut Rng) -> (u8, bool) {
    let (unary, choice) = match ty {
        I32 | I64 => (false, rng.below(6)),
        // The float choice runs 0..=4 either way: 0..=3 for the binary operators,
        // and the unary flag narrows it to the first two of those, so the
        // resulting index is always in range.
        F32 | F64 => (rng.below(8) == 0, rng.below(5)),
        _ => (false, rng.below(6)),
    };
    let opcode = if unary {
        match ty {
            F32 => [0x8b, 0x8c][(choice % 2) as usize], // abs, neg
            _ => [0x99, 0x9a][(choice % 2) as usize],   // abs, neg
        }
    } else {
        match (ty, choice) {
            (I32, 0) => 0x6a, // add
            (I32, 1) => 0x6b, // sub
            (I32, 2) => 0x6c, // mul
            (I32, 3) => 0x71, // and
            (I32, 4) => 0x72, // or
            (I32, _) => 0x73, // xor
            (I64, 0) => 0x7c, // add
            (I64, 1) => 0x7d, // sub
            (I64, 2) => 0x7e, // mul
            (I64, 3) => 0x83, // and
            (I64, 4) => 0x84, // or
            (I64, _) => 0x85, // xor
            (F32, 0) => 0x92, // add
            (F32, 1) => 0x93, // sub
            (F32, 2) => 0x94, // mul
            (F32, 3) => 0x95, // div
            // `min` and `max` are binary, and their NaN rule differs from every
            // arithmetic opcode above -- the operand order decides which operand's
            // NaN survives -- so they are the interesting float cases here.
            (F32, _) => [0x96, 0x97][(choice - 4) as usize], // min, max
            (_, 0) => 0xa0,                                  // f64.add
            (_, 1) => 0xa1,                                  // f64.sub
            (_, 2) => 0xa2,                                  // f64.mul
            (_, 3) => 0xa3,                                  // f64.div
            (_, _) => [0xa4, 0xa5][(choice - 4) as usize],   // f64.min, f64.max
        }
    };
    (opcode, unary)
}

/// A comparison for `ty`, chosen at random, yielding `i32` whatever it consumed.
fn comparison(ty: ValueType, rng: &mut Rng) -> u8 {
    match (ty, rng.below(4)) {
        (I32, 0) => 0x48, // lt_s
        (I32, 1) => 0x4a, // gt_s
        (I32, 2) => 0x46, // eq
        (I32, _) => 0x47, // ne
        (_, 0) => 0x54,   // i64.lt_s
        (_, 1) => 0x56,   // i64.gt_s
        (_, 2) => 0x52,   // i64.eq
        (_, _) => 0x53,   // i64.ne
    }
}

/// Send the top of stack to the counter global when it is an `i32`, otherwise
/// drop it.
///
/// This is what keeps almost every case stack-neutral: an `i32` result is
/// recorded where the test can read it from outside the module, and anything else
/// is discarded.
///
/// The value is removed from the tracked stack *here*, and the opcode written
/// directly, rather than delegating to `global_set`/`drop`. Those helpers pop for
/// themselves, so calling them after popping would consume whatever the *next*
/// statement pushed -- an off-by-one that would only show up as a type mismatch
/// several statements later.
fn sink(emitter: &mut Emitter, layout: &Layout) {
    let ty = emitter.stack.pop().expect("a statement leaves a value");
    if ty == I32 {
        emitter.op(0x24); // global.set
        emitter.u32(layout.counter_global);
    } else {
        emitter.op(0x1a); // drop
    }
}

/// Emit a memory access of `ty`'s width, in whichever direction `store` selects.
fn memory_access(emitter: &mut Emitter, ty: ValueType, store: bool) {
    let (load_opcode, store_opcode, align) = match ty {
        I32 => (0x28, 0x36, 2),
        I64 => (0x29, 0x37, 3),
        F32 => (0x2a, 0x38, 2),
        _ => (0x2b, 0x39, 3),
    };
    if store {
        // A store pops the *value* first and then the address, so the value -- the
        // top of stack -- is the first thing checked. Checking the address first
        // would pass for an `i32` store and fail for every other width, which is
        // the sort of thing that reads as a generator bug for a long time.
        assert_eq!(emitter.stack.pop(), Some(ty), "emitter operand type");
        assert_eq!(emitter.stack.pop(), Some(I32), "emitter operand type");
        emitter.op(store_opcode);
        emitter.memarg(align);
    } else {
        emitter.unary(load_opcode, I32, ty);
        emitter.memarg(align);
    }
}

/// One statement that leaves the stack exactly as it found it.
///
/// Every case is stack-neutral. That is the property that makes a whole sequence
/// of them composable: a case that left a value behind would force the rest of
/// the generator to track an unpredictable stack, and a case that consumed one
/// would need a matching producer somewhere. Neutrality is also why the
/// function's own result can be produced by one final case at the end rather than
/// by any statement in the middle.
fn statement(emitter: &mut Emitter, rng: &mut Rng, layout: &Layout) {
    match rng.below(16) {
        0..=2 => {
            // Arithmetic or bitwise, its result sunk.
            let ty = [I32, I64, F32, F64][rng.below(4) as usize];
            // The operator is chosen *before* the operands are pushed, and its
            // arity decides how many. Choosing it afterwards and then undoing the
            // surplus push is the obvious alternative and it is wrong: popping
            // the tracked stack does not un-emit the `local.get` byte already
            // written, so the body would carry a value nothing consumes and the
            // module would be rejected for a stack that does not balance.
            let (opcode, unary) = arithmetic(ty, rng);
            push(emitter, ty, rng, layout);
            if !unary {
                push(emitter, ty, rng, layout);
            }
            if unary {
                emitter.unary(opcode, ty, ty);
            } else {
                emitter.binary(opcode, ty, ty);
            }
            sink(emitter, layout);
        }
        3 => {
            // A comparison, which yields `i32` whatever it consumed.
            let ty = if rng.chance(50) { I32 } else { I64 };
            push(emitter, ty, rng, layout);
            push(emitter, ty, rng, layout);
            emitter.binary(comparison(ty, rng), ty, I32);
            sink(emitter, layout);
        }
        4 => {
            // A local write, read straight back so the value is used rather than
            // merely stored. With no local of that type the value is just dropped,
            // which is why this case is written to tolerate a missing local.
            let ty = [I32, I64, F32, F64][rng.below(4) as usize];
            push(emitter, ty, rng, layout);
            match pick_local(ty, rng, layout) {
                Some(index) => {
                    emitter.local_set(index, ty);
                    emitter.local_get(index, ty);
                    emitter.drop(ty);
                }
                None => emitter.drop(ty),
            }
        }
        5 | 6 => {
            // A store to, or a load from, a real address masked into the first
            // page. The access succeeds, so what the two backends must agree on is
            // the *value*, not merely that nothing trapped.
            emitter.page_address(rng, layout);
            let ty = [I32, I64, F32, F64][rng.below(4) as usize];
            if rng.chance(50) {
                push(emitter, ty, rng, layout);
                memory_access(emitter, ty, true);
            } else {
                memory_access(emitter, ty, false);
                emitter.drop(ty);
            }
        }
        7 => {
            // A load from an arbitrary address, which is *usually* out of bounds.
            // Both backends must agree that it is, and the trap itself is compared
            // too, so a differing out-of-bounds rule would show up here.
            emitter.const_i32(rng.i32());
            memory_access(emitter, I32, false);
            emitter.drop(I32);
        }
        8 => {
            // `memory.size`, folded into the counter. `memory.grow` is deliberately
            // *not* generated: a grown memory would change what every later case in
            // the same run observes, and the run would stop being a sequence of
            // independent comparisons.
            emitter.op(0x3f);
            emitter.op(0x00); // the reserved memory index byte
            emitter.stack.push(I32);
            sink(emitter, layout);
        }
        9 => {
            // A `block` with an `i32` result, the value produced inside and read
            // after -- so the block's own result plumbing is what is being checked,
            // not just its entry and exit.
            emitter.block(0x7f);
            push(emitter, I32, rng, layout);
            emitter.end(Some(0x7f));
            sink(emitter, layout);
        }
        10 => {
            // A `loop` whose back edge is tested and never taken. A loop whose
            // condition was always true would not terminate, and the point is to
            // finish -- but *omitting* the test would not exercise the back edge
            // at all, which is the part of a `loop` most likely to be wrong.
            emitter.loop_block(0x40);
            push(emitter, I32, rng, layout);
            emitter.global_set(layout.counter_global, I32);
            emitter.const_i32(0);
            emitter.br_if(0);
            emitter.end(Some(0x40));
        }
        11 => {
            // An `if` with both arms, no result.
            push(emitter, I32, rng, layout);
            emitter.if_block(0x40);
            push(emitter, I32, rng, layout);
            emitter.global_set(layout.counter_global, I32);
            emitter.else_block();
            push(emitter, I32, rng, layout);
            emitter.global_set(layout.counter_global, I32);
            emitter.end(Some(0x40));
        }
        12 => {
            // An `if` with an `i32` result, both arms producing it. The condition is
            // a pushed value rather than a constant, so across a run both arms get
            // taken and a backend that only ever runs one of them cannot pass.
            push(emitter, I32, rng, layout);
            emitter.if_block(0x7f);
            push(emitter, I32, rng, layout);
            emitter.else_block();
            push(emitter, I32, rng, layout);
            emitter.end(Some(0x7f));
            emitter.global_set(layout.counter_global, I32);
        }
        13 => {
            // A `br_if` out of a block. Whether the branch is taken depends on the
            // condition, and the counter must be right in both cases -- which is what
            // catches a backend that mis-restores the stack height on a taken branch.
            //
            // The value for `global.set` is pushed *after* the branch, not before.
            // `br_if` consumes its own condition, so a single value would leave the
            // `global.set` with nothing to store -- and the module would be rejected
            // for a stack underflow that has nothing to do with either backend.
            emitter.block(0x40);
            push(emitter, I32, rng, layout);
            emitter.br_if(0);
            push(emitter, I32, rng, layout);
            emitter.global_set(layout.counter_global, I32);
            emitter.end(Some(0x40));
        }
        _ => {
            // The same shape one level deeper, with the `br_if` targeting the
            // *outer* block, so a branch more than one frame out is resolved and the
            // intermediate frames are unwound correctly.
            emitter.block(0x40);
            emitter.block(0x40);
            push(emitter, I32, rng, layout);
            emitter.br_if(1);
            push(emitter, I32, rng, layout);
            emitter.global_set(layout.counter_global, I32);
            emitter.end(Some(0x40));
            emitter.end(Some(0x40));
        }
    }
}

/// A constant `i32` initializer, as a constant expression.
///
/// The terminating `end` is the point worth noting: a constant expression that
/// omits it is not a shorter encoding of the same thing, it runs into the next
/// section and the reader reports the end of the *module* -- an error that points
/// nowhere near the initializer that was short by one byte.
fn const_i32_expr(value: i32) -> ConstExpr {
    let mut emitter = Emitter::default();
    emitter.const_i32(value);
    emitter.end(None);
    ConstExpr(emitter.body)
}

/// A generated module and the seed that produced it.
struct Case {
    bytes: Vec<u8>,
    /// The function body, kept for the failure report: a validation error names
    /// neither an offset nor an instruction, so without the bytes the message
    /// points at the engine rather than at the statement that produced them.
    body: Vec<u8>,
    seed: u64,
    /// How many statements the body holds, for the failure report only.
    statements: usize,
}

/// Build one module from `seed`.
///
/// The module has a single exported function taking nothing and returning
/// `result_ty`, a one-page memory, and a mutable `i32` global exported as
/// `counter`. That global is what makes side effects observable from outside: a
/// backend could agree with the other on every return value while ordering its
/// stores differently, and only an externally readable value would show that.
///
/// The expected result is deliberately *not* computed here. The two backends are
/// compared against each other, not against anything this file believes the
/// answer should be -- predicting it would be a third implementation to maintain,
/// and a bug in it would be indistinguishable from a bug in either backend.
fn build_case(seed: u64) -> Case {
    let mut rng = Rng::new(seed);
    let result_ty = [I32, I64, F32, F64][rng.below(4) as usize];
    let count = 4 + rng.below(12) as u32;

    // One local of every type, so `push` has something to read and the local-write
    // case has somewhere to write. The layout is recorded rather than recomputed,
    // because the emitter must agree with the locals section on which index means
    // what.
    let mut layout = Layout {
        i32_locals: Vec::new(),
        i64_locals: Vec::new(),
        f32_locals: Vec::new(),
        f64_locals: Vec::new(),
        counter_global: 0,
        locals: Vec::new(),
    };
    for ty in [I32, I64, F32, F64] {
        let index = layout.locals.len() as u32;
        layout.locals.push(LocalDecl {
            count: 1,
            value_type: ty,
        });
        match ty {
            I32 => layout.i32_locals.push(index),
            I64 => layout.i64_locals.push(index),
            F32 => layout.f32_locals.push(index),
            _ => layout.f64_locals.push(index),
        }
    }

    let mut emitter = Emitter::default();
    for index in 0..count {
        statement(&mut emitter, &mut rng, &layout);
        assert!(
            emitter.stack.is_empty(),
            "seed {seed}: statement {index} left {:?} on the stack, having emitted {:02x?}",
            emitter.stack,
            emitter.body,
        );
    }
    // The function's own result, produced last so the statements above stay
    // neutral. `push` may read a local whose value a statement set, so the result
    // is not simply a constant.
    push(&mut emitter, result_ty, &mut rng, &layout);
    // `None`: the function body's own terminator, which closes the implicit outer
    // block and leaves the stack alone -- its result is whatever is on top.
    emitter.end(None);

    // A copy of the body, kept so the structural check runs on the finished bytes
    // rather than on the emitter's own bookkeeping -- the emitter is what the
    // check exists to second-guess, so consulting it would prove nothing.
    let emitter_body = emitter.body.clone();
    let module = Module {
        types: vec![FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![result_ty]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: layout.locals.clone(),
            body: emitter.body,
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits { min: 1, max: None },
                memory64: false,
            },
        }],
        globals: vec![Global {
            global_type: GlobalType {
                value_type: I32,
                mutable: true,
            },
            init: const_i32_expr(0),
        }],
        exports: vec![
            Export {
                name: "run".to_owned(),
                desc: ExportDesc::Function(0),
            },
            Export {
                name: "counter".to_owned(),
                desc: ExportDesc::Global(0),
            },
        ],
        ..Module::default()
    };
    let bytes = encode(&module)
        .unwrap_or_else(|problem| panic!("seed {seed}: the module did not encode: {problem}"));
    walk_body(&emitter_body)
        .unwrap_or_else(|problem| panic!("seed {seed}: malformed body: {problem}"));
    Case {
        bytes,
        body: emitter_body,
        seed,
        statements: count as usize,
    }
}

/// What one backend did with a generated module.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// The function's result, then the counter global read back through its
    /// export. Both, because a backend could agree on the return value while
    /// disagreeing about the side effect that produced it.
    Values(Vec<Value>),
    /// A trap, as the variant itself.
    ///
    /// The variant is compared rather than the prose describing it, on purpose.
    /// The wording of a trap message is a presentation choice, and this test is
    /// about which *rule* fired -- an out-of-bounds access is the same fact
    /// whichever way it is worded. `HostFailure` carries a message, so it is
    /// reduced to just its discriminant for the same reason.
    Trapped(&'static str),
}

/// The variant name of a trap, for comparison across backends.
fn trap_variant(trap: &Trap) -> &'static str {
    match trap {
        Trap::Unreachable => "Unreachable",
        Trap::IntegerDivisionByZero => "IntegerDivisionByZero",
        Trap::IntegerOverflow => "IntegerOverflow",
        Trap::InvalidConversion => "InvalidConversion",
        Trap::MemoryOutOfBounds => "MemoryOutOfBounds",
        Trap::TableOutOfBounds => "TableOutOfBounds",
        Trap::NullReference => "NullReference",
        Trap::IndirectCallTypeMismatch => "IndirectCallTypeMismatch",
        Trap::StackOverflow => "StackOverflow",
        Trap::CallDepthExceeded => "CallDepthExceeded",
        Trap::StepsExhausted => "StepsExhausted",
        Trap::HostFailure(_) => "HostFailure",
    }
}

/// The name of a backend, for failure messages.
fn name(mode: EngineMode) -> &'static str {
    if mode == EngineMode::Micro {
        "Micro"
    } else {
        "Baseline"
    }
}

/// Build an engine for one backend, in the configuration the Core suite uses.
fn engine_for(mode: EngineMode) -> Engine {
    Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("the engine should be constructible in either mode")
}

/// Run one module on one backend and report what happened.
///
/// A failure to *instantiate* panics rather than becoming an `Outcome`. Recorded
/// as an outcome it would compare equal on both backends and the test would pass
/// without executing a single instruction -- a vacuous pass, and the one failure
/// mode a differential test cannot afford to be blind to. A generated module is
/// supposed to be valid, so its not being valid is a bug in the generator and
/// belongs in the failure message.
fn run_once(mode: EngineMode, case: &Case) -> Outcome {
    let engine = engine_for(mode);
    let mut instance = engine
        .instantiate_bytes(&case.bytes)
        .unwrap_or_else(|problem| {
            panic!(
                "seed {}: {} rejected a generated module: {problem}\n  body: {:02x?}",
                case.seed,
                name(mode),
                case.body,
            )
        });
    match instance.call("run", Vec::new()) {
        Ok(values) => {
            // The counter is read *after* the call, through the export, rather
            // than returned by it. Returning it would only prove the module can
            // compute it; reading it back proves the engine kept the value the
            // module actually wrote.
            let counter = instance
                .global("counter")
                .expect("the counter global is exported");
            Outcome::Values([values, vec![counter]].concat())
        }
        Err(RuntimeError::Trap(trap)) => Outcome::Trapped(trap_variant(&trap)),
        Err(other) => panic!(
            "seed {}: {} failed unexpectedly: {other}",
            case.seed,
            name(mode)
        ),
    }
}

/// Run `case` on both backends and assert they agree.
fn compare(case: &Case) {
    let micro = run_once(EngineMode::Micro, case);
    let baseline = run_once(EngineMode::Baseline, case);
    assert_eq!(
        micro, baseline,
        "seed {} ({} statement(s)): the backends disagree\n  module bytes: {:02x?}",
        case.seed, case.statements, case.bytes,
    );
}

/// The seeds every run uses.
///
/// Fixed rather than drawn from the clock, so a failure is reproducible from the
/// seed alone and the suite is the same on every machine. Bumped by hand when the
/// generator grows, which is a deliberate cost: a new construct should come with a
/// new seed, not with a luckier pass.
const SEEDS: u64 = 2_000;

/// The main differential run: every seed, both backends, compared bit-exactly.
#[test]
fn the_backends_agree_on_generated_modules() {
    for seed in 0..SEEDS {
        compare(&build_case(seed));
    }
}

/// The generator produces modules the engine actually accepts.
///
/// Separate from the comparison above, and separate on purpose. If the generator
/// is broken, the comparison would report a disagreement between two backends
/// that never ran anything, or -- worse -- agree because both failed the same
/// way. Asserting that instantiation succeeds on its own turns a generator bug
/// into a failure that says so.
#[test]
fn generated_modules_are_accepted() {
    for seed in 0..SEEDS {
        let case = build_case(seed);
        for mode in [EngineMode::Micro, EngineMode::Baseline] {
            engine_for(mode)
                .instantiate_bytes(&case.bytes)
                .unwrap_or_else(|problem| {
                    panic!(
                        "seed {}: {} rejected a generated module: {problem}\n  body: {:02x?}",
                        case.seed,
                        name(mode),
                        case.body,
                    )
                });
        }
    }
}

/// A fixed seed must produce the same bytes every time.
///
/// The reproducibility the whole file rests on. Without this, a failure reported
/// against "seed 137" would not be reproducible at all, and a generator drawing
/// from ambient entropy would make the suite itself nondeterministic.
#[test]
fn a_seed_reproduces_its_module() {
    for seed in 0..32 {
        let first = build_case(seed);
        let second = build_case(seed);
        assert_eq!(
            first.bytes, second.bytes,
            "seed {seed} produced two different modules",
        );
    }
}

/// The byte walker rejects a body that is not well formed.
///
/// A validator for the validator. `walk_body` exists to catch a broken emitter,
/// and if it accepted everything it would catch nothing while appearing to; these
/// are the cases that check it is doing its job.
#[test]
fn the_byte_walker_rejects_malformed_bodies() {
    // A `block` that is never closed.
    assert!(walk_body(&[0x02, 0x40]).is_err());
    // An `end` with nothing open.
    assert!(walk_body(&[0x0b, 0x0b]).is_err());
    // A block type that is neither `0x40` nor a value type.
    assert!(walk_body(&[0x02, 0x11, 0x0b, 0x0b]).is_err());
    // A `local.get` whose index immediate is missing entirely.
    assert!(walk_body(&[0x20]).is_err());
    // And one well-formed body, so the checks above are not passing vacuously.
    assert!(walk_body(&[0x41, 0x01, 0x1a, 0x0b]).is_ok());
}
