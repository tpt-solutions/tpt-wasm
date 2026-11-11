# tpt-wasm-types

Semantic value and type definitions for WebAssembly, used by every other crate in
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

This crate is the shared vocabulary. It defines what a WebAssembly value, type, limit, and
trap *is*. It has no dependencies on other tpt-wasm crates, and it does not depend on any
machine-register representation.

> Pre-1.0. Part of the TPT-Wasm workspace; use the [`tpt-wasm`](https://docs.rs/tpt-wasm)
> facade unless you are building a lower-level tool.

## What it provides

- **Values** — `Value` covers `i32`, `i64`, `f32`, `f64`, `v128`, and references.
  Floating-point values are stored as raw bits, so NaN payloads survive round trips.
- **Value and reference types** — `ValueType`, `ReferenceType`, `RefType`, and `RefValue`.
- **Composite types** — `FunctionType`, `ResultType`, `MemoryType`, `TableType`,
  `GlobalType`, `TagType`, and `Limits`.
- **Traps** — `Trap` names every WebAssembly trap condition as a value. Traps are data,
  never Rust panics.
- **Resource limits** — `ResourceLimits` describes the bounds an embedder can impose on a
  module.
- **Classification** — `ValueType::is_mvp_numeric` and `ValueType::is_supported` report
  whether a type belongs to the MVP numeric set and whether this implementation supports
  it.

## Example

```rust
use tpt_wasm_types::{Value, ValueType};

let v = Value::I32(7);
assert!(ValueType::I32.is_mvp_numeric());
```

## Design rules

- The crate stays independent of the Micro Interpreter, the compilers, and the host.
- Types here are the contract the rest of the workspace is checked against, so changes
  here ripple outward. Treat a change to a public variant as a breaking change.

## Safety

No `unsafe` code.

## Testing

```sh
cargo test -p tpt-wasm-types
```

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
