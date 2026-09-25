# WASI

**Status:** Dedicated adapter facade implemented; complete ABI planned (M10)
**Specification:** https://wasi.dev/

WASI is an adapter layer, not the fundamental host abstraction.

```
tpt-wasm-host
      │
  ┌───┼───────────┐
  ▼   ▼           ▼
WASI  TPT Host  Custom Host
```

The WASI interface is implemented as a capability adapter:

```
WASI import → TPT Host capability system
```

rather than:

```
WASI import → POSIX
```

This allows the runtime to support both standard WASI programs and
TPT-native capabilities without making WASI the architectural center.

WASI must not dictate the architecture of the core runtime.

## Current adapter skeleton

`tpt-wasm-wasi::WasiAdapter` provides the explicit binding layer for
Preview 1-style imports. It wraps the lower-level `tpt-wasm-host::WasiAdapter`
and supplies common import labels; the embedder still supplies each signature
and capability operation explicitly.

```text
WASI import name → configured WasiBinding → Host::invoke → capability
```

Only bindings explicitly supplied by the embedder are installed in the
`wasi_snapshot_preview1` linker namespace. There are no default filesystem,
clock, random, process, or network grants. The dedicated `tpt-wasm-wasi` crate
and complete ABI remain future work.
