# tpt-wasm-host

The capability-oriented host for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

Core principle: **no ambient authority.** A WebAssembly program may interact with host
resources only through capabilities the embedder has explicitly granted. The host does not
expose a filesystem, sockets, a clock, a random source, or POSIX by default.

> Pre-1.0. Most users should use the [`tpt-wasm`](https://docs.rs/tpt-wasm) facade, which
> re-exports the host types.

## What it provides

- **`Host`** — the registry of capabilities and their implementations. Grants are made with
  `Host::grant` and revoked with `Host::revoke`.
- **Capability-backed host functions** (`CapabilityHostFunction`). `Host::invoke` checks the
  grant for the requested operation first, and only then runs the implementation. Each call
  that runs is appended to the effect log as an `EffectRecord`.
- **Controlled filesystem** (`ControlledFilesystem`). An in-memory store that a module can
  reach only through an explicit grant. It does not touch the real filesystem.
- **Network policy** (`NetworkPolicy`). Network access is allowed only in `HostDependent`
  mode. `Host::check_network` returns `HostError::NetworkProhibited` in the other two modes.
- **Deterministic time** (`VirtualClock`). Ticks advance only when the embedder advances
  them, through `advance_virtual_time`.
- **Deterministic random** (`DeterministicRng`). A seeded generator, so the same seed
  produces the same stream.
- **Deterministic scheduling** (`DeterministicScheduler`). `pop` returns the task with the
  lowest priority number first. Tasks with equal priority run in the order they were
  scheduled. Scheduler access requires deterministic mode.
- **Execution modes** (`ExecutionMode`):
  - `HostDependent` — the default; host time, randomness, and network are allowed
  - `Deterministic` — virtual time, seeded random, and the scheduler; no network
  - `RecordReplay` — invocations return results from a log supplied with `install_replay`,
    in order, and never call the capability implementation

## Example

```rust
use tpt_wasm_host::{ExecutionMode, Host};

let host = Host::new_with_seed(ExecutionMode::Deterministic, 42);
assert_eq!(host.mode(), ExecutionMode::Deterministic);
assert!(host.check_network().is_err());
```

## Design rules

- Every operation goes through `Host::invoke`, which checks the grant before anything runs.
  A capability that has not been granted cannot be reached.
- Errors are `HostError` values. The host does not panic on bad input from a module.
- The effect log (`Host::effects`) is the audit trail of a run in which capabilities were
  called. In `RecordReplay` mode, a request that does not match the next recorded entry is
  refused with `HostError::ReplayMismatch`, and running past the end of the log is
  `HostError::ReplayExhausted`.

## Relationship to other crates

- `tpt-wasm-capability` defines the capability types and the grant model.
- `tpt-wasm-resource` provides the opaque handle table the host uses for resources.
- `tpt-wasm-wasi` adapts WASI Preview 1 imports onto host capabilities.
- `tpt-wasm-runtime` is where a `Host` is linked into an instance.

## Safety

No `unsafe` code. The crate is marked `#![forbid(unsafe_code)]`.

## Testing

```sh
cargo test -p tpt-wasm-host
```

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
