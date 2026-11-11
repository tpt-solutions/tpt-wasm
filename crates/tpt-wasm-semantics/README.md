# tpt-wasm-semantics

An executable Rust model of the WebAssembly abstract machine, for
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

The crate models the Core specification's configuration and transition boundary: a
`Configuration` holds the store, the frame stack, and the operand stack, and `step` moves the
configuration forward by one transition. It is written to be read against the specification,
not to run programs quickly.

It deliberately does **not** depend on the Micro Interpreter or on compiler internals.
Correspondence between this model and the other implementations is checked separately by
`tpt-wasm-verify`, and by the Lean 4 development in the repository's `formal/` directory.

> Pre-1.0. This is a verification artifact. For running modules, use
> [`tpt-wasm`](https://docs.rs/tpt-wasm).

## Main types

| Type | Role |
|---|---|
| `Configuration` | The machine state: the store, the frame stack, and the operand stack |
| `Store` | Functions, globals, memories, and tables as the model sees them |
| `Frame`, `ControlFrame`, `FunctionState` | Call frames and structured-control state |
| `GlobalState`, `MemoryState`, `TableState` | The state of each store entity |
| `Instruction` | The instructions the model steps through |
| `Transition` | The result of one step |
| `ModelError` | A transition the model refuses |

Helpers cover the stack discipline (`push_value`, `pop_value`, `pop_i`, `pop_f`, `pop_typed`,
`push_control`, `pop_control`, `push_frame`, `pop_frame`) and the invariant check
`check_invariants`.

## Relationship to the proofs

- `check_invariants` checks the state invariants at run time.
- The `formal/` directory holds the Lean 4 model and its proofs.
- `tpt-wasm-verify` projects IR functions into this model so that the IR can be checked
  against it.

## Safety

No `unsafe` code.

## Testing

```sh
cargo test -p tpt-wasm-semantics
```

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
