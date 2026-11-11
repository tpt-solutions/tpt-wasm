# Changelog

All notable changes to `tpt-wasm-types` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- Semantic value and type definitions: `Value`, `ValueType`, `FunctionType`, `ResultType`,
  `Limits`, `MemoryType`, `TableType`, `GlobalType`, `TagType`, and `Trap` (`d1d0e56`).
- Reference types and reference values, `ReferenceType` and `RefValue`, used by the validator,
  the IR, and the baseline (`74bc07f`).
- `ResourceLimits` for embedder-imposed bounds on a module.
- `ValueType::is_mvp_numeric` and `ValueType::is_supported` for classifying types.

### Notes

- Floating-point values are stored as raw bits, so NaN payloads are preserved.
- The crate has no dependencies on other tpt-wasm crates and must not depend on
  machine-register representations.
