// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Semantic value and type definitions for WebAssembly.
//!
//! This crate contains the core types used throughout tpt-wasm.
//! It has no dependencies on other tpt-wasm crates and must remain
//! independent of machine-register representations.

/// A WebAssembly runtime value.
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub enum RefValue {
    Null(RefType),
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
    Ref(RefType),
}

/// A WebAssembly reference type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefType {
    FuncRef,
    ExternRef,
}

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
    pub element_type: RefType,
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
#[derive(Debug, Clone, Copy)]
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
            max_memory_pages: 65536,     // 4 GiB
            max_table_elements: 10_000_000,
            max_instances: 10_000,
            max_stack_depth: 65536,
            max_call_depth: 1000,
            max_execution_steps: None,
        }
    }
}

/// A WebAssembly trap — never represented as a Rust panic.
#[derive(Debug, Clone, PartialEq)]
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
