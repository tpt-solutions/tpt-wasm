// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Baseline code generation.
//!
//! Priorities: correctness, startup time, predictable output, easy debugging.
//! NOT peak performance — that is the Optimizing compiler's job.
//!
//! Pipeline: Wasm → TPT IR → simple lowering → native code
//!
//! Status: M7 — not yet implemented.

/// Target architecture for code generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetArch {
    X86_64,
    Aarch64,
    Riscv64,
}

/// A compiled function (native code bytes + metadata).
#[derive(Debug)]
pub struct CompiledFunction {
    pub arch: TargetArch,
    pub code: Vec<u8>,
}
