// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! x86-64 code generated from the machine IR.
//!
//! This encoder reads a [`MirFunction`], not the IR, so the native path is
//! `IR -> machine IR -> x86-64`. It handles the same `i32` and `i64` slices as the machine IR, and it
//! is deliberately simple: every slot lives in its own 8-byte frame slot, and each operation loads
//! its operands into `rax`, `rcx` and `rdx`, computes, and stores the result back. Nothing is kept in
//! a register across operations. That costs speed, and it makes each encoding easy to check against
//! the manual, which is the point of this native stage.
//!
//! A slot is a whole 64-bit word, so operands and results move as words. An `i32` value is the low
//! half of its word, and the instructions that compute one are the 32-bit forms, which read only that
//! half. The few places that need an `i32` as a full unsigned value (an address, a table index, a
//! growth count, a host argument) load it with a 32-bit move, which zero-extends it.
//!
//! The code follows the same convention as the existing encoders, so it is run through the same
//! `CompiledFunction`: the function receives a pointer to its arguments (eight-byte entries) and a
//! context pointer, and returns the result in `rax`. A successful run leaves the status word in the
//! context at zero, so the value is reported. A division or a memory access that traps writes its
//! status there, returns zero, and the caller reads the trap from the status, as the other encoders
//! do. A call goes through the context's function table and counts against its depth, as the other
//! encoders' calls do; it is refused unless the callee is compiled. A trap terminator, such as
//! `unreachable`, writes its status the same way.
//!
//! The encodings are the standard ones and are listed next to each emitter.

use tpt_wasm_ir::{FloatComparison, IntComparison, IntUnary};

use crate::mir::{
    BinOp, DivOp, FloatOp, FloatUnaryOp, IntKind, LoadOp, MirFunction, MirOp, MirTerm, Precision,
    Slot, StoreOp, WordWidth, CANONICAL_NAN_F64,
};
use crate::shared::{self, trap_status, widest_call, Abi, Environment};
use crate::{
    JitError, STATUS_CALL_DEPTH, STATUS_DIVIDE_BY_ZERO, STATUS_GLOBAL_OUT_OF_BOUNDS,
    STATUS_INDIRECT_TYPE_MISMATCH, STATUS_INVALID_CONVERSION, STATUS_MEMORY_OUT_OF_BOUNDS,
    STATUS_NULL_REFERENCE, STATUS_OVERFLOW, STATUS_TABLE_OUT_OF_BOUNDS,
};

/// Register numbers in the ModR/M encoding.
const EAX: u8 = 0;
const ECX: u8 = 1;
const EDX: u8 = 2;

/// The byte offset of the status word in the context, which the trap sequence writes.
const STATUS_OFFSET: u8 = std::mem::offset_of!(crate::Context, status) as u8;

/// The byte offsets of the memory base and length in the context.
const MEMORY_OFFSET: u8 = std::mem::offset_of!(crate::Context, memory) as u8;
const MEMORY_LENGTH_OFFSET: u8 = std::mem::offset_of!(crate::Context, memory_len) as u8;
const MEMORY_CAPACITY_OFFSET: u8 = std::mem::offset_of!(crate::Context, memory_capacity) as u8;

/// The byte offsets of the call depth, its limit, and the function table, in the context.
const DEPTH_OFFSET: u8 = std::mem::offset_of!(crate::Context, depth) as u8;
const DEPTH_LIMIT_OFFSET: u8 = std::mem::offset_of!(crate::Context, depth_limit) as u8;
const FUNCTIONS_OFFSET: u8 = std::mem::offset_of!(crate::Context, functions) as u8;

/// The byte offsets of the table's length, types and entries, in the context.
const TABLE_LEN_OFFSET: u8 = std::mem::offset_of!(crate::Context, table_len) as u8;
const TABLE_TYPES_OFFSET: u8 = std::mem::offset_of!(crate::Context, table_types) as u8;
const TABLE_ENTRIES_OFFSET: u8 = std::mem::offset_of!(crate::Context, table_entries) as u8;

/// The byte offset of the host trampoline's address, in the context.
const HOST_CALL_OFFSET: u8 = std::mem::offset_of!(crate::Context, host_call) as u8;

/// The byte offsets of the globals array and its length, in the context.
const GLOBALS_OFFSET: u8 = std::mem::offset_of!(crate::Context, globals) as u8;
const GLOBALS_LENGTH_OFFSET: u8 = std::mem::offset_of!(crate::Context, globals_len) as u8;

/// Where the words of a call are written. Each of the two areas, the arguments and the results, is
/// `len` eight-byte entries: area `block` for the arguments and `results_block` for the results. The
/// callee or the host reads entry `i` at `8 * i` from the pointer, so entry `i` is the slot numbered
/// `area + len - 1 - i`, which keeps the entries ascending in memory. `entry` is a slot that holds
/// the entry point of an indirect call while its arguments are set.
struct Calls {
    block: Slot,
    results_block: Slot,
    len: u32,
    entry: Slot,
    abi: Abi,
    /// The signature id of each module type, as the table records them, by type index.
    type_ids: Vec<i32>,
}

struct Code {
    bytes: Vec<u8>,
    /// Byte offset of each machine block, once it is emitted.
    labels: Vec<Option<usize>>,
    /// Positions of 32-bit relative displacements, and the block each one jumps to.
    fixups: Vec<(usize, usize)>,
}

impl Code {
    /// The displacement from `rbp` of a slot: slot `i` is the eight bytes below `rbp - 8 * i`.
    fn slot(slot: u32) -> i32 {
        -8 * (slot as i32 + 1)
    }

    /// `opcode` with a `[rbp + disp32]` operand and `reg` in the ModR/M reg field.
    fn with_frame_operand(&mut self, opcode: &[u8], reg: u8, disp: i32) {
        self.bytes.extend_from_slice(opcode);
        self.bytes.push(0x80 | (reg << 3) | 0b101); // mod = 10, rm = rbp
        self.bytes.extend_from_slice(&disp.to_le_bytes());
    }

    /// `mov reg64, [slot]`: `REX.W 8B /r`. The whole word.
    fn load(&mut self, reg: u8, slot: Slot) {
        self.with_frame_operand(&[0x48, 0x8B], reg, Self::slot(slot));
    }

    /// `mov reg32, [slot]`: `8B /r`. The low half, zero-extended into the register.
    fn load32(&mut self, reg: u8, slot: Slot) {
        self.with_frame_operand(&[0x8B], reg, Self::slot(slot));
    }

    /// `mov [slot], reg64`: `REX.W 89 /r`. The whole word.
    fn store(&mut self, reg: u8, slot: Slot) {
        self.with_frame_operand(&[0x48, 0x89], reg, Self::slot(slot));
    }

    /// An instruction whose last field is a displacement to `block`, filled in later.
    fn branch(&mut self, opcode: &[u8], block: usize) {
        self.bytes.extend_from_slice(opcode);
        self.fixups.push((self.bytes.len(), block));
        self.bytes.extend_from_slice(&[0; 4]);
    }

    /// `op eax, ecx` in two-byte register form: `op` takes `r/m32, r32` with `rm = eax`, `reg = ecx`.
    fn register_pair(&mut self, opcode: u8) {
        self.bytes.extend_from_slice(&[opcode, 0xC8]);
    }
}

