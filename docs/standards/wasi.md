# WASI

**Status:** Planned (M10)  
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
