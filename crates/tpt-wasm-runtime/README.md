# tpt-wasm-runtime

The WebAssembly runtime for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm): the
`Store`, `Instance`, `Linker`, and `Engine` that embed a validated module and drive a chosen
backend.

It accepts a validated module, builds explicit store state, and runs calls through the
selected engine mode. It is the execution boundary between an embedder and the Micro
Interpreter, the baseline compiler, and the optimizing pipeline.

> Pre-1.0. Most users should use the [`tpt-wasm`](https://docs.rs/tpt-wasm) facade, which
> re-exports the types here.

## Main types

| Type | Role |
|---|---|
| `Engine` | Owns a `Config`, compiles or interprets modules, and creates instances |
| `Config` | Engine mode, features, resource limits, determinism, and debugging switches |
| `EngineMode` | `Micro` (default), `Baseline`, or `Optimizing` |
| `Features` | Feature switches for the MVP runtime, such as `reference_types` and `bulk_memory` |
| `Instance` | An instantiated module; `Instance::call` runs an exported function |
| `Linker` | Resolves imports to host functions and to instances |
| `Store` | Module-level state for memories, tables, and globals |
| `RuntimeError` / `StoreError` | Failures from instantiation, linking, and calls |

## Usage

```rust
use tpt_wasm_runtime::{Config, Engine, EngineMode};

let config = Config { engine_mode: EngineMode::Baseline, ..Config::default() };
let engine = Engine::new(config)?;
```

`Engine::instantiate_bytes` creates an `Instance` from raw bytes, and `Engine::instantiate`
creates one from a decoded `Module`. The `tpt-wasm` facade has a complete example.

## Features

- **Three engine modes** behind one API. The same module can run on each, and the results
  are required to match.
- **Debug sessions** (`DebugSession`, `StopReason`, `FrameInfo`). Breakpoints by function
  and program counter, and frame inspection. These run on the Micro engine, so they are
  refused for the compiled backends.
- **Compilation cache** (`CompilationCache`, `CompilationKey`, `CacheStats`). Keyed by a
  module digest, the engine mode, and the feature set, and reports hits and misses.
- **Execution provenance** (`Reproduction`). A record of a run that can be checked
  against the same engine configuration; the record is hashed with SHA-256.
- **Module digests.** The cache uses a 64-bit FNV-1a digest, which is for identification
  and is not a security boundary.
- **Deterministic execution.** `Config::deterministic` is on by default. When it is on, the
  engine runs with `ExecutionConfig::deterministic`, so time and randomness come from the
  deterministic sources rather than from the machine. Turn it off only when a module needs
  host time or randomness.

## Engine modes in detail

| Mode | Pipeline | Status |
|---|---|---|
| `Micro` | Decode → validate → Micro Interpreter | Complete for the implemented feature set |
| `Baseline` | Validate → IR → baseline compiler | Runs the implemented IR subset; native code emission pending |
| `Optimizing` | Validate → IR → optimization passes → baseline compiler | Runs the passes implemented in `tpt-wasm-opt` |

## Where it fits

```
tpt-wasm-host ─┐
tpt-wasm-wasi ─┼──▶ tpt-wasm-runtime ──▶ tpt-wasm-micro
               │          │
               │          ├──▶ tpt-wasm-ir ──▶ tpt-wasm-opt ──▶ tpt-wasm-codegen
               │          └──▶ tpt-wasm-decode / tpt-wasm-validate
```

`tpt-wasm-runtime` depends on the host-facing `tpt-wasm-capability` crate, so host
functions are granted by capability rather than by ambient access.

## Safety

No `unsafe` code in the current implementation.

## Testing

```sh
cargo test -p tpt-wasm-runtime
cargo test -p tpt-wasm-spec
```

The core spec suite runs through the runtime on both the Micro engine and the baseline
compiler.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
