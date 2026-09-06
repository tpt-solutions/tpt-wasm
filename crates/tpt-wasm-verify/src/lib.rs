// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Verification infrastructure.
//!
//! Contains: invariants, contracts, model checking, property tests,
//! semantic correspondence checks, proof harnesses.
//!
//! Verification ladder:
//!   V0  Type invariants
//!   V1  Interpreter correspondence
//!   V2  Memory safety
//!   V3  Host capability safety
//!   V4  IR refinement
//!   V5  Compiler transformation correctness
//!   V6  Machine-code refinement
//!
//! Lean 4 proofs live in formal/ at repository root.
//! Status: M5 — not yet implemented.

/// Verification level identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VerificationLevel {
    V0TypeInvariants,
    V1InterpreterCorrespondence,
    V2MemorySafety,
    V3HostCapabilitySafety,
    V4IrRefinement,
    V5CompilerTransformations,
    V6MachineCodeRefinement,
}