/// Encode a machine-IR function for x86-64 under `abi`, returning the code bytes.
///
/// # Errors
///
/// [`JitError::Unsupported`] for a trap terminator, an empty
/// function, a `memory.grow` when the environment does not reserve memory, or a call to a function
/// the environment does not mark as callable.
///
/// A reserved memory means the code grows the memory in place, up to the capacity in the context,
/// which is the protocol the caller must honour.
pub(crate) fn encode(
    function: &MirFunction,
    abi: Abi,
    env: &Environment,
) -> Result<Vec<u8>, JitError> {
    if function.blocks.is_empty() {
        return Err(JitError::Unsupported(
            "a machine function with no blocks".into(),
        ));
    }
    shared::check_environment(function, env)?;
    let widest = widest_call(function);
    let mut code = Code {
        bytes: Vec::new(),
        labels: vec![None; function.blocks.len()],
        fixups: Vec::new(),
    };
    let calls = Calls {
        block: function.slots + 2,
        results_block: function.slots + 2 + widest,
        len: widest,
        entry: function.slots + 2 + 2 * widest,
        abi,
        // Only the ids of types an indirect call names are compared, and those fit, as checked above.
        type_ids: env
            .type_ids
            .iter()
            .map(|id| i32::try_from(*id).unwrap_or(0))
            .collect(),
    };

    // push rbp; mov rbp, rsp; sub rsp, frame. The frame holds every slot, then the saved argument
    // pointer, the saved context pointer, the argument and result entries of a call, and the entry
    // of an indirect call. Below all of those sit the 32 bytes of shadow space that a Windows callee
    // may use, so the frame is at least that much larger, rounded up to 16 bytes so the stack stays
    // aligned at calls.
    let frame = ((8 * (function.slots as i32 + 3 + 2 * widest as i32)) + 32 + 15) & !15;
    code.bytes.push(0x55);
    code.bytes.extend_from_slice(&[0x48, 0x89, 0xE5]);
    code.bytes.extend_from_slice(&[0x48, 0x81, 0xEC]);
    code.bytes.extend_from_slice(&frame.to_le_bytes());

    // Every slot starts at zero, as the machine IR does: a declared local is read before it is
    // written, and WebAssembly defines that read as zero. `mov qword [rbp + disp], 0`: REX.W C7 /0.
    for slot in 0..function.slots {
        code.with_frame_operand(&[0x48, 0xC7], 0, Code::slot(slot));
        code.bytes.extend_from_slice(&0_i32.to_le_bytes());
    }

    // Keep the argument pointer in the slot just past the last one, and the context pointer in the
    // next one: `mov [rbp + disp], args` and `mov [rbp + disp], context`.
    let arguments_slot = function.slots;
    let context_slot = function.slots + 1;
    code.with_frame_operand(
        &[0x48, 0x89],
        abi.args_register(),
        Code::slot(arguments_slot),
    );
    code.with_frame_operand(
        &[0x48, 0x89],
        abi.context_register(),
        Code::slot(context_slot),
    );

    // Each parameter: `mov rax, [rbp + args]` then `mov rax, [rax + 8 * i]`, then into its slot.
    for (index, slot) in function.params.iter().enumerate() {
        code.with_frame_operand(&[0x48, 0x8B], EAX, Code::slot(arguments_slot));
        code.bytes.extend_from_slice(&[0x48, 0x8B, 0x80]); // mov rax, [rax + disp32]
        code.bytes
            .extend_from_slice(&(8 * index as i32).to_le_bytes());
        code.store(EAX, *slot);
    }
    // jmp entry
    code.branch(&[0xE9], function.entry);

    for (index, block) in function.blocks.iter().enumerate() {
        code.labels[index] = Some(code.bytes.len());
        for op in &block.ops {
            emit_op(&mut code, op, context_slot, &calls);
        }
        match &block.term {
            MirTerm::Jump(target) => code.branch(&[0xE9], *target),
            MirTerm::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // The condition is an `i32`, so its low half decides the branch.
                code.load(EAX, *cond);
                code.bytes.extend_from_slice(&[0x85, 0xC0]); // test eax, eax
                code.branch(&[0x0F, 0x85], *then_block); // jnz then
                code.branch(&[0xE9], *else_block); // jmp else
            }
            MirTerm::Return(values) => {
                match values.as_slice() {
                    [] => code.bytes.extend_from_slice(&[0x31, 0xC0]), // xor eax, eax
                    [value] => code.load(EAX, *value),
                    // Several values go to the argument block: `mov [rcx + 8 * j], rax`, with the
                    // argument pointer reloaded into rcx. The return register is then zero.
                    _ => {
                        code.with_frame_operand(&[0x48, 0x8B], ECX, Code::slot(arguments_slot));
                        for (index, value) in values.iter().enumerate() {
                            code.load(EAX, *value);
                            code.bytes.extend_from_slice(&[0x48, 0x89, 0x81]); // mov [rcx + disp32], rax
                            code.bytes
                                .extend_from_slice(&(8 * index as i32).to_le_bytes());
                        }
                        code.bytes.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax
                    }
                }
                code.bytes.extend_from_slice(&[0xC9, 0xC3]); // leave; ret
            }
            MirTerm::Trap(trap) => {
                let status = trap_status(trap).ok_or_else(|| {
                    JitError::Unsupported(format!("a {trap:?} trap in machine code"))
                })?;
                emit_trap(&mut code, status, context_slot);
            }
        }
    }

    for (position, block) in code.fixups {
        let target = code.labels[block].expect("every block is emitted");
        let displacement = target as i64 - (position as i64 + 4);
        code.bytes[position..position + 4].copy_from_slice(&(displacement as i32).to_le_bytes());
    }
    Ok(code.bytes)
}

/// Emit a short forward jump with `opcode` whose target is filled in by [`land`]. Returns the
/// position of its displacement byte.
fn forward(code: &mut Code, opcode: u8) -> usize {
    code.bytes.extend_from_slice(&[opcode, 0]);
    code.bytes.len() - 1
}

/// Point the short jump whose displacement is at `at` to the current end of the code.
fn land(code: &mut Code, at: usize) {
    let distance = code.bytes.len() - (at + 1);
    code.bytes[at] = u8::try_from(distance).expect("a trap sequence fits a short jump");
}

/// Write `status` into the context and return zero, as a trap does. Every instruction here is
/// position-independent, so the sequence can sit anywhere in a block.
fn emit_trap(code: &mut Code, status: u32, context_slot: Slot) {
    // mov rcx, [rbp + context]; mov qword [rcx + STATUS], status; xor eax, eax; leave; ret
    code.with_frame_operand(&[0x48, 0x8B], ECX, Code::slot(context_slot));
    code.bytes
        .extend_from_slice(&[0x48, 0xC7, 0x41, STATUS_OFFSET]);
    code.bytes.extend_from_slice(&(status as i32).to_le_bytes());
    code.bytes.extend_from_slice(&[0x31, 0xC0]);
    code.bytes.extend_from_slice(&[0xC9, 0xC3]);
}

/// A division or remainder of `eax` by `ecx` into `eax`, after the checks WebAssembly requires, at
/// 32 bits.
///
/// A zero divisor traps. Signed division of the minimum value by `-1` traps, because the hardware
/// would fault. The remainder by `-1` is zero, which is taken directly, since the hardware would
/// fault on the minimum value there too.
fn emit_division(code: &mut Code, op: DivOp, context_slot: Slot) {
    // test ecx, ecx; jnz nonzero; trap(DivideByZero); nonzero:
    code.bytes.extend_from_slice(&[0x85, 0xC9]);
    let nonzero = forward(code, 0x75);
    emit_trap(code, STATUS_DIVIDE_BY_ZERO, context_slot);
    land(code, nonzero);

    match op {
        DivOp::DivU | DivOp::RemU => {
            code.bytes.extend_from_slice(&[0x31, 0xD2]); // xor edx, edx
            code.bytes.extend_from_slice(&[0xF7, 0xF1]); // div ecx
            if op == DivOp::RemU {
                code.bytes.extend_from_slice(&[0x89, 0xD0]); // mov eax, edx
            }
        }
        DivOp::DivS => {
            // cmp ecx, -1; jne normal; cmp eax, MIN; jne normal; trap(Overflow); normal:
            code.bytes.extend_from_slice(&[0x83, 0xF9, 0xFF]);
            let normal = forward(code, 0x75);
            code.bytes.push(0x3D); // cmp eax, imm32
            code.bytes.extend_from_slice(&i32::MIN.to_le_bytes());
            let not_minimum = forward(code, 0x75);
            emit_trap(code, STATUS_OVERFLOW, context_slot);
            land(code, not_minimum);
            land(code, normal);
            code.bytes.extend_from_slice(&[0x99, 0xF7, 0xF9]); // cdq; idiv ecx
        }
        DivOp::RemS => {
            // cmp ecx, -1; jne normal; xor eax, eax; jmp done; normal: cdq; idiv ecx;
            // mov eax, edx; done:
            code.bytes.extend_from_slice(&[0x83, 0xF9, 0xFF]);
            let normal = forward(code, 0x75);
            code.bytes.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax
            let done = forward(code, 0xEB);
            land(code, normal);
            code.bytes.extend_from_slice(&[0x99, 0xF7, 0xF9]); // cdq; idiv ecx
            code.bytes.extend_from_slice(&[0x89, 0xD0]); // mov eax, edx
            land(code, done);
        }
    }
}

/// A division or remainder of `rax` by `rcx` into `rax`, at 64 bits: the same checks as
/// [`emit_division`], with the 64-bit forms, and the minimum value of `i64` in place of `i32`.
fn emit_division64(code: &mut Code, op: DivOp, context_slot: Slot) {
    // test rcx, rcx; jnz nonzero; trap(DivideByZero); nonzero:
    code.bytes.extend_from_slice(&[0x48, 0x85, 0xC9]);
    let nonzero = forward(code, 0x75);
    emit_trap(code, STATUS_DIVIDE_BY_ZERO, context_slot);
    land(code, nonzero);

    match op {
        DivOp::DivU | DivOp::RemU => {
            code.bytes.extend_from_slice(&[0x31, 0xD2]); // xor edx, edx
            code.bytes.extend_from_slice(&[0x48, 0xF7, 0xF1]); // div rcx
            if op == DivOp::RemU {
                code.bytes.extend_from_slice(&[0x48, 0x89, 0xD0]); // mov rax, rdx
            }
        }
        DivOp::DivS => {
            // cmp rcx, -1; jne normal; movabs rdx, MIN; cmp rax, rdx; jne normal;
            // trap(Overflow); normal: cqo; idiv rcx
            code.bytes.extend_from_slice(&[0x48, 0x83, 0xF9, 0xFF]);
            let normal = forward(code, 0x75);
            code.bytes.extend_from_slice(&[0x48, 0xBA]); // movabs rdx, imm64
            code.bytes.extend_from_slice(&i64::MIN.to_le_bytes());
            code.bytes.extend_from_slice(&[0x48, 0x39, 0xD0]); // cmp rax, rdx
            let not_minimum = forward(code, 0x75);
            emit_trap(code, STATUS_OVERFLOW, context_slot);
            land(code, not_minimum);
            land(code, normal);
            code.bytes
                .extend_from_slice(&[0x48, 0x99, 0x48, 0xF7, 0xF9]); // cqo; idiv rcx
        }
        DivOp::RemS => {
            // cmp rcx, -1; jne normal; xor eax, eax; jmp done; normal: cqo; idiv rcx;
            // mov rax, rdx; done:
            code.bytes.extend_from_slice(&[0x48, 0x83, 0xF9, 0xFF]);
            let normal = forward(code, 0x75);
            code.bytes.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax
            let done = forward(code, 0xEB);
            land(code, normal);
            code.bytes
                .extend_from_slice(&[0x48, 0x99, 0x48, 0xF7, 0xF9]); // cqo; idiv rcx
            code.bytes.extend_from_slice(&[0x48, 0x89, 0xD0]); // mov rax, rdx
            land(code, done);
        }
    }
}

