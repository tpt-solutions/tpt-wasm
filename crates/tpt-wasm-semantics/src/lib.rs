// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Formal abstract machine for WebAssembly.
//!
//! Defines the abstract Configuration and transition relation (C → C')
//! mirroring the WebAssembly Core spec §4 execution semantics.
//!
//! The Lean 4 formal proofs live in formal/ at the repository root.
//! This crate provides the Rust-side of the correspondence:
//!
//!   FormalRule(op) ≈ Interpreter(op)   (per instruction)
//!
//! Status: M5 — not yet implemented.

use tpt_wasm_types::Value;

/// The abstract machine configuration.
///
/// C = { Store, FrameStack, OperandStack, ControlStack }
#[derive(Debug)]
pub struct Configuration {
    // TODO(M5): implement full formal configuration
}

/// A formal transition result.
pub enum Transition {
    /// The machine advanced to a new configuration.
    Step(Configuration),
    /// The machine returned with values.
    Return(Vec<Value>),
    /// The machine trapped.
    Trap(tpt_wasm_types::Trap),
}
