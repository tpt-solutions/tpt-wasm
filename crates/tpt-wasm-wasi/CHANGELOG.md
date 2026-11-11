# Changelog

All notable changes to `tpt-wasm-wasi` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- `PREVIEW1_MODULE` and the `Preview1Function` labels for the common WASI Preview 1 imports
  (`d1d0e56`).
- `WasiAdapter`, which binds imports to capability operations on a shared `Host`, with
  `bind`, `bind_function`, `define`, `bindings`, and `binding` (`d1d0e56`).

### Notes

- This is a facade. It does not provide a complete WASI ABI, POSIX access, or implicit
  authority over any resource. Each binding is explicit and must name its capability.
