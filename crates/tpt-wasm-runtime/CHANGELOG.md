# Changelog

All notable changes to `tpt-wasm-runtime` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- `Engine`, `Config`, `Features`, `Instance`, `Linker`, `Store`, `RuntimeError`, and
  `StoreError`, forming the MVP execution boundary (`d1d0e56`).
- `EngineMode::Baseline`, which runs through the IR and the baseline compiler (`c56fc3e`).
- Table and global imports on both the Micro and baseline backends (`0481815`).
- Linking of instances through the `register` directive (`f15df05`).
- A memory shared across instances on the baseline backend (`5c64681`) and shared globals
  (`fce8067`).
- An instruction budget enforced on the baseline backend as well as the Micro engine
  (`afb7cc5`).
- `EngineMode::Optimizing`, which runs the optimization passes before the baseline (`f6c5a21`).
- A compilation cache with `CompilationCache`, `CompilationKey`, and `CacheStats`, keyed by a
  module digest, the engine mode, and the feature set (`4e8ce90`).
- Execution provenance: a recorded run can be reproduced and checked against the same engine
  configuration. Records are hashed with SHA-256 (`4e8ce90`).
- Debug sessions on the Micro engine, with breakpoints by function and program counter, and
  frame inspection through `DebugSession`, `StopReason`, and `FrameInfo` (`eeee246`).

### Fixed

- Two import bugs found by linking instances through the `register` directive (`f15df05`).
- Shared-state bugs in the baseline found by the core spec suite (`c774db8`, `609de6a`,
  `fce8067`).

### Changed

- `Config::deterministic` is on by default, so execution uses deterministic time and
  randomness unless it is turned off.