/// `eax = clz(ecx)`, `ctz(ecx)` or `popcnt(ecx)`, clobbering `edx`, at 32 bits.
///
/// `bsr` and `bsf` leave their destination undefined for a zero input, so `cmovz` gives the zero
/// input the width's count: `clz` takes `31 - bsr`, with `-1` standing for a zero input, and `ctz`
/// takes `bsf` with `32` standing in for zero.
fn emit_unary(code: &mut Code, op: IntUnary) {
    match op {
        IntUnary::Clz => {
            code.bytes
                .extend_from_slice(&[0xBA, 0xFF, 0xFF, 0xFF, 0xFF]); // mov edx, -1
            code.bytes.extend_from_slice(&[0x0F, 0xBD, 0xC1]); // bsr eax, ecx
            code.bytes.extend_from_slice(&[0x0F, 0x44, 0xC2]); // cmovz eax, edx
            code.bytes.push(0xB9); // mov ecx, 31
            code.bytes.extend_from_slice(&31_i32.to_le_bytes());
            code.bytes.extend_from_slice(&[0x29, 0xC1]); // sub ecx, eax
            code.bytes.extend_from_slice(&[0x89, 0xC8]); // mov eax, ecx
        }
        IntUnary::Ctz => {
            code.bytes
                .extend_from_slice(&[0xBA, 0x20, 0x00, 0x00, 0x00]); // mov edx, 32
            code.bytes.extend_from_slice(&[0x0F, 0xBC, 0xC1]); // bsf eax, ecx
            code.bytes.extend_from_slice(&[0x0F, 0x44, 0xC2]); // cmovz eax, edx
        }
        IntUnary::Popcnt => {
            // The divide-and-add sum over 2-, 4- and 8-bit fields, then the byte sums collected
            // into the top byte by a multiplication, all on 32 bits.
            code.bytes.extend_from_slice(&[0x89, 0xC8]); // mov eax, ecx
            code.bytes.extend_from_slice(&[0x89, 0xC2]); // mov edx, eax
            code.bytes.extend_from_slice(&[0xD1, 0xEA]); // shr edx, 1
            code.bytes.extend_from_slice(&[0x81, 0xE2]); // and edx, 0x5555_5555
            code.bytes.extend_from_slice(&0x5555_5555_i32.to_le_bytes());
            code.bytes.extend_from_slice(&[0x29, 0xD0]); // sub eax, edx
            code.bytes.extend_from_slice(&[0x89, 0xC2]); // mov edx, eax
            code.bytes.extend_from_slice(&[0x81, 0xE2]); // and edx, 0x3333_3333
            code.bytes.extend_from_slice(&0x3333_3333_i32.to_le_bytes());
            code.bytes.extend_from_slice(&[0xC1, 0xE8, 0x02]); // shr eax, 2
            code.bytes.push(0x25); // and eax, 0x3333_3333
            code.bytes.extend_from_slice(&0x3333_3333_i32.to_le_bytes());
            code.bytes.extend_from_slice(&[0x01, 0xD0]); // add eax, edx
            code.bytes.extend_from_slice(&[0x89, 0xC2]); // mov edx, eax
            code.bytes.extend_from_slice(&[0xC1, 0xEA, 0x04]); // shr edx, 4
            code.bytes.extend_from_slice(&[0x01, 0xD0]); // add eax, edx
            code.bytes.push(0x25); // and eax, 0x0F0F_0F0F
            code.bytes.extend_from_slice(&0x0F0F_0F0F_i32.to_le_bytes());
            code.bytes.extend_from_slice(&[0x69, 0xC0]); // imul eax, eax, 0x0101_0101
            code.bytes.extend_from_slice(&0x0101_0101_i32.to_le_bytes());
            code.bytes.extend_from_slice(&[0xC1, 0xE8, 0x18]); // shr eax, 24
        }
    }
}

/// The canonical NaN of an `f32` result, as Micro gives it.
const CANONICAL_NAN_F32: u32 = 0x7fc0_0000;

/// Load a float from `slot` into the low half of `xmm`: `movq` (`F3 0F 7E`) for an `f64`, and `movd`
/// (`66 0F 6E`) for an `f32`, which reads only the low 32 bits of the slot.
fn load_xmm(code: &mut Code, xmm: u8, slot: Slot, precision: Precision) {
    match precision {
        Precision::Double => code.with_frame_operand(&[0xF3, 0x0F, 0x7E], xmm, Code::slot(slot)),
        Precision::Single => code.with_frame_operand(&[0x66, 0x0F, 0x6E], xmm, Code::slot(slot)),
    }
}

/// Store the low half of `xmm` as a float result. An `f64` is written as `movq [slot], xmm`. An `f32`
/// goes through `eax`, so the slot's word is the zero-extended value, as an `i32` reader expects.
fn store_xmm(code: &mut Code, xmm: u8, slot: Slot, precision: Precision) {
    match precision {
        Precision::Double => code.with_frame_operand(&[0x66, 0x0F, 0xD6], xmm, Code::slot(slot)),
        Precision::Single => {
            code.bytes
                .extend_from_slice(&[0x66, 0x0F, 0x7E, 0xC0 | (xmm << 3)]); // movd eax, xmm
            code.store(EAX, slot);
        }
    }
}

/// `ucomisd` (`66 0F 2E`) or `ucomiss` (`0F 2E`) `xmm_x, xmm_y`.
fn ucomi(code: &mut Code, x: u8, y: u8, precision: Precision) {
    let modrm = 0xC0 | (x << 3) | y;
    match precision {
        Precision::Double => code.bytes.extend_from_slice(&[0x66, 0x0F, 0x2E, modrm]),
        Precision::Single => code.bytes.extend_from_slice(&[0x0F, 0x2E, modrm]),
    }
}

/// Put the canonical NaN of `precision` in `xmm0`.
fn load_canonical_xmm0(code: &mut Code, precision: Precision) {
    match precision {
        Precision::Double => {
            code.bytes.extend_from_slice(&[0x48, 0xB8]); // movabs rax, canonical
            code.bytes
                .extend_from_slice(&CANONICAL_NAN_F64.to_le_bytes());
            code.bytes
                .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0]); // movq xmm0, rax
        }
        Precision::Single => {
            code.bytes.push(0xB8); // mov eax, canonical
            code.bytes
                .extend_from_slice(&CANONICAL_NAN_F32.to_le_bytes());
            code.bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, 0xC0]); // movd xmm0, eax
        }
    }
}

/// Replace a NaN in `xmm0` with the canonical NaN that Micro gives. The hardware's own NaN differs,
/// and the replacement is skipped when the value is not a NaN: an unordered compare of `xmm0` with
/// itself sets the parity flag exactly for a NaN.
fn canonicalize_xmm0(code: &mut Code, precision: Precision) {
    ucomi(code, 0, 0, precision);
    let ordered = forward(code, 0x7B); // jnp ordered
    load_canonical_xmm0(code, precision);
    land(code, ordered);
}

/// A jump or call with a 32-bit displacement, `opcode` then four bytes filled in by [`land32`].
/// Returns the position of the displacement.
fn forward32(code: &mut Code, opcode: &[u8]) -> usize {
    code.bytes.extend_from_slice(opcode);
    let at = code.bytes.len();
    code.bytes.extend_from_slice(&[0; 4]);
    at
}

/// Point the 32-bit displacement at `at` to the current end of the code.
fn land32(code: &mut Code, at: usize) {
    let displacement = code.bytes.len() as i64 - (at as i64 + 4);
    code.bytes[at..at + 4].copy_from_slice(&(displacement as i32).to_le_bytes());
}

/// `min` (`minimum`) or `max`, with Micro's rules. A NaN in either operand gives the canonical NaN. Two
/// equal operands are the same value, except for two zeros of different sign, where min takes the
/// negative one and max the positive one: the OR and the AND of their bits. Otherwise the smaller (min)
/// or larger (max) operand is kept, and the other one is taken.
fn emit_min_max(code: &mut Code, minimum: bool, dst: Slot, a: Slot, b: Slot, precision: Precision) {
    load_xmm(code, 0, a, precision);
    load_xmm(code, 1, b, precision);
    ucomi(code, 0, 0, precision);
    let nan_left = forward32(code, &[0x0F, 0x8A]); // jp canonical
    ucomi(code, 1, 1, precision);
    let nan_right = forward32(code, &[0x0F, 0x8A]); // jp canonical
    ucomi(code, 0, 1, precision);
    let unequal = forward32(code, &[0x0F, 0x85]); // jne unequal
                                                  // The bits of the two equal operands, in rax and rcx: moved as words, or as 32-bit values.
    match precision {
        Precision::Double => {
            code.bytes
                .extend_from_slice(&[0x66, 0x48, 0x0F, 0x7E, 0xC0]); // movq rax, xmm0
            code.bytes
                .extend_from_slice(&[0x66, 0x48, 0x0F, 0x7E, 0xC9]); // movq rcx, xmm1
        }
        Precision::Single => {
            code.bytes.extend_from_slice(&[0x66, 0x0F, 0x7E, 0xC0]); // movd eax, xmm0
            code.bytes.extend_from_slice(&[0x66, 0x0F, 0x7E, 0xC9]); // movd ecx, xmm1
        }
    }
    let combine: &[u8] = match (minimum, precision) {
        (true, Precision::Double) => &[0x48, 0x09, 0xC8], // or rax, rcx
        (false, Precision::Double) => &[0x48, 0x21, 0xC8], // and rax, rcx
        (true, Precision::Single) => &[0x09, 0xC8],       // or eax, ecx
        (false, Precision::Single) => &[0x21, 0xC8],      // and eax, ecx
    };
    code.bytes.extend_from_slice(combine);
    match precision {
        Precision::Double => code
            .bytes
            .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0]), // movq xmm0, rax
        Precision::Single => code.bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, 0xC0]), // movd xmm0, eax
    }
    let done_equal = forward32(code, &[0xE9]); // jmp done
    land32(code, unequal);
    // min keeps the left operand when it is below the right one, and max when it is above it.
    if minimum {
        ucomi(code, 1, 0, precision);
    } else {
        ucomi(code, 0, 1, precision);
    }
    let keep = forward(code, 0x77); // ja keep
    code.bytes.extend_from_slice(&[0x0F, 0x28, 0xC1]); // movaps xmm0, xmm1
    land(code, keep);
    let done_unequal = forward32(code, &[0xE9]); // jmp done
    land32(code, nan_left);
    land32(code, nan_right);
    load_canonical_xmm0(code, precision);
    land32(code, done_equal);
    land32(code, done_unequal);
    store_xmm(code, 0, dst, precision);
}

