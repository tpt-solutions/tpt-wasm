# Changelog

All notable changes to `tpt-wasm-verify` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The verification ladder, `VerificationLevel`, from V0 (type invariants) to V6 (machine-code
  refinement) (`d1d0e56`).
- The V4 correspondence, with IR projection into the formal model through
  `project_function_to_model` and `IrProjectionError` (`94eef19`).
- The V6 lowering skeleton, recording which links to machine code are still missing
  (`48b24ef`).
- The V5 correspondence checks for the optimizer: the per-step and per-run facts, and the
  dead-value-elimination proof (`4348aab`, `ea39052`, `bf79190`).
- A record of a model bug that the dead-value-elimination proof exposed (`bf79190`).

### Notes

- The V0 checker and the initial V4 projection are executable Rust. The full V1–V4 properties
  and the V6 machine-code refinement are proof obligations that are not yet discharged.
