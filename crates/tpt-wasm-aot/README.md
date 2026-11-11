# tpt-wasm-aot

The ahead-of-time compiler for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

> **Status: not yet implemented.** This crate is a placeholder in the workspace. It is
> scheduled for milestone M8. Its target-code generator does not exist yet: it depends on the
> native code generation still pending in `tpt-wasm-codegen`.

## Planned design

The AOT compiler will translate a validated module into target code ahead of time, so that
the module does not need to be compiled when it is loaded. It will use the same pipeline as
the rest of the compiler:

```
Wasm → TPT IR → optimization → code generation → target artifact
```

Its output is held to the same contract as every other backend: the compiled module must be
indistinguishable from the Micro Interpreter's behaviour.

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

No `unsafe` code.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
