# tpt-wasm-resource

The host resource table and opaque resource handles for
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

WebAssembly code refers to host-owned objects through opaque `u64` handles. Rust pointers
and object addresses are never exposed to a module. The host stores each resource in a
`ResourceTable` and gives the module a `ResourceId` that names the entry.

```
Wasm → ResourceId → ResourceTable → Host Resource
```

> Pre-1.0. Applications normally use these types through the
> [`tpt-wasm`](https://docs.rs/tpt-wasm) facade.

## Main types

| Type | Role |
|---|---|
| `ResourceId` | An opaque handle. `as_u64` gives the value that crosses into WebAssembly |
| `ResourceTable` | Owns the resources. `insert` stores one and returns its id; `get`, `get_mut`, `remove`, `contains`, `len`, `is_empty` |

The table stores resources of any `Send + Sync + 'static` type. `get`, `get_mut`, and
`remove` take the type to retrieve as a type parameter, and return `None` if the stored
resource has a different type.

## Example

```rust
use tpt_wasm_resource::ResourceTable;

let mut table = ResourceTable::new();
let id = table.insert(String::from("config.toml"));
assert_eq!(table.get::<String>(id).map(String::as_str), Some("config.toml"));
assert_eq!(table.remove::<String>(id), Some(String::from("config.toml")));
assert!(!table.contains(id));
```

## Design rules

- A handle is a number issued by the table, not a reference. A module can hold a handle but
  cannot dereference it; access always goes through the table, which the host controls.
- No pointers cross the boundary.

## Relationship to other crates

- `tpt-wasm-host` uses a `ResourceTable` for the resources its capabilities hand out.
- `tpt-wasm-types` supplies the value types a handle is passed as.

## Safety

No `unsafe` code. The crate is marked `#![forbid(unsafe_code)]`.

## Testing

```sh
cargo test -p tpt-wasm-resource
```

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
