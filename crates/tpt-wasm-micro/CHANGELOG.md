# Changelog

All notable changes to `tpt-wasm-micro` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The Micro Interpreter (Golden Machine): `Machine`, `Step`, `Store`, `ExecutionConfig`,
  `ExecutionMode`, and `DeterministicRng` (`d1d0e56`).
- Reference instructions in the interpreter (`74bc07f`).
- Control-flow and numeric-operation behaviour checked by the core spec-test suite
  (`52d82a5`).
- `#![forbid(unsafe_code)]`, which enforces the zero-unsafe policy at compile time (`8cf57d9`).

### Fixed

- Interpreter behaviour corrected where the core spec suite found it disagreeing with the
  specification (`52d82a5`).
