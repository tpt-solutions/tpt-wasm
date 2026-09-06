// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! JIT compiler.
//!
//! Unsafe is permitted only at the executable-memory boundary.
//! Every unsafe block must document its safety invariant.
//!
//! Safe semantic runtime → code generation boundary → executable memory
//!
//! Status: M8 — not yet implemented.
