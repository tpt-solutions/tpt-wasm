// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly validation.
//!
//! Validation consumes the structural [`Module`](tpt_wasm_format::Module)
//! produced by the binary decoder. It performs no decoding and does not
//! instantiate or execute the module.

mod body;
mod validator;

#[cfg(test)]
mod tests;

use tpt_wasm_format::Module;

/// Errors produced during validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    TypeMismatch { expected: String, actual: String },
    UnknownFunction(u32),
    UnknownGlobal(u32),
    UnknownMemory(u32),
    UnknownTable(u32),
    UnknownType(u32),
    UnknownLabel(u32),
    ImmutableGlobal(u32),
    InvalidStartFunction,
    DuplicateExport(String),
    InvalidInstruction(u8),
    InvalidBody(String),
    InvalidBlockType,
    InvalidConstantExpression,
    InvalidLeb128,
    InvalidLimits(String),
    InvalidLocalCount,
    InvalidMemoryIndex(u32),
    InvalidMemoryAlignment(u32),
    StackUnderflow,
    UnexpectedEof,
    InvalidBranchTypes,
    UnsupportedFeature(&'static str),
    Custom(String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ValidationError {}

/// A module that has passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedModule {
    pub module: Module,
    pub certificate: ValidationCertificate,
}

/// Architectural hook for future formal proofs.
///
/// The private marker prevents callers from fabricating a validation result
/// without going through [`Validator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationCertificate {
    _private: (),
}

/// Stateless WebAssembly module validator.
#[derive(Debug, Default, Clone, Copy)]
pub struct Validator;

impl Validator {
    pub const fn new() -> Self {
        Self
    }

    pub fn validate(&self, module: Module) -> Result<ValidatedModule, ValidationError> {
        validator::validate_module(&module)?;
        Ok(ValidatedModule {
            module,
            certificate: ValidationCertificate { _private: () },
        })
    }
}

/// Validate a decoded module.
pub fn validate(module: Module) -> Result<ValidatedModule, ValidationError> {
    Validator::new().validate(module)
}
