# Changelog

All notable changes to `tpt-wasm` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The public embedding API, re-exporting the decoder, validator, runtime, host, capability,
  resource, and WASI-adapter types. The facade contains no execution logic of its own
  (`d1d0e56`).
- Crate scaffold with the public API layout (`48cb533`).

### Notes

- Pre-1.0. The API may change between `0.x` releases; each release lists its breaking changes
  here.
