# tpt-wasm-wasi

A capability-backed WASI adapter facade for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

This crate gives WASI Preview 1 imports a capability-backed binding. It does **not** provide
a complete WASI ABI, POSIX access, or implicit authority over the filesystem, clock, random
source, processes, or network. Each binding is explicit: the embedder chooses the capability
and operation behind every import, and supplies the function signature.

> Pre-1.0. Applications normally use this through the [`tpt-wasm`](https://docs.rs/tpt-wasm)
> facade.

## What it provides

- `PREVIEW1_MODULE` — the module name `wasi_snapshot_preview1`, as imports refer to it.
- `Preview1Function` — the common Preview 1 import names, as labels. A label does not grant
  anything.
- `WasiAdapter` — binds imports to a shared `Host` (`Arc<Mutex<Host>>`):
  - `bind(import, capability, operation, function_type)` attaches an import to a capability
    operation, with an explicit function type;
  - `bind_function` binds a Preview 1 function label;
  - `define` adds the bindings to a `Linker`;
  - `bindings` and `binding` report what has been bound.
- `WasiBinding`, `WasiAdapterError` — the bindings and the reasons a binding is refused.

## Example

```rust
use tpt_wasm_wasi::PREVIEW1_MODULE;

assert_eq!(PREVIEW1_MODULE, "wasi_snapshot_preview1");
```

## Design rules

- **Explicit, not ambient.** An import does nothing until the embedder binds it to a
  capability. An unbound import is not a silent no-op.
- **The embedder owns the signature.** The adapter does not infer types from WASI definitions.
- **Authority comes from the host.** Calls go through `Host::invoke`, so a binding cannot
  reach beyond what the host has granted.

## Status and limits

Only the labels and the binding facade are implemented. Full WASI Preview 1 semantics, and the
WASI Preview 2 / Component Model, are out of scope for this crate.

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-capability`
- `tpt-wasm-runtime`
- `tpt-wasm-host`

## Testing

```sh
cargo test -p tpt-wasm-wasi
```

## Safety

No `unsafe` code.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
