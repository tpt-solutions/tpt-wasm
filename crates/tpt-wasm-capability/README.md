# tpt-wasm-capability

Capability types and the `Capability` trait for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

The host has no ambient authority. Every interaction between a WebAssembly instance and the
host goes through an explicit capability grant. This crate defines the vocabulary for those
grants: what a capability is, which operations it supports, what permission an operation
needs, and which grants an instance holds.

The rule the model enforces is

```
Authority(instance) ⊆ GrantedCapabilities(instance)
```

so an operation runs only if a grant covers it.

> Pre-1.0. Applications normally use these types through the
> [`tpt-wasm`](https://docs.rs/tpt-wasm) facade.

## Main types

| Type | Role |
|---|---|
| `CapabilityId` | An identifier for a capability |
| `Operation` | A named operation on a capability, with the `Permission` it needs |
| `Permission` | `Read`, `Write`, or `Execute` |
| `Permissions` | The set of permissions a capability allows |
| `CapabilityDescriptor` | What a capability is and which operations it supports |
| `CapabilityGrant` | A grant of one capability to an instance |
| `CapabilitySet` | The registered capabilities and the grants held; see below |
| `Effect` | The observable effect of an operation, recorded for audit and replay |
| `Capability` (trait) | The implementation a host provides for a capability |
| `CapabilityError`, `HostError` | Why a registration, grant, or call was refused |

## Example

```rust
use tpt_wasm_capability::{Operation, Permission};

let read = Operation::read("file.read");
assert_eq!(read.permission(), Permission::Read);
```

## How a grant is checked

`CapabilitySet` enforces the order of operations:

1. `register` adds a `CapabilityDescriptor`. A second registration of the same id is refused
   with `DuplicateCapability`.
2. `grant` attaches a `CapabilityGrant`. A grant for an id that was never registered is
   refused with `UnknownCapability`.
3. `authorize` checks an operation against the descriptor and the grant before it runs.
4. `revoke` removes the grant. Later calls are refused, because the grant is gone.

## Relationship to other crates

- `tpt-wasm-host` owns the capability set for a running host and checks it in `Host::invoke`.
- `tpt-wasm-wasi` binds WASI imports to capability operations.
- `tpt-wasm-types` supplies the `Value` type that operations take and return.

## Safety

No `unsafe` code. The crate is marked `#![forbid(unsafe_code)]`.

## Testing

```sh
cargo test -p tpt-wasm-capability
```

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
