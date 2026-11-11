# tpt-wasm-oracle

A differential oracle over arbitrary WebAssembly bytes, for
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

The Micro Interpreter is the golden machine, so every other backend is judged against it.
For a byte string that decodes, validates, and imports nothing, the oracle:

1. instantiates the module on the Micro Interpreter, the baseline compiler, and the optimizing
   engine;
2. calls every function export with zero arguments; and
3. requires the same answers from all three.

Two results agree when the returned values are bit-identical, or when the traps are the same
variant.

> This crate is `publish = false`. It is development infrastructure for the workspace and the
> fuzzing targets in the repository's `fuzz/` directory.

## Scope

**In scope:** modules that decode, validate, and import nothing, with numeric exports.

**Out of scope, on purpose:**

- Bytes that do not decode or validate. Those are rejected by the decoder and validator, which
  are tested elsewhere; there is nothing to compare.
- Modules with imports. Their behaviour depends on host callbacks the oracle does not supply.
- Exports whose parameters are not numeric. A zero value is only defined for the four numeric
  types.
- A backend that refuses a module as `UnsupportedFeature`. That is a documented limit of the
  backend, not a disagreement about the module's meaning.

Anything else is a divergence, reported as an error string.

A panic is **not** caught on purpose. A fuzzer reports a panic as a crash. Catching it would
turn a crash into a quiet pass.

## Main items

| Item | Role |
|---|---|
| `check(bytes) -> Result<Verdict, String>` | Run the oracle and report what was compared |
| `differential_check(bytes) -> Result<(), String>` | Run the oracle; `Ok` means the backends agreed or the input was out of scope |
| `agree` | The equality rule for two results |
| `Verdict`, `CallOutcome` | What was compared, and the outcome of each call |
| `MEMORY_PAGES`, `STEP_BUDGET` | Bounds that keep every run finite |

`check` distinguishes "agreed" from "never run", so a caller can tell a real agreement from an
input the oracle skipped.

## Example

```rust
use tpt_wasm_oracle::differential_check;

let empty_module = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
assert!(differential_check(&empty_module).is_ok());
```

## Testing

```sh
cargo test -p tpt-wasm-oracle
```

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-format`
- `tpt-wasm-decode`
- `tpt-wasm-validate`
- `tpt-wasm-runtime`

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