/// `copysign`: the first operand's magnitude and payload with the second operand's sign. The bits are
/// combined directly, so a NaN keeps its payload, as Micro's copysign does.
fn emit_copysign(code: &mut Code, dst: Slot, a: Slot, b: Slot, precision: Precision) {
    match precision {
        Precision::Double => {
            code.load(EAX, a);
            code.load(ECX, b);
            movabs_rdx(code, 0x8000_0000_0000_0000); // the sign bit
            code.bytes.extend_from_slice(&[0x48, 0x21, 0xD1]); // and rcx, rdx
            movabs_rdx(code, 0x7FFF_FFFF_FFFF_FFFF); // everything but the sign
            code.bytes.extend_from_slice(&[0x48, 0x21, 0xD0]); // and rax, rdx
            code.bytes.extend_from_slice(&[0x48, 0x09, 0xC8]); // or rax, rcx
        }
        Precision::Single => {
            code.load32(EAX, a);
            code.load32(ECX, b);
            code.bytes.extend_from_slice(&[0x81, 0xE1]); // and ecx, 0x8000_0000
            code.bytes.extend_from_slice(&0x8000_0000_u32.to_le_bytes());
            code.bytes.push(0x25); // and eax, 0x7FFF_FFFF
            code.bytes.extend_from_slice(&0x7FFF_FFFF_u32.to_le_bytes());
            code.bytes.extend_from_slice(&[0x09, 0xC8]); // or eax, ecx
        }
    }
    code.store(EAX, dst);
}

/// Rounding with `roundsd` or `roundss`: `mode` is 0 to the nearest even integer, 1 down, 2 up, 3 toward
/// zero. A NaN result is the canonical one, as Micro gives it.
fn emit_round(code: &mut Code, mode: u8, dst: Slot, a: Slot, precision: Precision) {
    load_xmm(code, 0, a, precision);
    let opcode = match precision {
        Precision::Double => 0x0B, // roundsd
        Precision::Single => 0x0A, // roundss
    };
    code.bytes
        .extend_from_slice(&[0x66, 0x0F, 0x3A, opcode, 0xC0, mode]); // round xmm0, xmm0, mode
    canonicalize_xmm0(code, precision);
    store_xmm(code, 0, dst, precision);
}

/// Load a constant float of `precision`, given as its bits, into `xmm`: through `rax` for an `f64`, and
/// `eax` for an `f32`. Clobbers `rax`.
fn load_const_xmm(code: &mut Code, xmm: u8, bits: u64, precision: Precision) {
    match precision {
        Precision::Double => {
            code.bytes.extend_from_slice(&[0x48, 0xB8]); // movabs rax, bits
            code.bytes.extend_from_slice(&bits.to_le_bytes());
            code.bytes
                .extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0 | (xmm << 3)]);
            // movq xmm, rax
        }
        Precision::Single => {
            code.bytes.push(0xB8); // mov eax, bits
            code.bytes.extend_from_slice(&(bits as u32).to_le_bytes());
            code.bytes
                .extend_from_slice(&[0x66, 0x0F, 0x6E, 0xC0 | (xmm << 3)]); // movd xmm, eax
        }
    }
}

/// The bits of `value` as a float of `precision`. Both bounds of the truncations are exact in either.
fn float_bits_of(value: f64, precision: Precision) -> u64 {
    match precision {
        Precision::Double => value.to_bits(),
        Precision::Single => u64::from((value as f32).to_bits()),
    }
}

/// `dst = (float) src`: an integer of `from` converted to a float of precision `to`, rounded once.
/// A 32-bit value is read zero-extended for the unsigned kind; an unsigned 64-bit value that has its top
/// bit set is halved, keeping the bit it would lose as a sticky low bit, converted, and doubled, which
/// rounds as a direct conversion does.
fn emit_int_to_float(code: &mut Code, to: Precision, from: IntKind, dst: Slot, src: Slot) {
    let prefix = match to {
        Precision::Double => 0xF2,
        Precision::Single => 0xF3,
    };
    match from {
        IntKind::Signed32 => {
            code.load32(EAX, src);
            code.bytes.extend_from_slice(&[prefix, 0x0F, 0x2A, 0xC0]); // cvtsi2 xmm0, eax
        }
        IntKind::Unsigned32 => {
            code.load32(EAX, src); // zero-extended, so a 64-bit conversion reads it unsigned
            code.bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2 xmm0, rax
        }
        IntKind::Signed64 => {
            code.load(EAX, src);
            code.bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2 xmm0, rax
        }
        IntKind::Unsigned64 => {
            code.load(EAX, src);
            code.bytes.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
            let small = forward(code, 0x79); // jns small
            code.bytes.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
            code.bytes.extend_from_slice(&[0x48, 0xD1, 0xE9]); // shr rcx, 1
            code.bytes.extend_from_slice(&[0x83, 0xE0, 0x01]); // and eax, 1: the bit lost
            code.bytes.extend_from_slice(&[0x48, 0x09, 0xC1]); // or rcx, rax: the sticky bit
            code.bytes.extend_from_slice(&[0x48, 0x89, 0xC8]); // mov rax, rcx
            code.bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2 xmm0, rax
            code.bytes.extend_from_slice(&[prefix, 0x0F, 0x58, 0xC0]); // add xmm0, xmm0
            let done = forward(code, 0xEB); // jmp done
            land(code, small);
            code.bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2 xmm0, rax
            land(code, done);
        }
    }
    store_xmm(code, 0, dst, to);
}

/// `dst = (float) src` between the precisions: `cvtsd2ss` or `cvtss2sd`, then the canonical NaN.
fn emit_float_to_float(code: &mut Code, to: Precision, dst: Slot, src: Slot) {
    match to {
        Precision::Single => {
            load_xmm(code, 0, src, Precision::Double);
            code.bytes.extend_from_slice(&[0xF2, 0x0F, 0x5A, 0xC0]); // cvtsd2ss xmm0, xmm0
        }
        Precision::Double => {
            load_xmm(code, 0, src, Precision::Single);
            code.bytes.extend_from_slice(&[0xF3, 0x0F, 0x5A, 0xC0]); // cvtss2sd xmm0, xmm0
        }
    }
    canonicalize_xmm0(code, to);
    store_xmm(code, 0, dst, to);
}

/// `dst = trunc(src)` as an integer of kind `to`, trapping as Micro does. The value is truncated toward
/// zero with `roundsd`/`roundss`, so a NaN stays a NaN, and an infinity is out of range. A NaN traps, and
/// so does a truncated value below the lower bound or at or above the upper one; both bounds are powers
/// of two, so they are exact in either precision. An unsigned 64-bit value at or above `2^63` has that
/// subtracted before the conversion, and the top bit set afterwards.
fn emit_float_to_int(
    code: &mut Code,
    from: Precision,
    to: IntKind,
    dst: Slot,
    src: Slot,
    context_slot: Slot,
) {
    let prefix = match from {
        Precision::Double => 0xF2,
        Precision::Single => 0xF3,
    };
    load_xmm(code, 0, src, from);
    let round = match from {
        Precision::Double => 0x0B, // roundsd
        Precision::Single => 0x0A, // roundss
    };
    code.bytes
        .extend_from_slice(&[0x66, 0x0F, 0x3A, round, 0xC0, 0x03]); // round xmm0, xmm0, toward zero
                                                                    // A NaN traps: an unordered compare of xmm0 with itself sets the parity flag.
    ucomi(code, 0, 0, from);
    let ordered = forward(code, 0x7B); // jnp ordered
    emit_trap(code, STATUS_INVALID_CONVERSION, context_slot);
    land(code, ordered);

    let (low, high) = match to {
        IntKind::Signed32 => (-2_147_483_648.0, 2_147_483_648.0),
        IntKind::Unsigned32 => (0.0, 4_294_967_296.0),
        IntKind::Signed64 => (-9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0),
        IntKind::Unsigned64 => (0.0, 18_446_744_073_709_551_616.0),
    };
    // Below the lower bound traps: `jae` skips the trap when the value is not below it.
    load_const_xmm(code, 1, float_bits_of(low, from), from);
    ucomi(code, 0, 1, from);
    let at_least_low = forward(code, 0x73); // jae
    emit_trap(code, STATUS_INVALID_CONVERSION, context_slot);
    land(code, at_least_low);
    // At or above the upper bound traps: `jb` skips the trap when the value is below it.
    load_const_xmm(code, 2, float_bits_of(high, from), from);
    ucomi(code, 0, 2, from);
    let below_high = forward(code, 0x72); // jb
    emit_trap(code, STATUS_INVALID_CONVERSION, context_slot);
    land(code, below_high);

    match to {
        IntKind::Signed32 => code.bytes.extend_from_slice(&[prefix, 0x0F, 0x2C, 0xC0]), // cvtt eax, xmm0
        IntKind::Signed64 | IntKind::Unsigned32 => {
            code.bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2C, 0xC0]) // cvtt rax, xmm0
        }
        IntKind::Unsigned64 => {
            // Values from 2^63 up are shifted down by 2^63 first, and the bit put back afterwards.
            load_const_xmm(
                code,
                3,
                float_bits_of(9_223_372_036_854_775_808.0, from),
                from,
            );
            ucomi(code, 0, 3, from);
            let small = forward(code, 0x72); // jb small
            code.bytes.extend_from_slice(&[prefix, 0x0F, 0x5C, 0xC3]); // sub xmm0, xmm3
            code.bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2C, 0xC0]); // cvtt rax, xmm0
            code.bytes.extend_from_slice(&[0x48, 0xB9]); // movabs rcx, 2^63
            code.bytes.extend_from_slice(&(1_u64 << 63).to_le_bytes());
            code.bytes.extend_from_slice(&[0x48, 0x31, 0xC8]); // xor rax, rcx
            let done = forward(code, 0xEB); // jmp done
            land(code, small);
            code.bytes
                .extend_from_slice(&[prefix, 0x48, 0x0F, 0x2C, 0xC0]); // cvtt rax, xmm0
            land(code, done);
        }
    }
    code.store(EAX, dst);
}

