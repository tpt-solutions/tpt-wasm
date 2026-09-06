# TPT-Wasm IR

**Status:** M6 — planned  
**Crate:** `tpt-wasm-ir`

---

## Position in pipeline

```
Binary
  ↓ decode
Module
  ↓ validate
ValidatedModule
  ↓ lower
TPT-Wasm IR
  ↓
  ├── Baseline compiler
  ├── Optimizing compiler
  └── Verification model
```

---

## Design principles

- SSA-ready (full SSA in M6+)
- Explicit control flow graph
- Typed values (aligned with Wasm types)
- Explicit memory operations
- Explicit traps
- Explicit calls (direct and indirect)
- Explicit references
- Explicit host boundaries
- No machine registers in initial IR

---

## IR is independent of the interpreter

The compiler must not depend on `tpt-wasm-micro`.
The IR receives a `ValidatedModule`, not internal interpreter state.

---

## Correctness relationship

```
Core Wasm
    ≈
TPT-Wasm IR
    ≈
Optimized IR
    ≈
Machine IR
    ≈
Machine Code
```

Each `≈` is a refinement/equivalence boundary that must be proved or tested.
The IR never becomes the definition of Wasm semantics.
