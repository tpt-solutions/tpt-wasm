# Changelog

All notable changes to `tpt-wasm-validate` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- Validation pass over `Module`, with `validate`, `Validator`, `ValidatedModule`,
  `ValidationCertificate`, and `ValidationError` (`d1d0e56`).
- Validation of reference types and reference instructions (`74bc07f`).

### Fixed

- A `br_if` typing bug found by the generated-program sweep (`6a031c8`).
- Validation bugs found by the core control-flow and numeric-op spec suite (`52d82a5`).
- Element-segment validation gaps closed by reading all eight encodings (`0932bfb`).
