// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! x86-64 code generated from the machine IR.
//!
//! This encoder reads a [`MirFunction`], not the IR, so the native path is
//! `IR -> machine IR -> x86-64`. It handles the same `i32` slice as the machine IR, and it is
//! deliberately simple: every slot lives in its own 8-byte frame slot, and each operation loads
//! its operands into `eax`, `ecx` and `edx`, computes, and stores the result back. Nothing is kept
//! in a register across operations. That costs speed, and it makes each encoding easy to check
//! against the manual, which is the point of this first native stage.
//!
//! The code follows the same convention as the existing encoders, so it is run through the same
//! `CompiledFunction`: the function receives a pointer to its arguments (eight-byte entries) and a
//! context pointer, and returns the result in `rax`. The status word in the context stays zero, so
//! a successful run reports its value. Traps and calls are refused, because the slice has none.
//!
//! The encodings are the standard ones and are listed next to each emitter. The `i32` operations
//! use the 32-bit forms, so their results wrap, and shifts take their count from `cl`, which the
//! hardware reduces modulo 32, as WebAssembly requires.

use tpt_wasm_ir::IntComparison;

use crate::mir::{BinOp, MirFunction, MirOp, MirTerm, Slot};
use crate::x86::Abi;
use crate::JitError;

/// Register numbers in the ModR/M encoding.
const EAX: u8 = 0;
const ECX: u8 = 1;
const EDX: u8 = 2;

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

    /// `mov reg32, [slot]`: `8B /r`.
    fn load(&mut self, reg: u8, slot: Slot) {
        self.with_frame_operand(&[0x8B], reg, Self::slot(slot));
    }

    /// `mov [slot], reg32`: `89 /r`.
    fn store(&mut self, reg: u8, slot: Slot) {
        self.with_frame_operand(&[0x89], reg, Self::slot(slot));
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
/// [`JitError::Unsupported`] for a trap terminator, a return of more than one value, or an empty
/// function. Division, calls, memory, and the other forms are refused earlier, by the machine IR.
pub(crate) fn encode(function: &MirFunction, abi: Abi) -> Result<Vec<u8>, JitError> {
    if function.blocks.is_empty() {
        return Err(JitError::Unsupported(
            "a machine function with no blocks".into(),
        ));
    }
    let mut code = Code {
        bytes: Vec::new(),
        labels: vec![None; function.blocks.len()],
        fixups: Vec::new(),
    };

    // push rbp; mov rbp, rsp; sub rsp, frame. The frame holds every slot and the saved argument
    // pointer, rounded up to 16 bytes so the stack stays aligned at calls (none are made here).
    let frame = ((8 * (function.slots as i32 + 1)) + 15) & !15;
    code.bytes.push(0x55);
    code.bytes.extend_from_slice(&[0x48, 0x89, 0xE5]);
    code.bytes.extend_from_slice(&[0x48, 0x81, 0xEC]);
    code.bytes.extend_from_slice(&frame.to_le_bytes());

    // Every slot starts at zero, as the machine IR does: a declared local is read before it is
    // written, and WebAssembly defines that read as zero. `mov dword [rbp + disp], 0`: C7 /0.
    for slot in 0..function.slots {
        code.with_frame_operand(&[0xC7], 0, Code::slot(slot));
        code.bytes.extend_from_slice(&0_i32.to_le_bytes());
    }

    // Keep the argument pointer in the slot just past the last one: `mov [rbp + disp], args`.
    let arguments_slot = function.slots;
    code.with_frame_operand(
        &[0x48, 0x89],
        abi.args_register(),
        Code::slot(arguments_slot),
    );

    // Each parameter: `mov rax, [rbp + args]` then `mov eax, [rax + 8 * i]`, then into its slot.
    for (index, slot) in function.params.iter().enumerate() {
        code.with_frame_operand(&[0x48, 0x8B], EAX, Code::slot(arguments_slot));
        code.bytes.extend_from_slice(&[0x8B, 0x80]); // mov eax, [rax + disp32]
        code.bytes
            .extend_from_slice(&(8 * index as i32).to_le_bytes());
        code.store(EAX, *slot);
    }
    // jmp entry
    code.branch(&[0xE9], function.entry);

    for (index, block) in function.blocks.iter().enumerate() {
        code.labels[index] = Some(code.bytes.len());
        for op in &block.ops {
            emit_op(&mut code, op);
        }
        match &block.term {
            MirTerm::Jump(target) => code.branch(&[0xE9], *target),
            MirTerm::Branch {
                cond,
                then_block,
                else_block,
            } => {
                code.load(EAX, *cond);
                code.bytes.extend_from_slice(&[0x85, 0xC0]); // test eax, eax
                code.branch(&[0x0F, 0x85], *then_block); // jnz then
                code.branch(&[0xE9], *else_block); // jmp else
            }
            MirTerm::Return(values) => {
                match values.as_slice() {
                    [] => code.bytes.extend_from_slice(&[0x31, 0xC0]), // xor eax, eax
                    [value] => code.load(EAX, *value),
                    _ => {
                        return Err(JitError::Unsupported(
                            "a machine function that returns more than one value".into(),
                        ))
                    }
                }
                code.bytes.extend_from_slice(&[0xC9, 0xC3]); // leave; ret
            }
            MirTerm::Trap(_) => {
                return Err(JitError::Unsupported(
                    "a trap in machine code; the slice has no trapping operations".into(),
                ))
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

fn emit_op(code: &mut Code, op: &MirOp) {
    match op {
        MirOp::Const { dst, value } => {
            code.bytes.push(0xB8); // mov eax, imm32
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
            }
            code.store(EAX, *dst);
        }
        MirOp::Compare { cond, dst, a, b } => {
            code.load(EAX, *a);
            code.load(ECX, *b);
            code.register_pair(0x39); // cmp eax, ecx
            let condition = match cond {
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
            };
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
        MirOp::Select { dst, cond, a, b } => {
            // Flags come from testing the condition; the loads that follow leave them alone.
            code.load(EDX, *cond);
            code.bytes.extend_from_slice(&[0x85, 0xD2]); // test edx, edx
            code.load(EAX, *a);
            code.load(ECX, *b);
            code.bytes.extend_from_slice(&[0x0F, 0x44, 0xC1]); // cmovz eax, ecx
            code.store(EAX, *dst);
        }
    }
}
