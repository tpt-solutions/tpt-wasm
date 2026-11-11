# tpt-wasm-micro

The **Micro Interpreter**, the golden machine of
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

This is the canonical, minimal, executable definition of WebAssembly semantics in the
project. It is intentionally not optimized. It exists to be correct and easy to read.
Every other execution backend is judged against it: an optimization is accepted only if
the optimized module is indistinguishable from the one this interpreter runs, in values,
traps, memory effects, global effects, and the order of all of them.

> Pre-1.0. Applications normally reach this crate through the `Micro` engine mode of
> [`tpt-wasm`](https://docs.rs/tpt-wasm).

## Design invariants

- **No `unsafe` code.** The crate is marked `#![forbid(unsafe_code)]`, so the compiler
  enforces the rule.
- **Traps are values, not panics.** A WebAssembly trap is returned as a `Trap` through the
  normal control flow. A Rust panic in this crate is a bug.
- **Simple over fast.** The machine steps through instructions one at a time. This is what
  makes it readable as a specification.

## What it provides

- `Machine` — the interpreter state. `Machine::run` executes until it produces a `Step`.
- `Step` — the outcome of a run step: `Continue` (keep going), `Return` (the call finished
  with its results), `Trap` (a WebAssembly trap), or `HostCall` (a pending host function
  call that the embedder must service before resuming).
- `Store` — the module-level state: functions, instances, memories, tables, and globals.
- `ExecutionConfig` and `ExecutionMode` — step budgets and the determinism setting.
- `DeterministicRng` — the seeded random source used in deterministic mode.
- `instr` — the decoded instruction representation the machine executes.

The `execution` module holds the configuration and deterministic-mode types. `WASM_PAGE_SIZE`
is the page size used for linear memory.

## Where it fits

```
tpt-wasm-validate ──▶ tpt-wasm-micro ◀── tpt-wasm-runtime
                          ▲
                          │ (reference for every other backend)
          tpt-wasm-codegen / tpt-wasm-opt / tpt-wasm-oracle
```

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-validate`

## Testing

```sh
cargo test -p tpt-wasm-micro
cargo test -p tpt-wasm-spec
```

The core spec-test harness runs against this interpreter, and the differential oracle uses
it as the reference result.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
