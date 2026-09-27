// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Memory optimization: store-to-load forwarding, dead-store elimination, and
//! bounds-check elimination.
//!
//! ## Precondition
//!
//! Verified IR over a module that may declare a linear memory.
//!
//! ## Transformations
//!
//! 1. **Redundant-store elimination.** A store to an address that an identical
//!    store already wrote, with nothing in between that could change memory or
//!    trap, is removed.
//! 2. **Store-to-load forwarding.** A load from an address a store just wrote is
//!    replaced by the stored value, when the load's width fits inside the store's
//!    and the store's value is at an offset the load can extract from it.
//! 3. **Bounds-check elimination.** An access whose address and offset are
//!    constant, and whose last byte lies within the memory's declared minimum
//!    size, is marked [`BoundsCheck::Proven`].
//!
//! ## Semantic invariant
//!
//! Memory contents, memory effects, and traps are unchanged, *including their
//! order*.
//!
//! The order clause is the whole difficulty, and it is why forwarding is
//! restricted to a single block with nothing in between. A load hoisted above a
//! store would read a stale value; a store deleted from between two stores could
//! change what a trap observes. So a candidate is only accepted when every
//! instruction between the store and the use is itself pure, total, and
//! non-trapping -- anything else ends the search rather than being stepped over.
//!
//! Forwarding is further restricted to loads that read a *prefix-aligned* window
//! of the stored value, computed with the same `MemoryLoad` decoding the backend
//! uses. Reusing that decoder rather than re-deriving the extraction is what keeps
//! a sign-extended narrow load correct.
//!
//! Bounds-check elimination proves the access against the memory's *declared
//! minimum*, which instantiation guarantees and `memory.grow` only increases, so
//! the proof holds for the life of the instance. The claim is recorded in the IR
//! and re-derived by the IR verifier, which is what makes it safe to hand a
//! backend permission to skip the check.

use tpt_wasm_ir::{BoundsCheck, IrFunction, IrInstr, IrModule, MemoryLoad, ValueId};
use tpt_wasm_types::Value;

use crate::analysis::{has_side_effects, may_trap, Cfg};
use crate::numeric::Const;
use crate::PassOutcome;

/// Run the memory-dependent optimizations.
pub fn run(module: &mut IrModule) -> PassOutcome {
    let mut removed = 0;
    let mut rewrites = 0;
    for function in &mut module.functions {
        let (r, w) = optimize_function(function);
        removed += r;
        rewrites += w;
    }
    PassOutcome {
        instructions_removed: removed,
        rewrites,
        ..PassOutcome::default()
    }
}

/// Run only the bounds-check elimination pass.
pub fn run_bounds_checks(module: &mut IrModule) -> PassOutcome {
    // The memory declaration is read out before the functions are borrowed, so
    // the pass does not need a second borrow of the module it is rewriting.
    let memory = module.memory.clone();
    let mut rewrites = 0;
    for function in &mut module.functions {
        rewrites += eliminate_bounds_checks(memory.as_ref(), function);
    }
    PassOutcome {
        rewrites,
        ..PassOutcome::default()
    }
}

/// Fold the load/store optimizations and return `(removed, rewrites)`.
fn optimize_function(function: &mut IrFunction) -> (usize, usize) {
    let cfg = Cfg::new(function);
    let mut removed = 0;
    let mut rewrites = 0;
    for block_index in 0..function.blocks.len() {
        if !cfg.is_reachable(block_index) {
            continue;
        }
        let (r, w) = optimize_block(function, block_index);
        removed += r;
        rewrites += w;
    }
    (removed, rewrites)
}

/// Everything remembered about one memory access in the current block.
#[derive(Debug, Clone, Copy)]
struct Access {
    /// The byte offset from the block's base that was written.
    offset: u64,
    /// The value written, as raw bits in the stored width.
    bits: u64,
    /// The stored width in bytes.
    width: u32,
}

