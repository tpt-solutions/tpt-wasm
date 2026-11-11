# Changelog

All notable changes to `tpt-wasm-capability` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- Capability types: `CapabilityId`, `Operation`, `Permission`, `Permissions`,
  `CapabilityDescriptor`, `CapabilityGrant`, and `Effect` (`d1d0e56`).
- The `CapabilitySet` checks: a capability must be registered before it can be granted, and
  an operation is authorized against both its descriptor and its grant (`d1d0e56`).
- The `Capability` trait, which a host implements for each capability (`d1d0e56`).
- `#![forbid(unsafe_code)]` (`8cf57d9`).
