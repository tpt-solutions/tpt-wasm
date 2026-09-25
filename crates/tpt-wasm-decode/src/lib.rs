// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly binary format decoder.
//!
//! Decoding is strictly separate from validation and execution. A successfully
//! decoded module may still fail validation.

mod decoder;
mod encoder;

pub use tpt_wasm_format::Module;

#[cfg(test)]
mod tests;

pub use decoder::decode;
pub use encoder::encode;

/// Errors produced during binary decoding.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    UnexpectedEof,
    InvalidMagic,
    InvalidVersion,
    InvalidSectionId(u8),
    DuplicateSection(u8),
    InvalidOpcode(u32),
    InvalidLeb128,
    InvalidUtf8,
    SectionTooLarge,
    UnexpectedData,
    InvalidTypeForm,
    UnsupportedValueType(u8),
    UnsupportedReferenceType(u8),
    UnsupportedLimitFlags(u8),
    UnsupportedMutability(u8),
    InvalidImportKind(u8),
    InvalidExportKind(u8),
    UnsupportedElementSegmentKind,
    UnsupportedDataSegmentKind,
    MismatchedCodeSection,
    Custom(String),
    UnsupportedFeature(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DecodeError {}

/// Errors produced while encoding a structural module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    ValueTooLarge,
    InvalidConstExpr,
    UnsupportedValueType,
    UnsupportedReferenceType,
    UnsupportedFeature(&'static str),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for EncodeError {}
