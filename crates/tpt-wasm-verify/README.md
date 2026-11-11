# tpt-wasm-verify

The verification infrastructure for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm):
invariants, contracts, and checks that connect the implementation to the formal model.

Verification is organised as a ladder. Each level says what a claim proves, and the crate
records which levels are checked by executable code and which are still proof obligations.

| Level | Claim |
|---|---|
| `V0TypeInvariants` | Type and configuration invariants hold |
| `V1InterpreterCorrespondence` | The Micro Interpreter agrees with the model |
| `V2MemorySafety` | Memory accesses stay within bounds and trap correctly |
| `V3HostCapabilitySafety` | Host effects happen only under a grant |
| `V4IrRefinement` | The IR refines the model |
| `V5CompilerTransformations` | Each compiler transformation preserves behaviour |
| `V6MachineCodeRefinement` | Generated machine code refines the IR |

`VerificationLevel` names these levels in code.

> Status: the V0 checker and the initial V4 IR projection are executable Rust. The proof
> assistant work and the full V1–V4 properties are pending. V5 covers the IR transformations
> in `tpt-wasm-opt`. V6 needs native code generation, which is not yet implemented.

## Main items

- `project_function_to_model` — projects an IR function into the `tpt-wasm-semantics` model,
  so it can be checked against the model's transitions.
- `IrProjectionError` — why a projection was refused.

## Testing

```sh
cargo test -p tpt-wasm-verify
```

The Lean 4 proofs are kept in the repository's `formal/` directory and are not part of the
Cargo build.

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-semantics`
- `tpt-wasm-ir`

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
