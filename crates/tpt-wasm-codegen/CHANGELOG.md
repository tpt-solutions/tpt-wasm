# Changelog

All notable changes to `tpt-wasm-codegen` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The baseline: lowering of IR to `BaselineFunction` and `BaselineModule`, and execution through
  a `HostBoundary` (`d1d0e56`, `8546b73`).
- Shared memory across instances (`5c64681`) and shared globals (`fce8067`).
- Table and global imports (`0481815`).
- A bounded call depth, `set_max_call_depth`, so runaway recursion traps rather than exhausting
  the host stack.
- An instruction budget, `set_max_execution_steps`, and per-call step reporting through
  `call_counted` (`afb7cc5`, `e23534e`).
- `TargetArch` naming the planned native targets. No native backend is implemented yet.

### Fixed

- Baseline bugs found by the core spec suite and the generated-program sweep (`c774db8`,
  `609de6a`, `4d2d5a8`).
- Baseline behaviour under runaway recursion, which previously exhausted the host stack
  (`c774db8`).
