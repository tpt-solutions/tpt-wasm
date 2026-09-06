// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly binary format decoder.
//!
//! Responsibility: bytes → Module
//!
//! Decoding is strictly separate from validation and execution.
//! A successfully decoded module may still fail validation.

use tpt_wasm_format::Module;

/// Errors produced during binary decoding.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    UnexpectedEof,
    InvalidMagic,
    InvalidVersion,
    InvalidSectionId(u8),
    InvalidOpcode(u32),
    InvalidLeb128,
    InvalidUtf8,
    SectionTooLarge,
    UnexpectedData,
    Custom(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for DecodeError {}

/// Decode a WebAssembly binary module.
///
/// Returns the structural module representation or a decode error.
/// Does not perform validation.
pub fn decode(bytes: &[u8]) -> Result<Module, DecodeError> {
    // TODO(M1): implement full binary decoder
    let _ = bytes;
    Err(DecodeError::Custom("decoder not yet implemented".into()))
}