/// `movabs rdx, value`: `REX.W BA imm64`.
fn movabs_rdx(code: &mut Code, value: u64) {
    code.bytes.extend_from_slice(&[0x48, 0xBA]);
    code.bytes.extend_from_slice(&value.to_le_bytes());
}

/// `rax = clz(rcx)`, `ctz(rcx)` or `popcnt(rcx)`, clobbering `rdx`, at 64 bits: the forms of
/// [`emit_unary`] with 64-bit registers and the width's count, 64, in place of 32.
fn emit_unary64(code: &mut Code, op: IntUnary) {
    match op {
        IntUnary::Clz => {
            code.bytes
                .extend_from_slice(&[0x48, 0xC7, 0xC2, 0xFF, 0xFF, 0xFF, 0xFF]); // mov rdx, -1
            code.bytes.extend_from_slice(&[0x48, 0x0F, 0xBD, 0xC1]); // bsr rax, rcx
            code.bytes.extend_from_slice(&[0x48, 0x0F, 0x44, 0xC2]); // cmovz rax, rdx
            code.bytes
                .extend_from_slice(&[0x48, 0xC7, 0xC1, 0x3F, 0x00, 0x00, 0x00]); // mov rcx, 63
            code.bytes.extend_from_slice(&[0x48, 0x29, 0xC1]); // sub rcx, rax
            code.bytes.extend_from_slice(&[0x48, 0x89, 0xC8]); // mov rax, rcx
        }
        IntUnary::Ctz => {
            code.bytes
                .extend_from_slice(&[0x48, 0xC7, 0xC2, 0x40, 0x00, 0x00, 0x00]); // mov rdx, 64
            code.bytes.extend_from_slice(&[0x48, 0x0F, 0xBC, 0xC1]); // bsf rax, rcx
            code.bytes.extend_from_slice(&[0x48, 0x0F, 0x44, 0xC2]); // cmovz rax, rdx
        }
        IntUnary::Popcnt => {
            // The divide-and-add sum over 2-, 4- and 8-bit fields, then the byte sums collected
            // into the top byte by a multiplication, all on 64 bits.
            code.bytes.extend_from_slice(&[0x48, 0x89, 0xC8]); // mov rax, rcx
                                                               // x -= (x >> 1) & 0x5555...
            code.bytes
                .extend_from_slice(&[0x48, 0x89, 0xC1, 0x48, 0xD1, 0xE9]); // mov rcx, rax; shr rcx, 1
            movabs_rdx(code, 0x5555_5555_5555_5555);
            code.bytes
                .extend_from_slice(&[0x48, 0x21, 0xD1, 0x48, 0x29, 0xC8]); // and rcx, rdx; sub rax, rcx
                                                                           // x = (x & 0x3333...) + ((x >> 2) & 0x3333...)
            movabs_rdx(code, 0x3333_3333_3333_3333);
            code.bytes.extend_from_slice(&[
                0x48, 0x89, 0xC1, // mov rcx, rax
                0x48, 0x21, 0xD1, // and rcx, rdx
                0x48, 0xC1, 0xE8, 0x02, // shr rax, 2
                0x48, 0x21, 0xD0, // and rax, rdx
                0x48, 0x01, 0xC8, // add rax, rcx
            ]);
            // x = (x + (x >> 4)) & 0x0F0F...
            code.bytes
                .extend_from_slice(&[0x48, 0x89, 0xC1, 0x48, 0xC1, 0xE9, 0x04, 0x48, 0x01, 0xC8]);
            movabs_rdx(code, 0x0F0F_0F0F_0F0F_0F0F);
            code.bytes.extend_from_slice(&[0x48, 0x21, 0xD0]); // and rax, rdx
                                                               // x * 0x0101... >> 56: the byte sums collect in the top byte.
            movabs_rdx(code, 0x0101_0101_0101_0101);
            code.bytes
                .extend_from_slice(&[0x48, 0x0F, 0xAF, 0xC2, 0x48, 0xC1, 0xE8, 0x38]);
            // imul rax, rdx; shr rax, 56
        }
    }
}

/// Leave the address of an access in `rax` and the memory base in `rdx`, after trapping if the
/// access does not lie wholly inside the memory.
///
/// The address is the 32-bit operand plus the offset, computed in 64 bits so it cannot wrap. The
/// end of the access must be at most the memory length, which the context carries.
fn emit_address(code: &mut Code, address: Slot, offset: u32, width: u32, context_slot: Slot) {
    code.load32(EAX, address); // mov eax, [address]: the unsigned address, zero-extended
    code.bytes.push(0xB9); // mov ecx, offset: zero-extends into rcx
    code.bytes.extend_from_slice(&offset.to_le_bytes());
    code.bytes.extend_from_slice(&[0x48, 0x01, 0xC8]); // add rax, rcx
    code.bytes.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
    code.bytes
        .extend_from_slice(&[0x48, 0x83, 0xC1, width as u8]); // add rcx, width
    code.with_frame_operand(&[0x48, 0x8B], EDX, Code::slot(context_slot)); // mov rdx, [context]
    code.bytes
        .extend_from_slice(&[0x48, 0x3B, 0x4A, MEMORY_LENGTH_OFFSET]); // cmp rcx, [rdx + len]
    let inside = forward(code, 0x76); // jbe inside
    emit_trap(code, STATUS_MEMORY_OUT_OF_BOUNDS, context_slot);
    land(code, inside);
    code.bytes
        .extend_from_slice(&[0x48, 0x8B, 0x52, MEMORY_OFFSET]); // mov rdx, [rdx + base]
}

/// `dst = function(args...)`, through the context's function table.
///
/// The arguments are written to their entries, then the call depth is checked and counted, the
/// callee's entry is called with the argument pointer and the context, and afterwards the depth is
/// counted off. If the callee left a status, this function returns zero without storing a result,
/// as the other encoders do.
fn emit_call(
    code: &mut Code,
    callee: u32,
    args: &[Slot],
    dst: &[Slot],
    context_slot: Slot,
    calls: &Calls,
) {
    emit_arguments(code, args, calls);
    emit_enter(code, context_slot);
    emit_registers(code, context_slot, calls);
    // mov rax, [ctx + functions]; call [rax + 8 * callee]
    code.bytes.extend_from_slice(&[
        0x48,
        0x8B,
        0x40 | (calls.abi.context_register() & 7),
        FUNCTIONS_OFFSET,
    ]);
    code.bytes.extend_from_slice(&[0xFF, 0x90]); // call [rax + disp32]
    code.bytes
        .extend_from_slice(&(8 * callee as i32).to_le_bytes());
    emit_leave(code, context_slot, dst, calls);
}

/// `dst = table[index](args...)`: the index is checked against the table's length, the slot against
/// null, and its function's signature against the one the call names, in that order and with the
/// traps Micro gives. Then the slot's entry is called, through a frame slot, because the registers
/// the call needs are set after the depth check.
fn emit_call_indirect(
    code: &mut Code,
    type_index: u32,
    index: Slot,
    args: &[Slot],
    dst: &[Slot],
    context_slot: Slot,
    calls: &Calls,
) {
    emit_arguments(code, args, calls);
    // rcx = the index, zero-extended from its slot; rax = the context.
    code.load32(ECX, index);
    code.with_frame_operand(&[0x48, 0x8B], EAX, Code::slot(context_slot));
    // cmp rcx, [rax + table_len]; jb ok; trap: the index is past the table.
    code.bytes
        .extend_from_slice(&[0x48, 0x3B, 0x48, TABLE_LEN_OFFSET]);
    let ok = forward(code, 0x72);
    emit_trap(code, STATUS_TABLE_OUT_OF_BOUNDS, context_slot);
    land(code, ok);
    // rdx = the slot's signature id, -1 for a null slot: mov rdx, [rax + types]; mov rdx, [rdx + rcx * 8]
    code.bytes
        .extend_from_slice(&[0x48, 0x8B, 0x50, TABLE_TYPES_OFFSET, 0x48, 0x8B, 0x14, 0xCA]);
    // cmp rdx, -1; jne ok; trap: the slot is null.
    code.bytes.extend_from_slice(&[0x48, 0x83, 0xFA, 0xFF]);
    let ok = forward(code, 0x75);
    emit_trap(code, STATUS_NULL_REFERENCE, context_slot);
    land(code, ok);
    // cmp rdx, expected; je ok; trap: the slot's function has another signature.
    code.bytes.extend_from_slice(&[0x48, 0x81, 0xFA]);
    code.bytes
        .extend_from_slice(&calls.type_ids[type_index as usize].to_le_bytes());
    let ok = forward(code, 0x74);
    emit_trap(code, STATUS_INDIRECT_TYPE_MISMATCH, context_slot);
    land(code, ok);
    // rdx = the slot's entry point: mov rdx, [rax + entries]; mov rdx, [rdx + rcx * 8];
    // mov [entry], rdx.
    code.bytes.extend_from_slice(&[
        0x48,
        0x8B,
        0x50,
        TABLE_ENTRIES_OFFSET,
        0x48,
        0x8B,
        0x14,
        0xCA,
    ]);
    code.with_frame_operand(&[0x48, 0x89], EDX, Code::slot(calls.entry));
    emit_enter(code, context_slot);
    emit_registers(code, context_slot, calls);
    code.with_frame_operand(&[0xFF], 2, Code::slot(calls.entry)); // call [rbp + entry]
    emit_leave(code, context_slot, dst, calls);
}