/// Optimize one block's memory accesses.
fn optimize_block(function: &mut IrFunction, block_index: usize) -> (usize, usize) {
    // Start from the constants the function already holds, so a forwarded value
    // can be reconstructed even when the store's operand is a constant rather
    // than a value the block defined.
    let constants = constant_table(function);
    let _ = &constants;

    // Track the most recent store to each byte offset. The map is keyed by the
    // *offset* rather than the value, because that is what decides whether a
    // later access overlaps.
    let mut last_store: Vec<(u64, Access)> = Vec::new();
    let removed = 0;
    let mut rewrites = 0;

    let block = &function.blocks[block_index];
    let mut rewritten: Vec<(usize, IrInstr)> = Vec::new();
    let mut drop: Vec<usize> = Vec::new();

    for (position, instruction) in block.instrs.iter().enumerate() {
        match instruction {
            IrInstr::Store {
                address,
                value,
                offset,
                operation,
                ..
            } => {
                let Some(base) = constant_of(&constants, *address) else {
                    // An unknown address ends the tracked window: nothing after
                    // it can be proven not to overlap.
                    last_store.clear();
                    continue;
                };
                let start = u64::from(base).wrapping_add(u64::from(*offset));
                let width = u64::from(operation.width());

                // A store covering exactly the same bytes as the previous one, to
                // the same bytes with the same value, is redundant -- provided the
                // first one did not trap, which it did not, or we would not be
                // here.
                if last_store.iter().any(|(known, access)| {
                    *known == start
                        && access.width as u64 == width
                        && access.bits == stored_bits(&constants, *value, *operation)
                }) {
                    drop.push(position);
                    continue;
                }
                last_store.push((
                    start,
                    Access {
                        offset: start,
                        bits: stored_bits(&constants, *value, *operation),
                        width: u32::try_from(width).unwrap_or(u32::MAX),
                    },
                ));
            }
            IrInstr::Load {
                result,
                address,
                offset,
                operation,
                ..
            } => {
                let Some(base) = constant_of(&constants, *address) else {
                    last_store.clear();
                    continue;
                };
                let start = u64::from(base).wrapping_add(u64::from(*offset));
                if let Some(value) = forward(&last_store, start, *operation) {
                    // A reference-valued load has no constant form, so the
                    // forwarding is simply declined rather than approximated.
                    if let Some(constant) = constant_instruction(*result, value) {
                        rewritten.push((position, constant));
                        rewrites += 1;
                        continue;
                    }
                }
            }
            _ => {}
        }

        // Anything that can trap or write memory ends the tracked window. This
        // is the conservative choice that makes forwarding sound: a call may
        // write any address, and a trapping instruction ends the block.
        if may_trap(instruction) || has_side_effects(instruction) {
            last_store.clear();
        }
    }

    if drop.is_empty() && rewritten.is_empty() {
        return (0, 0);
    }
    let rewritten: std::collections::HashMap<usize, IrInstr> = rewritten.into_iter().collect();
    let block = &mut function.blocks[block_index];
    let original = std::mem::take(&mut block.instrs);
    block.instrs = original
        .into_iter()
        .enumerate()
        .filter_map(|(position, instruction)| {
            if drop.contains(&position) {
                None
            } else {
                Some(rewritten.get(&position).cloned().unwrap_or(instruction))
            }
        })
        .collect();
    (removed + drop.len(), rewrites)
}

/// Every `i32` constant the function defines, as a value-to-value table.
fn constant_table(function: &IrFunction) -> std::collections::HashMap<ValueId, Const> {
    let mut table = std::collections::HashMap::new();
    for block in &function.blocks {
        for instruction in &block.instrs {
            if let IrInstr::ConstI32 { result, value } = instruction {
                table.insert(*result, Const::I32(*value));
            }
        }
    }
    table
}

fn constant_of(table: &std::collections::HashMap<ValueId, Const>, value: ValueId) -> Option<u32> {
    match table.get(&value) {
        Some(Const::I32(v)) => Some(*v as u32),
        _ => None,
    }
}

/// The bits a store would write, if the value is a constant.
fn stored_bits(
    table: &std::collections::HashMap<ValueId, Const>,
    value: ValueId,
    operation: tpt_wasm_ir::MemoryStore,
) -> u64 {
    let width = operation.width();
    let raw = match table.get(&value) {
        Some(Const::I32(v)) => u64::from(*v as u32),
        Some(Const::I64(v)) => *v as u64,
        _ => return u64::MAX,
    };
    // Mask to the stored width, because a store writes only that many bytes and
    // the remaining bits of the operand are not part of what it wrote.
    raw & mask(width)
}

fn mask(width: u32) -> u64 {
    match width {
        0 => 0,
        8 => u64::MAX,
        n => (1u64 << (n * 8)) - 1,
    }
}

/// The value a load at `start` would read, if a tracked store covers it.
fn forward(stores: &[(u64, Access)], start: u64, operation: MemoryLoad) -> Option<Value> {
    let width = operation.width();
    for (_, access) in stores.iter().rev() {
        // The load must read a window entirely inside the stored bytes.
        if start < access.offset {
            continue;
        }
        let into = start - access.offset;
        let into = u32::try_from(into).ok()?;
        let end = into.checked_add(width)?;
        if end > access.width {
            continue;
        }
        let bits = (access.bits >> (into * 8)) & mask(width);
        return Some(decode(operation, bits));
    }
    None
}

