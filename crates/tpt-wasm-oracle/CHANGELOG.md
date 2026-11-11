# Changelog

All notable changes to `tpt-wasm-oracle` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The differential oracle: `differential_check` and `check`, which run a module on the Micro
  Interpreter, the baseline, and the optimizing engine, and require the same results
  (`d4dc3bb`).
- `Verdict` and `CallOutcome`, so a caller can tell an agreement from an input that was never
  run (`d4dc3bb`).
- A fuzz target that drives the oracle, in the repository's `fuzz/` directory (`d4dc3bb`).

### Notes

- `publish = false`. This crate is development infrastructure, not a published API.
