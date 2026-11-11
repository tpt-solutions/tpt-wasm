# tpt-wasm-jit

The JIT compiler for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

> **Status: not yet implemented.** This crate is a placeholder in the workspace. It is
> scheduled for milestone M8 and depends on the native code generation that is still
> pending in `tpt-wasm-codegen`.

## Planned design

The JIT will sit at the boundary between the safe semantic runtime and executable memory:

```
safe semantic runtime → code generation boundary → executable memory
```

Unsafe code is permitted only at the executable-memory boundary. Every `unsafe` block will be
required to document its safety invariant, and the workspace's unsafe audit
(`scripts/unsafe-count.sh`) will track it. This crate is expected to be the first place in the
workspace that contains `unsafe` code.

Use the `Baseline` or `Optimizing` engine modes of [`tpt-wasm`](https://docs.rs/tpt-wasm) for
execution today.

## Testing

The crate currently has no tests of its own.

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-ir`
- `tpt-wasm-opt`
- `tpt-wasm-codegen`

## Safety

No `unsafe` code yet.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
