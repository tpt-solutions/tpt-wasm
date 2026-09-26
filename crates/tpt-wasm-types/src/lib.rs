// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Semantic value and type definitions for WebAssembly.
//!
//! This crate contains the core types used throughout tpt-wasm.
//! It has no dependencies on other tpt-wasm crates and must remain
//! independent of machine-register representations.

/// A WebAssembly runtime value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    I32(i32),
    I64(i64),
    /// F32 stored as raw bits (preserves NaN payloads).
    F32(u32),
    /// F64 stored as raw bits (preserves NaN payloads).
    F64(u64),
    V128(u128),
    Ref(RefValue),
}

/// A WebAssembly reference value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefValue {
    Null(ReferenceType),
    FuncRef(u32),
    ExternRef(u64),
}

/// A WebAssembly value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    I32,
    I64,
    F32,
    F64,
    V128,
    Ref(ReferenceType),
}

/// A WebAssembly reference type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceType {
    FuncRef,
    ExternRef,
}

/// Backwards-compatible short name used by the structural format.
pub type RefType = ReferenceType;

/// The result type of a WebAssembly block or function (a sequence of value types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultType(pub Vec<ValueType>);

/// A WebAssembly function type (signature).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: ResultType,
    pub results: ResultType,
}

/// A WebAssembly global type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalType {
    pub value_type: ValueType,
    pub mutable: bool,
}

/// A WebAssembly memory type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryType {
    pub limits: Limits,
    /// True for 64-bit (memory64 proposal), false for 32-bit (default).
    pub memory64: bool,
}

/// A WebAssembly table type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableType {
    pub element_type: ReferenceType,
    pub limits: Limits,
}

/// A WebAssembly tag type (exception handling proposal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagType {
    pub function_type: FunctionType,
}

/// A WebAssembly limit (min/optional max).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub min: u64,
    pub max: Option<u64>,
}

/// Implementation resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_memory_pages: u64,
    pub max_table_elements: u32,
    pub max_instances: u32,
    pub max_stack_depth: usize,
    pub max_call_depth: usize,
    pub max_execution_steps: Option<u64>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_pages: 65536, // 4 GiB
            max_table_elements: 10_000_000,
            max_instances: 10_000,
            max_stack_depth: 65536,
            max_call_depth: 1000,
            max_execution_steps: None,
        }
    }
}

/// A WebAssembly trap - never represented as a Rust panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    Unreachable,
    IntegerDivisionByZero,
    IntegerOverflow,
    InvalidConversion,
    MemoryOutOfBounds,
    TableOutOfBounds,
    NullReference,
    IndirectCallTypeMismatch,
    StackOverflow,
    CallDepthExceeded,
    StepsExhausted,
    HostFailure(String),
}

impl std::fmt::Display for Trap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Trap {}

impl ValueType {
    /// Returns whether this is one of the four numeric types in the WebAssembly MVP.
    pub fn is_mvp_numeric(self) -> bool {
        matches!(self, Self::I32 | Self::I64 | Self::F32 | Self::F64)
    }

    /// Returns whether this is a value type the validator accepts.
    ///
    /// The four MVP numeric types, plus the two reference types. `ref.null`,
    /// `ref.func`, and `ref.is_null` are implemented, so a reference has to be
    /// storable in a local, a global, and a signature for those instructions to
    /// be usable at all. `V128` stays out: the vector proposal is not
    /// implemented, and admitting the type without the instructions that move it
    /// would let a module declare a value nothing can produce.
    pub fn is_supported(self) -> bool {
        self.is_mvp_numeric() || matches!(self, Self::Ref(_))
    }
}

#[cfg(test)]
mod tests {
    use super::{RefValue, ReferenceType, Value, ValueType};

    #[test]
    fn raw_floating_point_values_preserve_nan_bits() {
        let nan = 0x7fc0_1234;
        assert_eq!(Value::F32(nan), Value::F32(nan));
        assert_ne!(Value::F32(nan), Value::F32(0x7fc0_5678));
    }

    #[test]
    fn null_references_retain_their_type() {
        assert_eq!(
            Value::Ref(RefValue::Null(ReferenceType::ExternRef)),
            Value::Ref(RefValue::Null(ReferenceType::ExternRef))
        );
        assert_ne!(
            Value::Ref(RefValue::Null(ReferenceType::FuncRef)),
            Value::Ref(RefValue::Null(ReferenceType::ExternRef))
        );
    }

    #[test]
    fn mvp_numeric_types_exclude_vectors_and_references() {
        assert!(ValueType::I32.is_mvp_numeric());
        assert!(ValueType::F64.is_mvp_numeric());
        assert!(!ValueType::V128.is_mvp_numeric());
        assert!(!ValueType::Ref(ReferenceType::FuncRef).is_mvp_numeric());
    }
}
