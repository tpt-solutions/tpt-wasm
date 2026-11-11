# Changelog

All notable changes to `tpt-wasm-ir` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The TPT IR for straight-line code with stable local slots and direct calls, and the
  `LoweringError` that refuses anything the IR does not represent (`d1d0e56`).
- Multi-block IR with dominance verification (`bb9c476`).
- Lowering of structured control flow, memory, globals, calls, and tables (`b3216ac`).
- Reference instructions in the IR (`74bc07f`).
- Table and global imports (`0481815`).
- `lower_and_verify`, which returns a `VerifiedIrModule` holding the module and its
  certificate.
- `IrModule`, `IrFunction`, `BasicBlock`, `IrInstr`, and `Terminator` as the public IR types.

### Fixed

- Lowering bugs found by the generated-program sweep (`6a031c8`, `4d2d5a8`) and by the core
  spec suite (`c774db8`).
