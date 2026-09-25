// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Explicit execution policies for the Micro interpreter.

/// Controls how external effects are expected to be sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// Reproducible execution using only explicit machine state.
    #[default]
    Deterministic,
    /// The embedding layer may provide effects outside the interpreter.
    HostDependent,
}

/// Configuration carried by a machine instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionConfig {
    pub mode: ExecutionMode,
    pub seed: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Deterministic,
            seed: 0,
        }
    }
}

impl ExecutionConfig {
    pub const fn deterministic(seed: u64) -> Self {
        Self {
            mode: ExecutionMode::Deterministic,
            seed,
        }
    }

    pub const fn host_dependent() -> Self {
        Self {
            mode: ExecutionMode::HostDependent,
            seed: 0,
        }
    }
}

/// A small, reproducible pseudo-random stream for embedding effects.
///
/// This is deliberately not used by core WebAssembly instructions. It is
/// available to a future host adapter that needs a reproducible effect source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }
}

#[cfg(test)]
mod tests {
    use super::{DeterministicRng, ExecutionConfig, ExecutionMode};

    #[test]
    fn same_seed_produces_the_same_stream() {
        let mut left = DeterministicRng::new(7);
        let mut right = DeterministicRng::new(7);
        let left_values = (0..8).map(|_| left.next_u64()).collect::<Vec<_>>();
        let right_values = (0..8).map(|_| right.next_u64()).collect::<Vec<_>>();
        assert_eq!(left_values, right_values);
    }

    #[test]
    fn different_seeds_produce_different_streams() {
        let mut left = DeterministicRng::new(7);
        let mut right = DeterministicRng::new(8);
        assert_ne!(left.next_u64(), right.next_u64());
    }

    #[test]
    fn default_policy_is_deterministic() {
        let config = ExecutionConfig::default();
        assert_eq!(config.mode, ExecutionMode::Deterministic);
        assert_eq!(config.seed, 0);
    }
}
