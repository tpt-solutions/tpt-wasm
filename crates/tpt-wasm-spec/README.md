# tpt-wasm-spec

The harness that runs the official WebAssembly specification tests against
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

This crate is test infrastructure. It has no role in running programs.

The `.wast` files in `testdata/` are vendored from the upstream specification repository.
`testdata/PROVENANCE.md` records the exact revision and licence. Nothing is fetched at test
time, so a run is reproducible and works offline.

> Not intended as a public API. It is published with the workspace so the harness can be
> reused, but its interface may change at any time.

## What it checks

- **Binary-format suite.** `(module binary ...)` forms must decode and validate.
  `assert_malformed` cases must fail to decode. `assert_invalid` cases must decode and then
  fail validation. The `reason` strings are not matched against this implementation's error
  text, so a test cannot pass by agreeing with our own wording.
- **Core suite.** The vendored core test files run through the decoder, validator, and
  runtime on each execution backend. `run_core_suite` uses the default backend and
  `run_core_suite_with` chooses one.

A directive the harness does not interpret becomes a skipped case rather than an error, so a
file that mixes binary and text modules still reports the parts that are covered.

## Main items

| Item | Role |
|---|---|
| `parse`, `Form`, `ParseError` | Parse a `.wast` file into directives |
| `cases` , `Case`, `Expectation`, `Outcome` | Turn binary-format directives into checkable cases |
| `run_core_suite`, `run_core_suite_with` | Run the core suite on a chosen backend |
| `CoreCase`, `CoreOutcome` | The result of each core-suite directive |

## Running

```sh
cargo test -p tpt-wasm-spec
```

## Dependencies

- `tpt-wasm-types`
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
The vendored upstream test files keep their own licence; see `testdata/PROVENANCE.md`.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
