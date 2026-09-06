// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly runtime: Store, Instance, Linker, and Engine.
//!
//! The Engine drives execution using a configurable EngineMode.
//! The public API is independent of compiler internals and TPT domain crates.

use tpt_wasm_types::ResourceLimits;

/// Which execution backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EngineMode {
    #[default]
    Micro,
    Baseline,
    Optimizing,
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub engine_mode: EngineMode,
    pub limits: ResourceLimits,
    pub deterministic: bool,
    pub debugging: bool,
    pub profiling: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            engine_mode: EngineMode::Micro,
            limits: ResourceLimits::default(),
            deterministic: false,
            debugging: false,
            profiling: false,
        }
    }
}

/// The WebAssembly execution engine.
pub struct Engine {
    #[allow(dead_code)]
    config: Config,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self, EngineError> {
        Ok(Self { config })
    }
}

#[derive(Debug)]
pub enum EngineError {
    Custom(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for EngineError {}
