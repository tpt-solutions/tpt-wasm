# Changelog

All notable changes to `tpt-wasm-spec` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- A harness for the upstream binary-format test suite, handling `(module binary ...)`,
  `assert_malformed`, and `assert_invalid` (`876b293`).
- The vendored binary-format suite, run to completion (`95c728d`).
- The vendored core memory suite, with its directives run on both execution backends
  (`dbf1205`). Eleven core test files are vendored, with `testdata/PROVENANCE.md` recording
  their revision (`609de6a`).
- Core control-flow and numeric-operation suite (`52d82a5`).
- Linking of instances through the `register` directive (`f15df05`).
- A generated-program differential harness that compares the backends (`e598072`).
