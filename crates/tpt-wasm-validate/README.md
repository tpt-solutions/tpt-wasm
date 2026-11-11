# tpt-wasm-validate

The WebAssembly validation pass for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

Validation takes the structural `Module` produced by the decoder and checks it against the
typing rules of WebAssembly Core. A module that validates is safe to instantiate. A module
that does not validate is rejected before anything runs.

Validation performs no decoding and does not instantiate or execute the module.

> Pre-1.0. Most users should use the [`tpt-wasm`](https://docs.rs/tpt-wasm) facade, which
> re-exports `validate`.

## Usage

Pass a decoded `Module` to `validate`. The `tpt-wasm` facade shows the full path from bytes:

```rust
use tpt_wasm_validate::validate;

fn check(module: tpt_wasm_format::Module) -> Result<(), tpt_wasm_validate::ValidationError> {
    let _validated = validate(module)?;
    Ok(())
}
```

## Main types

| Type | Role |
|---|---|
| `validate(Module)` | Validates a module (free function) |
| `Validator` | The validator object, with `Validator::validate` as its method |
| `ValidatedModule` | The result of successful validation; the input the IR lowering accepts |
| `ValidationCertificate` | A record of what was checked |
| `ValidationError` | Why a module was rejected |

`ValidatedModule` is the boundary for the rest of the pipeline. Code that needs a validated
module asks for this type, so an unvalidated module cannot be passed to the compilers.

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-format`

## Safety

No `unsafe` code.

## Testing

```sh
cargo test -p tpt-wasm-validate
```

The crate is also exercised by the upstream test suites through `tpt-wasm-spec`, including
the `assert_invalid` cases.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
