# Changelog

All notable changes to `tpt-wasm-format` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- Structural module representation: `Module`, `Function`, `LocalDecl`, `Import`, `ImportDesc`,
  `Export`, `ExportDesc`, `Global`, `Table`, `Memory`, `Tag`, `ConstExpr`, and `CustomSection`
  (`d1d0e56`).
- Element and data segment representations with their modes: `Element`, `ElementMode`,
  `ElementInit`, `DataSegment`, and `DataMode`. The element representation covers all eight
  encodings of the binary format (`0932bfb`).

### Notes

- This is the structural view, not an execution form. It is produced by the decoder and
  consumed by the validator.
