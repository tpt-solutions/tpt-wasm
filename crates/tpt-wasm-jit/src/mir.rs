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
//! This is the first stage of the pipeline `IR -> machine IR -> native code`. The slice here is
//! the `i32` integer subset: constants, locals, the wrapping arithmetic and bitwise operators,
//! shifts, comparisons, `eqz`, `select`, and the control terminators. Anything else is refused
//! with [`JitError::Unsupported`] rather than approximated. The native encoders in this crate
//! still read the IR directly; they have not been moved onto this stage, so this module's
//! results are checked against Micro by interpretation, not by running machine code.

use std::collections::HashMap;

use tpt_wasm_ir::{BlockId, IntComparison, IrFunction, IrInstr, Terminator, ValueId};
use tpt_wasm_types::Trap;

use crate::JitError;

/// A numbered storage location: a value, a local, or a temporary of this function.
pub type Slot = u32;

/// The `i32` operators the machine IR has, with WebAssembly's wrapping and masking rules.
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
}

/// One machine operation. Every operation names its destination and its sources by slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirOp {
    /// `dst = value`.
    Const { dst: Slot, value: i32 },
    /// `dst = src`.
    Move { dst: Slot, src: Slot },
    /// `dst = a op b`, with the operator's wrapping and shift-count rules.
    Bin {
        op: BinOp,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = (a cond b) as i32`.
    Compare {
        cond: IntComparison,
        dst: Slot,
        a: Slot,
        b: Slot,
    },
    /// `dst = (a == 0) as i32`.
    Eqz { dst: Slot, a: Slot },
    /// `dst = if cond != 0 { a } else { b }`.
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

/// A function in machine IR. Its arguments arrive in `params`, its blocks are indexed from
/// zero, and it starts at `entry`.
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
}

/// Lower a verified IR function to machine IR.
///
/// # Errors
///
/// [`JitError::Unsupported`] for an instruction or a value type outside the `i32` integer slice.
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
        if let Some(value) = function
            .values
            .iter()
            .find(|value| !matches!(value.value_type, tpt_wasm_types::ValueType::I32))
        {
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
        let lowered = match instruction {
            I::ConstI32 { result, value } => MirOp::Const {
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
            I::I32Eqz { result, value } => MirOp::Eqz {
                dst: Self::slot(*result),
                a: Self::slot(*value),
            },
            other => {
                return Err(JitError::Unsupported(format!(
                    "{} in machine IR",
                    crate::x86::instruction_name(other)
                )))
            }
        };
        ops.push(lowered);
        Ok(())
    }
}

/// Run a machine-IR function on `arguments`, returning its results.
///
/// `budget` bounds the number of blocks entered, so a loop that does not end is reported as
/// [`Trap::StepsExhausted`] rather than hanging.
///
/// # Errors
///
/// [`MirError::Trap`] when the function traps or runs out of budget.
pub fn run(function: &MirFunction, arguments: &[i32], budget: u64) -> Result<Vec<i32>, MirError> {
    let mut slots = vec![0_i32; function.slots as usize];
    for (slot, value) in function.params.iter().zip(arguments) {
        slots[*slot as usize] = *value;
    }
    let mut block = function.entry;
    let mut entered = 0_u64;
    loop {
        entered += 1;
        if entered > budget {
            return Err(MirError::Trap(Trap::StepsExhausted));
        }
        for op in &function.blocks[block].ops {
            execute(op, &mut slots);
        }
        match &function.blocks[block].term {
            MirTerm::Jump(next) => block = *next,
            MirTerm::Branch {
                cond,
                then_block,
                else_block,
            } => {
                block = if slots[*cond as usize] != 0 {
                    *then_block
                } else {
                    *else_block
                };
            }
            MirTerm::Return(values) => {
                return Ok(values.iter().map(|slot| slots[*slot as usize]).collect());
            }
            MirTerm::Trap(trap) => return Err(MirError::Trap(trap.clone())),
        }
    }
}

fn execute(op: &MirOp, slots: &mut [i32]) {
    match op {
        MirOp::Const { dst, value } => slots[*dst as usize] = *value,
        MirOp::Move { dst, src } => slots[*dst as usize] = slots[*src as usize],
        MirOp::Bin { op, dst, a, b } => {
            let (x, y) = (slots[*a as usize], slots[*b as usize]);
            slots[*dst as usize] = match op {
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
            };
        }
        MirOp::Compare { cond, dst, a, b } => {
            let (x, y) = (slots[*a as usize], slots[*b as usize]);
            let holds = match cond {
                IntComparison::Eq => x == y,
                IntComparison::Ne => x != y,
                IntComparison::LtS => x < y,
                IntComparison::LtU => (x as u32) < (y as u32),
                IntComparison::GtS => x > y,
                IntComparison::GtU => (x as u32) > (y as u32),
                IntComparison::LeS => x <= y,
                IntComparison::LeU => (x as u32) <= (y as u32),
                IntComparison::GeS => x >= y,
                IntComparison::GeU => (x as u32) >= (y as u32),
            };
            slots[*dst as usize] = i32::from(holds);
        }
        MirOp::Eqz { dst, a } => slots[*dst as usize] = i32::from(slots[*a as usize] == 0),
        MirOp::Select { dst, cond, a, b } => {
            slots[*dst as usize] = if slots[*cond as usize] != 0 {
                slots[*a as usize]
            } else {
                slots[*b as usize]
            };
        }
    }
}
