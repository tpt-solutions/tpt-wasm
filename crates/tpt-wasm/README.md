# tpt-wasm

The public embedding API for **TPT-Wasm**, an independent Rust implementation of
WebAssembly Core 3.0.

`tpt-wasm` is a facade. It re-exports the types an application needs to decode,
validate, instantiate, and call WebAssembly modules, and the capability types needed to
grant a module access to host resources. It contains no execution logic of its own.

> Pre-1.0. The API is stabilised per release and may change between `0.x` versions.
> See the [CHANGELOG](CHANGELOG.md) before upgrading.

## Why TPT-Wasm

- **One definition of meaning.** The Micro Interpreter is the golden machine. Every other
  execution mode is required to agree with it.
- **No ambient authority.** A module can reach the outside world only through capabilities
  that the embedder explicitly grants. There is no default filesystem, network, clock, or
  random source.
- **Deterministic when you ask for it.** Virtual time, a seeded RNG, a deterministic
  scheduler, and record/replay are available through the host.
- **No `unsafe` code.** The crates on the execution path currently contain no `unsafe`.

## Quick start

```rust
use tpt_wasm::{Config, Engine, Value};

// (module (func (export "add") (param i32 i32) (result i32)
//           local.get 0 local.get 1 i32.add))
let wasm: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
    0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // type section
    0x03, 0x02, 0x01, 0x00, // function section
    0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00, // export "add"
    0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b, // code
];

let engine = Engine::new(Config::default())?;
let mut instance = engine.instantiate_bytes(wasm)?;
let out = instance.call("add", vec![Value::I32(2), Value::I32(3)])?;
assert_eq!(out, vec![Value::I32(5)]);
```

## What it re-exports

| Area | Items |
|---|---|
| Decoding | `decode`, `encode`, `Module`, `DecodeError`, `EncodeError` |
| Validation | `validate`, `Validator`, `ValidatedModule`, `ValidationCertificate`, `ValidationError` |
| Runtime | `Engine`, `Config`, `EngineMode`, `Features`, `Instance`, `Linker`, `Store`, `RuntimeError`, `StoreError` |
| Values | `Value`, `ValueType`, `Trap` |
| Host | `Host`, `ExecutionMode`, `NetworkPolicy`, `HostError`, `EffectRecord`, `DeterministicRng`, `DeterministicScheduler`, `VirtualClock`, `ControlledFilesystem`, `CapabilityHostFunction` |
| Capabilities | `Capability`, `CapabilityId`, `CapabilityGrant`, `CapabilityDescriptor`, `CapabilityError`, `Operation`, `Permission`, `Permissions`, `Effect` |
| Resources | `ResourceId`, `ResourceTable` |
| WASI adapter | `WasiAdapter`, `Preview1Function`, `PREVIEW1_MODULE` |

### Engine modes

`Config::engine_mode` selects the backend. The default is `EngineMode::Micro`.

| Mode | Backend | Use it for |
|---|---|---|
| `Micro` | Micro Interpreter | Reference behaviour, debugging, and debug sessions |
| `Baseline` | Baseline compiler | Predictable execution and differential comparison |
| `Optimizing` | Optimizing pipeline | Running with the optimization passes enabled |

Every mode is required to produce the same observable results. The differential oracle
(`tpt-wasm-oracle`) exists to check that requirement.

## Feature status

- MVP core: binary decoding, validation, instantiation, and calls are implemented.
- Core spec-test suite: the vendored upstream suite runs on the execution backends.
- WASI: only explicit Preview 1 import labels and an adapter facade. This is **not** a
  complete WASI ABI.
- Native code generation: pending. The baseline and optimizing backends run through the
  IR and do not yet emit machine code.

## Crate layout

`tpt-wasm` sits at the top of the dependency graph. The implementation lives in these
crates:

- [`tpt-wasm-types`](https://docs.rs/tpt-wasm-types), [`tpt-wasm-format`](https://docs.rs/tpt-wasm-format)
- [`tpt-wasm-decode`](https://docs.rs/tpt-wasm-decode), [`tpt-wasm-validate`](https://docs.rs/tpt-wasm-validate)
- [`tpt-wasm-runtime`](https://docs.rs/tpt-wasm-runtime), [`tpt-wasm-micro`](https://docs.rs/tpt-wasm-micro)
- [`tpt-wasm-host`](https://docs.rs/tpt-wasm-host), [`tpt-wasm-capability`](https://docs.rs/tpt-wasm-capability), [`tpt-wasm-resource`](https://docs.rs/tpt-wasm-resource), [`tpt-wasm-wasi`](https://docs.rs/tpt-wasm-wasi)

The embedding API does not depend on compiler internals or on the formal verification
crates.

## Safety

No `unsafe` code. The workspace policy is enforced by `scripts/unsafe-count.sh` in CI.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/tpt-solutions/tpt-wasm/blob/master/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/tpt-solutions/tpt-wasm/blob/master/LICENSE-MIT))

at your option. Copyright (c) 2026 TPT Solutions.

Contributions are managed through GitHub Issues; see
[CONTRIBUTING.md](https://github.com/tpt-solutions/tpt-wasm/blob/master/CONTRIBUTING.md).
