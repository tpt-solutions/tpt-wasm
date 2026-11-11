# Changelog

All notable changes to `tpt-wasm-resource` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- `ResourceTable` with typed `insert`, `get`, `get_mut`, and `remove`, and `ResourceId`
  opaque handles (`d1d0e56`).
- `#![forbid(unsafe_code)]` (`8cf57d9`).