/// Write the arguments of a call to their entries, as whole words.
fn emit_arguments(code: &mut Code, args: &[Slot], calls: &Calls) {
    for (index, arg) in args.iter().enumerate() {
        code.load(EAX, *arg);
        code.store(EAX, calls.block + calls.len - 1 - index as u32);
    }
}

/// Count one more active frame against the depth limit, and trap if that is past it. Leaves the
/// context in `rdx`.
fn emit_enter(code: &mut Code, context_slot: Slot) {
    // mov rdx, [ctx]; mov rax, [rdx + depth]; cmp rax, [rdx + limit]; jb ok; trap; ok:
    // add qword [rdx + depth], 1
    code.with_frame_operand(&[0x48, 0x8B], EDX, Code::slot(context_slot));
    code.bytes
        .extend_from_slice(&[0x48, 0x8B, 0x42, DEPTH_OFFSET]); // mov rax, [rdx + depth]
    code.bytes
        .extend_from_slice(&[0x48, 0x3B, 0x42, DEPTH_LIMIT_OFFSET]); // cmp rax, [rdx + limit]
    let ok = forward(code, 0x72); // jb ok
    emit_trap(code, STATUS_CALL_DEPTH, context_slot);
    land(code, ok);
    code.bytes
        .extend_from_slice(&[0x48, 0x83, 0x42, DEPTH_OFFSET, 0x01]); // add qword [rdx + depth], 1
}

/// The argument pointer, the lowest entry, goes in the first argument register, and the context in
/// the second. `lea reg, [rbp + disp]`; `mov reg, [rbp + ctx]`.
fn emit_registers(code: &mut Code, context_slot: Slot, calls: &Calls) {
    let top = calls.block + calls.len.max(1) - 1;
    code.with_frame_operand(&[0x48, 0x8D], calls.abi.args_register(), Code::slot(top));
    code.with_frame_operand(
        &[0x48, 0x8B],
        calls.abi.context_register(),
        Code::slot(context_slot),
    );
}

/// After the callee returns: count the depth off and, if the callee left a status, return zero
/// without a result, as the other encoders do. Otherwise store the callee's result, if there is one.
fn emit_leave(code: &mut Code, context_slot: Slot, dst: &[Slot], calls: &Calls) {
    // mov rdx, [ctx]; sub qword [rdx + depth], 1; cmp qword [rdx + status], 0; je ok;
    // xor eax, eax; leave; ret; ok: the result, if any, is in rax.
    code.with_frame_operand(&[0x48, 0x8B], EDX, Code::slot(context_slot));
    code.bytes
        .extend_from_slice(&[0x48, 0x83, 0x6A, DEPTH_OFFSET, 0x01]); // sub qword [rdx + depth], 1
    code.bytes
        .extend_from_slice(&[0x48, 0x83, 0x7A, STATUS_OFFSET, 0x00]); // cmp qword [rdx + status], 0
    let done = forward(code, 0x74); // je done
    code.bytes.extend_from_slice(&[0x31, 0xC0, 0xC9, 0xC3]); // xor eax, eax; leave; ret
    land(code, done);
    match dst {
        [] => {}
        // A single result is in rax.
        [only] => code.store(EAX, *only),
        // Two or more results were written to the argument block, entry `j` at `8 * j` from its
        // pointer, which is slot `block + len - 1 - j`.
        _ => {
            for (index, slot) in dst.iter().enumerate() {
                let entry = calls.block + calls.len - 1 - index as u32;
                code.load(EAX, entry);
                code.store(EAX, *slot);
            }
        }
    }
}

/// Check that global `index` is inside the globals the caller provided, then leave the array's base
/// in `rax`. Returns the byte offset of the global within it, for `[rax + offset]`. An index past the
/// end traps, so code called without its globals cannot read past them.
fn emit_global_base(code: &mut Code, index: u32, context_slot: Slot) -> i32 {
    let index = i32::try_from(index).expect("checked by encode");
    let offset = index * 8;
    // mov rdx, [ctx]; cmp qword [rdx + len], index; ja ok; trap; ok:
    code.with_frame_operand(&[0x48, 0x8B], EDX, Code::slot(context_slot));
    code.bytes
        .extend_from_slice(&[0x48, 0x81, 0x7A, GLOBALS_LENGTH_OFFSET]);
    code.bytes.extend_from_slice(&index.to_le_bytes());
    let ok = forward(code, 0x77);
    emit_trap(code, STATUS_GLOBAL_OUT_OF_BOUNDS, context_slot);
    land(code, ok);
    code.bytes
        .extend_from_slice(&[0x48, 0x8B, 0x42, GLOBALS_OFFSET]); // mov rax, [rdx + globals]
    offset
}

/// Load a word of `width` from `slot` into `reg`: a 32-bit value zero-extended, or the whole word.
fn load_word(code: &mut Code, reg: u8, slot: Slot, width: WordWidth) {
    match width {
        WordWidth::Low32 => code.load32(reg, slot),
        WordWidth::Full64 => code.load(reg, slot),
    }
}

/// The registers the host trampoline takes its four arguments in: the context, the import index, the
/// argument words, and the result words.
fn host_registers(abi: Abi) -> [u8; 4] {
    match abi {
        Abi::SysV => [7, 6, 2, 1],  // rdi, rsi, rdx, rcx
        Abi::Win64 => [1, 2, 8, 9], // rcx, rdx, r8, r9
    }
}

/// `lea reg, [rbp + disp]`, with the REX bit that a register from r8 up needs.
fn lea_frame(code: &mut Code, reg: u8, disp: i32) {
    code.bytes.push(if reg >= 8 { 0x4C } else { 0x48 });
    code.bytes.push(0x8D);
    code.bytes.push(0x80 | ((reg & 7) << 3) | 0b101);
    code.bytes.extend_from_slice(&disp.to_le_bytes());
}

/// `dsts = import(args...)`: the arguments are written as zero-extended 64-bit words, the trampoline
/// is called with the context, the import index, and the two areas, and a failed call returns zero
/// with its status, as a trapping call does. Then each result is read from its word.
fn emit_host_call(
    code: &mut Code,
    import: u32,
    args: &[(Slot, WordWidth)],
    dsts: &[(Slot, WordWidth)],
    context_slot: Slot,
    calls: &Calls,
) {
    // Each argument is written as a whole word: a 32-bit value zero-extended, as the host convention
    // gives it, and an `i64` or `f64` in full.
    for (index, (arg, width)) in args.iter().enumerate() {
        load_word(code, EAX, *arg, *width);
        code.with_frame_operand(
            &[0x48, 0x89],
            EAX,
            Code::slot(calls.block + calls.len - 1 - index as u32),
        ); // mov [entry], rax
    }
    let [context_register, import_register, arguments_register, results_register] =
        host_registers(calls.abi);
    let top = calls.len.max(1) - 1;
    lea_frame(code, arguments_register, Code::slot(calls.block + top));
    lea_frame(
        code,
        results_register,
        Code::slot(calls.results_block + top),
    );
    code.bytes.push(0xB8 | (import_register & 7)); // mov import, imm32
    code.bytes.extend_from_slice(&import.to_le_bytes());
    // mov rax, [ctx]; mov context, rax; mov rax, [rax + host_call]; call rax
    code.with_frame_operand(&[0x48, 0x8B], EAX, Code::slot(context_slot));
    code.bytes
        .extend_from_slice(&[0x48, 0x89, 0xC0 | (context_register & 7)]);
    code.bytes
        .extend_from_slice(&[0x48, 0x8B, 0x40, HOST_CALL_OFFSET]);
    code.bytes.extend_from_slice(&[0xFF, 0xD0]);
    // mov rdx, [ctx]; cmp qword [rdx + status], 0; je ok; xor eax, eax; leave; ret; ok:
    code.with_frame_operand(&[0x48, 0x8B], EDX, Code::slot(context_slot));
    code.bytes
        .extend_from_slice(&[0x48, 0x83, 0x7A, STATUS_OFFSET, 0x00]);
    let done = forward(code, 0x74);
    code.bytes.extend_from_slice(&[0x31, 0xC0, 0xC9, 0xC3]);
    land(code, done);
    for (index, (dst, width)) in dsts.iter().enumerate() {
        load_word(
            code,
            EAX,
            calls.results_block + calls.len - 1 - index as u32,
            *width,
        );
        code.store(EAX, *dst);
    }
}

