// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly validation.
//!
//! Transforms a decoded Module into a ValidatedModule.
//!
//! Module → Validator → ValidatedModule
//!
//! The ValidationCertificate is an architectural hook for future
//! formal verification. It need not be a proof object initially.

use tpt_wasm_format::Module;

/// Errors produced during validation.
#[derive(Debug, Clone, PartialEq)]
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
    Custom(String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for ValidationError {}

/// A module that has passed validation.
///
/// Carries the original module plus a certificate that
/// the static properties required for execution hold.
#[derive(Debug, Clone)]
pub struct ValidatedModule {
    pub module: Module,
    pub certificate: ValidationCertificate,
}

/// Architectural hook for future formal proofs.
/// Currently an empty marker; will carry proof-relevant metadata in M5.
#[derive(Debug, Clone)]
pub struct ValidationCertificate {
    _private: (),
}

/// Validate a decoded module.
pub fn validate(module: Module) -> Result<ValidatedModule, ValidationError> {
    // TODO(M1): implement full validation per WebAssembly Core spec §3
    Ok(ValidatedModule {
        module,
        certificate: ValidationCertificate { _private: () },
    })
}