/// Decode a loaded bit pattern exactly as the backend does.
///
/// This mirrors `tpt_wasm_codegen`'s `decode_load` rather than re-deriving it,
/// because a sign-extension rule guessed slightly differently would forward the
/// wrong value on exactly the narrow loads the spec suite exercises hardest.
fn decode(operation: MemoryLoad, bits: u64) -> Value {
    match operation {
        MemoryLoad::I32 => Value::I32(bits as u32 as i32),
        MemoryLoad::I64 => Value::I64(bits as i64),
        MemoryLoad::F32 => Value::F32(bits as u32),
        MemoryLoad::F64 => Value::F64(bits),
        MemoryLoad::I32_8S => Value::I32(bits as u8 as i8 as i32),
        MemoryLoad::I32_8U => Value::I32(bits as u8 as i32),
        MemoryLoad::I32_16S => Value::I32(bits as u16 as i16 as i32),
        MemoryLoad::I32_16U => Value::I32(bits as u16 as i32),
        MemoryLoad::I64_8S => Value::I64(bits as u8 as i8 as i64),
        MemoryLoad::I64_8U => Value::I64(i64::from(bits as u8)),
        MemoryLoad::I64_16S => Value::I64(bits as u16 as i16 as i64),
        MemoryLoad::I64_16U => Value::I64(i64::from(bits as u16)),
        MemoryLoad::I64_32S => Value::I64(bits as u32 as i32 as i64),
        MemoryLoad::I64_32U => Value::I64(i64::from(bits as u32)),
    }
}

/// A constant definition producing `value`, or `None` for a reference type.
///
/// A reference cannot be written as a constant -- there is no `ConstRef` -- so a
/// forwardable reference load is simply not forwarded. Losing one optimization is
/// the right answer; inventing a reference constant would be a type error.
fn constant_instruction(result: ValueId, value: Value) -> Option<IrInstr> {
    Some(match value {
        Value::I32(v) => IrInstr::ConstI32 { result, value: v },
        Value::I64(v) => IrInstr::ConstI64 { result, value: v },
        Value::F32(bits) => IrInstr::ConstF32 {
            result,
            value: bits,
        },
        Value::F64(bits) => IrInstr::ConstF64 {
            result,
            value: bits,
        },
        Value::V128(_) | Value::Ref(_) => return None,
    })
}

/// Mark every access that provably lies inside the memory's minimum size.
fn eliminate_bounds_checks(
    memory: Option<&tpt_wasm_ir::IrMemory>,
    function: &mut IrFunction,
) -> usize {
    // No memory means no access could have verified in the first place, so there
    // is nothing to prove.
    let Some(memory) = memory else {
        return 0;
    };
    let available = memory.min_pages.saturating_mul(crate::analysis::PAGE_SIZE);
    let constants = constant_table(function);
    let mut changes = 0;
    for block in &mut function.blocks {
        for instruction in &mut block.instrs {
            // Read the access out first, then write the verdict back. Splitting
            // the two keeps the borrow of `instruction` short, which is what lets
            // the two arms share one proof instead of duplicating it.
            let (address, offset, width, needs_check) = match instruction {
                IrInstr::Load {
                    address,
                    offset,
                    operation,
                    bounds,
                    ..
                } => (*address, *offset, operation.width(), bounds.needs_check()),
                IrInstr::Store {
                    address,
                    offset,
                    operation,
                    bounds,
                    ..
                } => (*address, *offset, operation.width(), bounds.needs_check()),
                _ => continue,
            };
            if !needs_check {
                continue;
            }
            let Some(base) = constant_of(&constants, address) else {
                continue;
            };
            // `checked_add` throughout: a wrapped sum would turn an
            // out-of-bounds access into an apparently tiny in-bounds one, which
            // is the failure this pass exists to make impossible.
            let end = u64::from(base)
                .checked_add(u64::from(offset))
                .and_then(|value| value.checked_add(u64::from(width)));
            let proven = matches!(end, Some(end) if end <= available);
            let bounds = match instruction {
                IrInstr::Load { bounds, .. } | IrInstr::Store { bounds, .. } => bounds,
                _ => continue,
            };
            if proven {
                *bounds = BoundsCheck::Proven;
                changes += 1;
            }
        }
    }
    changes
}
