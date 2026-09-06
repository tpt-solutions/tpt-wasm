// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! TPT-Wasm — public embedding API.
//!
//! ```rust
//! use tpt_wasm::{Engine, Config, Store, Module};
//!
//! let engine = Engine::new(Config::default()).unwrap();
//! // let module = Module::from_file(&engine, "program.wasm").unwrap();
//! // let mut store = Store::new(&engine, ());
//! // let instance = engine.instantiate(&mut store, &module).unwrap();
//! // let result = instance.get_function("main").unwrap().call(&mut store, &[]).unwrap();
//! ```
//!
//! The API is:
//!   - stable and versioned independently
//!   - independent of compiler internals
//!   - independent of the formal verification system
//!   - independent of TPT domain crates

pub use tpt_wasm_runtime::{Config, Engine, EngineMode};
pub use tpt_wasm_types::{Trap, Value, ValueType};
