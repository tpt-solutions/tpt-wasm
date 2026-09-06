// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The Micro Interpreter — the Golden Machine.
//!
//! This is the canonical, minimal, executable definition of WebAssembly semantics.
//! It is intentionally NOT optimized. It exists to be correct and obvious.
//!
//! Design invariants:
//!   - unsafe blocks: target 0
//!   - Wasm traps are never Rust panics
//!   - Every optimized implementation must match this interpreter's observable behavior

pub mod instr;
pub mod machine;
pub mod store;

pub use machine::{Machine, Step};
pub use store::Store;

/// Execute a single step of the WebAssembly abstract machine.
pub fn step(machine: &mut Machine) -> Step {
    machine::step(machine)
}
