# Resource Model

**Crate:** `tpt-wasm-resource`

---

Host resources are represented by opaque handles.

```rust
pub struct ResourceId(u64);
```

Rust pointers and object addresses are never exposed to WebAssembly programs.

```
Wasm
 │ ResourceId (u64)
 ▼
ResourceTable
 │
 ▼
Host Resource (any Rust type)
```

The `ResourceTable` maps opaque `ResourceId` handles to boxed host resources
using type-erased `Box<dyn Any>`. Downcasting is done on the host side.

Capability-backed runtime imports are authorized before they can reach this
resource boundary:

```text
Wasm → HostFunction → Host::invoke → CapabilitySet::authorize → Capability
```

A module reaches a capability only through an explicitly defined runtime
import; the host is checked on every invocation, so no ambient authority is
inferred from module structure or import names.

## Lifetime

Resources are created by the host (via `ResourceTable::insert`), passed to Wasm
as opaque handles, and removed by the host (via `ResourceTable::remove`).

Wasm programs cannot create, copy, or destroy resources directly. They hold handles.

## Security

- No pointer aliasing from Wasm
- No use-after-free from Wasm (handle validation on every access)
- No type confusion (downcast checked on host side)
- Wasm cannot address host memory directly
