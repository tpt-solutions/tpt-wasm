# tpt-wasm-cli

The command-line interface for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm). It
builds a binary named `tpt-wasm`.

> **Status: early.** The binary currently prints its version. The subcommands below are
> planned and are not yet implemented.

## Planned subcommands

| Command | Purpose |
|---|---|
| `tpt-wasm run` | Instantiate a module and call an export |
| `tpt-wasm validate` | Decode and validate a module, and report the first error |
| `tpt-wasm inspect` | Print a module's sections, imports, and exports |
| `tpt-wasm diff` | Run a module on two engine modes and report any difference |

## Building

The crate is part of the workspace and is not yet published:

```sh
cargo run -p tpt-wasm-cli
```

## Current behaviour

```console
$ tpt-wasm
tpt-wasm 0.1.0
```

Until the subcommands land, use the library through [`tpt-wasm`](https://docs.rs/tpt-wasm).

## Dependencies

- `tpt-wasm` (the embedding API)

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
