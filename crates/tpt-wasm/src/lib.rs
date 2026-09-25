// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! TPT-Wasm — public embedding API.
//!
//! ```rust
//! use tpt_wasm::{Config, Engine};
//!
//! let engine = Engine::new(Config::default()).unwrap();
//! ```
//!
//! The API is:
//!   - stable and versioned independently
//!   - independent of compiler internals
//!   - independent of the formal verification system
//!   - independent of TPT domain crates

pub use tpt_wasm_capability::{
    Capability, CapabilityDescriptor, CapabilityError, CapabilityGrant, CapabilityId, Effect,
    Operation, Permission, Permissions,
};
pub use tpt_wasm_decode::{decode, encode, DecodeError, EncodeError, Module};
pub use tpt_wasm_host::{
    CapabilityHostFunction, ControlledFilesystem, DeterministicRng, DeterministicScheduler,
    EffectRecord, ExecutionMode, Host, HostError, NetworkPolicy, VirtualClock,
};
pub use tpt_wasm_resource::{ResourceId, ResourceTable};
pub use tpt_wasm_runtime::{
    Config, Engine, EngineMode, Features, Instance, Linker, RuntimeError, Store, StoreError,
};
pub use tpt_wasm_types::{Trap, Value, ValueType};
pub use tpt_wasm_validate::{
    validate, ValidatedModule, ValidationCertificate, ValidationError, Validator,
};
pub use tpt_wasm_wasi::{Preview1Function, WasiAdapter, PREVIEW1_MODULE};
