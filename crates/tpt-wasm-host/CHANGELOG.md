# Changelog

All notable changes to `tpt-wasm-host` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0; before that, breaking
changes can appear in any `0.x` release.

## [Unreleased]

### Added

- The capability-oriented `Host`, with `grant`, `revoke`, `invoke`, and an effect log
  (`d1d0e56`).
- `ExecutionMode` with `HostDependent`, `Deterministic`, and `RecordReplay`, and the
  `NetworkPolicy` that goes with each (`d1d0e56`).
- Deterministic time (`VirtualClock`), seeded random (`DeterministicRng`), and a deterministic
  scheduler (`DeterministicScheduler`) (`d1d0e56`).
- A controlled in-memory filesystem (`ControlledFilesystem`) that is reachable only through a
  grant (`d1d0e56`).
- Record and replay of host effects, with `install_replay` (`d1d0e56`).
- A stub `tpt_system_adapter` module, which reports that the integration is not available
  (`48cb533`).
- `#![forbid(unsafe_code)]` (`8cf57d9`).