fn emit_op(code: &mut Code, op: &MirOp, context_slot: Slot, calls: &Calls) {
    match op {
        MirOp::Call {
            function,
            args,
            dst,
        } => emit_call(code, *function, args, dst, context_slot, calls),
        MirOp::CallIndirect {
            type_index,
            index,
            args,
            dst,
        } => emit_call_indirect(code, *type_index, *index, args, dst, context_slot, calls),
        MirOp::CallHost { import, args, dsts } => {
            emit_host_call(code, *import, args, dsts, context_slot, calls)
        }
        MirOp::GlobalGet { dst, index, width } => {
            let offset = emit_global_base(code, *index, context_slot);
            match width {
                // A 32-bit read zero-extends; a whole entry is a 64-bit read.
                WordWidth::Low32 => code.bytes.extend_from_slice(&[0x8B, 0x80]), // mov eax, [rax + offset]
                WordWidth::Full64 => code.bytes.extend_from_slice(&[0x48, 0x8B, 0x80]), // mov rax, [rax + offset]
            }
            code.bytes.extend_from_slice(&offset.to_le_bytes());
            code.store(EAX, *dst);
        }
        MirOp::GlobalSet { index, src, width } => {
            let offset = emit_global_base(code, *index, context_slot);
            load_word(code, ECX, *src, *width);
            code.bytes.extend_from_slice(&[0x48, 0x89, 0x88]); // mov [rax + offset], rcx
            code.bytes.extend_from_slice(&offset.to_le_bytes());
        }
        MirOp::MemorySize { dst } => {
            code.with_frame_operand(&[0x48, 0x8B], EDX, Code::slot(context_slot)); // mov rdx, [ctx]
            code.bytes
                .extend_from_slice(&[0x48, 0x8B, 0x42, MEMORY_LENGTH_OFFSET]); // mov rax, [rdx + len]
            code.bytes.extend_from_slice(&[0x48, 0xC1, 0xE8, 0x10]); // shr rax, 16: the pages
            code.store(EAX, *dst);
        }
        MirOp::MemoryGrow { dst, delta } => {
            // The new length is the old one plus the pages in bytes. It is kept only if it is
            // within the capacity; otherwise the result is -1 and the length is left alone.
            code.with_frame_operand(&[0x48, 0x8B], EDX, Code::slot(context_slot)); // mov rdx, [ctx]
            code.bytes
                .extend_from_slice(&[0x48, 0x8B, 0x42, MEMORY_LENGTH_OFFSET]); // mov rax, [rdx + len]
            code.load32(ECX, *delta); // ecx = the pages, zero-extended
            code.bytes.extend_from_slice(&[0x48, 0xC1, 0xE1, 0x10]); // shl rcx, 16
            code.bytes.extend_from_slice(&[0x48, 0x01, 0xC1]); // add rcx, rax: the new length
            code.bytes
                .extend_from_slice(&[0x48, 0x3B, 0x4A, MEMORY_CAPACITY_OFFSET]); // cmp rcx, [rdx + cap]
            let fail = forward(code, 0x77); // ja fail
            code.bytes
                .extend_from_slice(&[0x48, 0x89, 0x4A, MEMORY_LENGTH_OFFSET]); // mov [rdx + len], rcx
            code.bytes.extend_from_slice(&[0x48, 0xC1, 0xE8, 0x10]); // shr rax, 16: the old pages
            let done = forward(code, 0xEB); // jmp done
            land(code, fail);
            code.bytes
                .extend_from_slice(&[0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF]); // mov rax, -1
            land(code, done);
            code.store(EAX, *dst);
        }
        MirOp::Load {
            op,
            dst,
            address,
            offset,
        } => {
            emit_address(code, *address, *offset, op.width(), context_slot);
            // Each form fills the whole of rax: the 32-bit moves zero-extend, and the signed forms
            // sign-extend to 64 bits.
            let access: &[u8] = match op {
                LoadOp::Quad => &[0x48, 0x8B, 0x04, 0x02], // mov rax, [rdx + rax]
                LoadOp::Word => &[0x8B, 0x04, 0x02],       // mov eax, [rdx + rax]
                LoadOp::SignedWord => &[0x48, 0x63, 0x04, 0x02], // movsxd rax, dword [...]
                LoadOp::SignedByte => &[0x48, 0x0F, 0xBE, 0x04, 0x02], // movsx rax, byte [...]
                LoadOp::UnsignedByte => &[0x0F, 0xB6, 0x04, 0x02], // movzx eax, byte [...]
                LoadOp::SignedHalf => &[0x48, 0x0F, 0xBF, 0x04, 0x02], // movsx rax, word [...]
                LoadOp::UnsignedHalf => &[0x0F, 0xB7, 0x04, 0x02], // movzx eax, word [...]
            };
            code.bytes.extend_from_slice(access);
            code.store(EAX, *dst);
        }
        MirOp::Store {
            op,
            address,
            value,
            offset,
        } => {
            emit_address(code, *address, *offset, op.width(), context_slot);
            code.load(ECX, *value);
            let access: &[u8] = match op {
                StoreOp::Quad => &[0x48, 0x89, 0x0C, 0x02], // mov [rdx + rax], rcx
                StoreOp::Word => &[0x89, 0x0C, 0x02],       // mov [rdx + rax], ecx
                StoreOp::Byte => &[0x88, 0x0C, 0x02],       // mov [rdx + rax], cl
                StoreOp::Half => &[0x66, 0x89, 0x0C, 0x02], // mov [rdx + rax], cx
            };
            code.bytes.extend_from_slice(access);
        }
        MirOp::FloatBin {
            precision,
            op,
            dst,
            a,
            b,
        } => match op {
            FloatOp::Min => emit_min_max(code, true, *dst, *a, *b, *precision),
            FloatOp::Max => emit_min_max(code, false, *dst, *a, *b, *precision),
            FloatOp::Copysign => emit_copysign(code, *dst, *a, *b, *precision),
            FloatOp::Add | FloatOp::Sub | FloatOp::Mul | FloatOp::Div => {
                load_xmm(code, 0, *a, *precision);
                load_xmm(code, 1, *b, *precision);
                // The scalar form: `F2` for a double (`sd`), `F3` for a single (`ss`).
                let prefix = match precision {
                    Precision::Double => 0xF2,
                    Precision::Single => 0xF3,
                };
                let opcode = match op {
                    FloatOp::Add => 0x58,
                    FloatOp::Sub => 0x5C,
                    FloatOp::Mul => 0x59,
                    _ => 0x5E,
                };
                code.bytes.extend_from_slice(&[prefix, 0x0F, opcode, 0xC1]); // op xmm0, xmm1
                canonicalize_xmm0(code, *precision);
                store_xmm(code, 0, *dst, *precision);
            }
        },
        MirOp::FloatCompare {
            precision,
            cond,
            dst,
            a,
            b,
        } => {
            load_xmm(code, 0, *a, *precision);
            load_xmm(code, 1, *b, *precision);
            // `ucomisd` sets the flags as an unsigned compare, and an unordered pair sets zero, carry
            // and parity. "Above" is false for it, so `<` and `<=` compare the other way round.
            match cond {
                FloatComparison::Gt => {
                    ucomi(code, 0, 1, *precision);
                    code.bytes.extend_from_slice(&[0x0F, 0x97, 0xC0]); // seta al
                }
                FloatComparison::Ge => {
                    ucomi(code, 0, 1, *precision);
                    code.bytes.extend_from_slice(&[0x0F, 0x93, 0xC0]); // setae al
                }
                FloatComparison::Lt => {
                    ucomi(code, 1, 0, *precision);
                    code.bytes.extend_from_slice(&[0x0F, 0x97, 0xC0]); // seta al
                }
                FloatComparison::Le => {
                    ucomi(code, 1, 0, *precision);
                    code.bytes.extend_from_slice(&[0x0F, 0x93, 0xC0]); // setae al
                }
                FloatComparison::Eq => {
                    // Equal means zero and not unordered: the zero flag, with no parity.
                    ucomi(code, 0, 1, *precision);
                    code.bytes.extend_from_slice(&[0x0F, 0x94, 0xC0]); // sete al
                    code.bytes.extend_from_slice(&[0x0F, 0x9B, 0xC1]); // setnp cl
                    code.bytes.extend_from_slice(&[0x20, 0xC8]); // and al, cl
                }
                FloatComparison::Ne => {
                    // Not equal is the complement, so an unordered pair compares unequal.
                    ucomi(code, 0, 1, *precision);
                    code.bytes.extend_from_slice(&[0x0F, 0x95, 0xC0]); // setne al
                    code.bytes.extend_from_slice(&[0x0F, 0x9A, 0xC1]); // setp cl
                    code.bytes.extend_from_slice(&[0x08, 0xC8]); // or al, cl
                }
            }
            code.bytes.extend_from_slice(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
            code.store(EAX, *dst);
        }
        MirOp::FloatUnary {
            precision,
            op,
            dst,
            a,
        } => match op {
            FloatUnaryOp::Ceil => emit_round(code, 2, *dst, *a, *precision),
            FloatUnaryOp::Floor => emit_round(code, 1, *dst, *a, *precision),
            FloatUnaryOp::Trunc => emit_round(code, 3, *dst, *a, *precision),
            FloatUnaryOp::Nearest => emit_round(code, 0, *dst, *a, *precision),
            FloatUnaryOp::Sqrt => {
                load_xmm(code, 0, *a, *precision);
                // sqrtsd (`F2 0F 51`) or sqrtss (`F3 0F 51`), on xmm0 itself.
                let prefix = match precision {
                    Precision::Double => 0xF2,
                    Precision::Single => 0xF3,
                };
                code.bytes.extend_from_slice(&[prefix, 0x0F, 0x51, 0xC0]);
                canonicalize_xmm0(code, *precision);
                store_xmm(code, 0, *dst, *precision);
            }
            // The sign operations work on the bits, so a NaN keeps its payload.
            FloatUnaryOp::Neg | FloatUnaryOp::Abs => {
                let neg = matches!(op, FloatUnaryOp::Neg);
                match precision {
                    Precision::Double => {
                        let mask: u64 = if neg {
                            0x8000_0000_0000_0000
                        } else {
                            0x7FFF_FFFF_FFFF_FFFF
                        };
                        code.load(EAX, *a);
                        code.bytes.extend_from_slice(&[0x48, 0xB9]); // movabs rcx, mask
                        code.bytes.extend_from_slice(&mask.to_le_bytes());
                        if neg {
                            code.bytes.extend_from_slice(&[0x48, 0x31, 0xC8]); // xor rax, rcx
                        } else {
                            code.bytes.extend_from_slice(&[0x48, 0x21, 0xC8]); // and rax, rcx
                        }
                    }
                    Precision::Single => {
                        // The low 32 bits, with the sign bit flipped or cleared.
                        code.load32(EAX, *a);
                        if neg {
                            code.bytes.push(0x35); // xor eax, 0x8000_0000
                            code.bytes.extend_from_slice(&0x8000_0000_u32.to_le_bytes());
                        } else {
                            code.bytes.push(0x25); // and eax, 0x7FFF_FFFF
                            code.bytes.extend_from_slice(&0x7FFF_FFFF_u32.to_le_bytes());
                        }
                    }
                }
                code.store(EAX, *dst);
            }
        },
        MirOp::Narrow { dst, src } => {
            code.load32(EAX, *src); // zero-extends into rax
            code.store(EAX, *dst);
        }
        MirOp::SignExtend { dst, src, bits } => {
            code.load(EAX, *src);
            match bits {
                8 => code.bytes.extend_from_slice(&[0x48, 0x0F, 0xBE, 0xC0]), // movsx rax, al
                16 => code.bytes.extend_from_slice(&[0x48, 0x0F, 0xBF, 0xC0]), // movsx rax, ax
                _ => code.bytes.extend_from_slice(&[0x48, 0x63, 0xC0]),       // movsxd rax, eax
            }
            code.store(EAX, *dst);
        }
        MirOp::IntToFloat { to, from, dst, src } => emit_int_to_float(code, *to, *from, *dst, *src),
        MirOp::FloatToFloat { to, dst, src } => emit_float_to_float(code, *to, *dst, *src),
        MirOp::FloatToInt { from, to, dst, src } => {
            emit_float_to_int(code, *from, *to, *dst, *src, context_slot)
        }
        MirOp::Unary64 { op, dst, a } => {
            code.load(ECX, *a);
            emit_unary64(code, *op);
            code.store(EAX, *dst);
        }
        MirOp::Unary { op, dst, a } => {
            code.load(ECX, *a);
            emit_unary(code, *op);
            code.store(EAX, *dst);
        }
        MirOp::Divide { op, dst, a, b } => {
            code.load(EAX, *a);
            code.load(ECX, *b);
            emit_division(code, *op, context_slot);
            code.store(EAX, *dst);
        }
        MirOp::Divide64 { op, dst, a, b } => {
            code.load(EAX, *a);
            code.load(ECX, *b);
            emit_division64(code, *op, context_slot);
            code.store(EAX, *dst);
        }
        MirOp::Const { dst, value } => {
            code.bytes.push(0xB8); // mov eax, imm32: zero-extends into rax
            code.bytes.extend_from_slice(&value.to_le_bytes());
            code.store(EAX, *dst);
        }
        MirOp::Const64 { dst, value } => {
            code.bytes.extend_from_slice(&[0x48, 0xB8]); // movabs rax, imm64
            code.bytes.extend_from_slice(&value.to_le_bytes());
            code.store(EAX, *dst);
        }
        MirOp::Move { dst, src } => {
            code.load(EAX, *src);
            code.store(EAX, *dst);
        }
        MirOp::Bin { op, dst, a, b } => {
            code.load(EAX, *a);
            code.load(ECX, *b);
            match op {
                BinOp::Add => code.register_pair(0x01), // add eax, ecx
                BinOp::Sub => code.register_pair(0x29), // sub eax, ecx
                BinOp::And => code.register_pair(0x21), // and eax, ecx
                BinOp::Or => code.register_pair(0x09),  // or eax, ecx
                BinOp::Xor => code.register_pair(0x31), // xor eax, ecx
                BinOp::Mul => code.bytes.extend_from_slice(&[0x0F, 0xAF, 0xC1]), // imul eax, ecx
                BinOp::Shl => code.bytes.extend_from_slice(&[0xD3, 0xE0]), // shl eax, cl
                BinOp::ShrS => code.bytes.extend_from_slice(&[0xD3, 0xF8]), // sar eax, cl
                BinOp::ShrU => code.bytes.extend_from_slice(&[0xD3, 0xE8]), // shr eax, cl
                BinOp::Rotl => code.bytes.extend_from_slice(&[0xD3, 0xC0]), // rol eax, cl
                BinOp::Rotr => code.bytes.extend_from_slice(&[0xD3, 0xC8]), // ror eax, cl
            }
            code.store(EAX, *dst);
        }
        MirOp::Bin64 { op, dst, a, b } => {
            code.load(EAX, *a);
            code.load(ECX, *b);
            match op {
                BinOp::Add => code.bytes.extend_from_slice(&[0x48, 0x01, 0xC8]), // add rax, rcx
                BinOp::Sub => code.bytes.extend_from_slice(&[0x48, 0x29, 0xC8]), // sub rax, rcx
                BinOp::And => code.bytes.extend_from_slice(&[0x48, 0x21, 0xC8]), // and rax, rcx
                BinOp::Or => code.bytes.extend_from_slice(&[0x48, 0x09, 0xC8]),  // or rax, rcx
                BinOp::Xor => code.bytes.extend_from_slice(&[0x48, 0x31, 0xC8]), // xor rax, rcx
                BinOp::Mul => code.bytes.extend_from_slice(&[0x48, 0x0F, 0xAF, 0xC1]), // imul rax, rcx
                BinOp::Shl => code.bytes.extend_from_slice(&[0x48, 0xD3, 0xE0]), // shl rax, cl
                BinOp::ShrS => code.bytes.extend_from_slice(&[0x48, 0xD3, 0xF8]), // sar rax, cl
                BinOp::ShrU => code.bytes.extend_from_slice(&[0x48, 0xD3, 0xE8]), // shr rax, cl
                BinOp::Rotl => code.bytes.extend_from_slice(&[0x48, 0xD3, 0xC0]), // rol rax, cl
                BinOp::Rotr => code.bytes.extend_from_slice(&[0x48, 0xD3, 0xC8]), // ror rax, cl
            }
            code.store(EAX, *dst);
        }
        MirOp::Compare { cond, dst, a, b } => {
            code.load(EAX, *a);
            code.load(ECX, *b);
            code.register_pair(0x39); // cmp eax, ecx
            let condition = condition_code(*cond);
            code.bytes.extend_from_slice(&[0x0F, condition, 0xC0]); // setcc al
            code.bytes.extend_from_slice(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
            code.store(EAX, *dst);
        }
        MirOp::Compare64 { cond, dst, a, b } => {
            code.load(EAX, *a);
            code.load(ECX, *b);
            code.bytes.extend_from_slice(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
            let condition = condition_code(*cond);
            code.bytes.extend_from_slice(&[0x0F, condition, 0xC0]); // setcc al
            code.bytes.extend_from_slice(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
            code.store(EAX, *dst);
        }
        MirOp::Eqz { dst, a } => {
            code.load(EAX, *a);
            code.bytes.extend_from_slice(&[0x85, 0xC0]); // test eax, eax
            code.bytes.extend_from_slice(&[0x0F, 0x94, 0xC0]); // sete al
            code.bytes.extend_from_slice(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
            code.store(EAX, *dst);
        }
        MirOp::Eqz64 { dst, a } => {
            code.load(EAX, *a);
            code.bytes.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
            code.bytes.extend_from_slice(&[0x0F, 0x94, 0xC0]); // sete al
            code.bytes.extend_from_slice(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
            code.store(EAX, *dst);
        }
        MirOp::Select { dst, cond, a, b } => {
            // The condition is an `i32`, so its low half is tested. Flags come from the test; the
            // loads that follow leave them alone.
            code.load32(EDX, *cond);
            code.bytes.extend_from_slice(&[0x85, 0xD2]); // test edx, edx
            code.load(EAX, *a);
            code.load(ECX, *b);
            code.bytes.extend_from_slice(&[0x48, 0x0F, 0x44, 0xC1]); // cmovz rax, rcx
            code.store(EAX, *dst);
        }
    }
}

/// The `setcc` opcode for a comparison's condition.
fn condition_code(cond: IntComparison) -> u8 {
    match cond {
        IntComparison::Eq => 0x94,
        IntComparison::Ne => 0x95,
        IntComparison::LtS => 0x9C,
        IntComparison::GeS => 0x9D,
        IntComparison::LeS => 0x9E,
        IntComparison::GtS => 0x9F,
        IntComparison::LtU => 0x92,
        IntComparison::GeU => 0x93,
        IntComparison::LeU => 0x96,
        IntComparison::GtU => 0x97,
    }
}
