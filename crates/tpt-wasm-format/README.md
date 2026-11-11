# tpt-wasm-format

The structural representation of a decoded WebAssembly module, for
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

`Module` is the structural view that the decoder produces and the validator consumes. It
mirrors the sections of the binary format: types, imports, functions, tables, memories,
globals, exports, start, elements, data, and custom sections.

It is **not** an execution form. Nothing here runs, and nothing here is optimized. For an
executable representation, see `tpt-wasm-micro` (the reference interpreter) and
`tpt-wasm-ir` (the compiler IR).

> Pre-1.0. Most users should use the [`tpt-wasm`](https://docs.rs/tpt-wasm) facade.

## Main types

| Type | Describes |
|---|---|
| `Module` | A whole module: the collection of every section |
| `Function`, `LocalDecl` | Function bodies and their local declarations |
| `Import`, `ImportDesc` | Imports and their kinds |
| `Export`, `ExportDesc` | Exports and their kinds |
| `Global`, `Table`, `Memory`, `Tag` | Module-level definitions |
| `Element`, `ElementMode`, `ElementInit` | Element segments in each of their modes |
| `DataSegment`, `DataMode` | Data segments in each of their modes |
| `ConstExpr` | Constant expressions used in initializers |
| `CustomSection` | Custom sections, kept verbatim |

The contents are plain data, so a module can be inspected or constructed directly.

## Where it fits

```
bytes ──decode──▶ Module (tpt-wasm-format) ──validate──▶ ValidatedModule
```

- `tpt-wasm-decode` produces `Module` and writes it back out with `encode`.
- `tpt-wasm-validate` consumes `Module` and does not decode or execute anything.

Keeping the structural view separate means that a decoded module can still be rejected by
validation, and validation never has to parse bytes.

## Dependencies

- `tpt-wasm-types`

## Safety

No `unsafe` code.

## Testing

```sh
cargo test -p tpt-wasm-format
```

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
