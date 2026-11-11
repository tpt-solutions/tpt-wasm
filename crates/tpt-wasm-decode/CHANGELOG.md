# Changelog

All notable changes to `tpt-wasm-decode` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- Binary format decoder, `decode`, and encoder, `encode`, with `DecodeError` and `EncodeError`
  (`d1d0e56`).
- A spec test harness that runs the upstream binary-format suite against the decoder
  (`876b293`).
- Reading of all eight element-segment encodings (`0932bfb`).

### Fixed

- Four decoder bugs found by closing the binary-format spec suite (`95c728d`).
- Two decoder bugs found while building the first spec test harness (`876b293`).
- Two of the four remaining element-segment gaps, closed by reading the full set of encodings
  (`0932bfb`).
