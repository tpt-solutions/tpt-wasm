# Changelog

All notable changes to `tpt-wasm-semantics` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The executable abstract-machine model: `Configuration`, `Store`, frames, control state,
  `Instruction`, `Transition`, and `step` (`d1d0e56`).
- The MVP memory instruction set in the model, with the machine-checked Lean 4 model it
  corresponds to (`722f2d4`).
- `check_invariants`, which checks the state invariants at run time.
