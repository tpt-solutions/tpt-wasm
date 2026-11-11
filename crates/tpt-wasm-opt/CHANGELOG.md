# Changelog

All notable changes to `tpt-wasm-opt` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The optimizing pipeline, `optimize`, with `OptimizationLevel::None`, `Fast`, and `Full`
  (`f6c5a21`).
- The optimization contract, the observable-behaviour rules every pass must keep, in the
  `contract` module (`f6c5a21`).
- Constant folding and propagation, control-flow simplification, dead-code elimination, local
  and global value numbering, load/store optimization, bounds-check elimination, and
  instruction scheduling in `Fast` (`f6c5a21`).
- Register allocation in `Full`, scheduled last (`f6c5a21`).
- The pipeline runs to a fixed point, with a backstop on the number of rounds (`f6c5a21`).
- `COMPILER_VERSION`, which is part of the compilation cache key, and per-pass tests that
  run each pass on its own (`7c44d0e`).
