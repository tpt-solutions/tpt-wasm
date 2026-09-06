# Architecture Overview

tpt-wasm is a specification-first WebAssembly execution system organized around four complementary layers.

## The fundamental invariant

> There is one meaning of a WebAssembly program, expressed formally and embodied
> by the Micro Interpreter. Every optimized implementation must preserve that meaning.

## Four-layer architecture

### Layer 1 — Micro Interpreter (Golden Machine)

**Question:** What does WebAssembly *mean*?

**Crate:** `tpt-wasm-micro`

Characteristics: small, obvious, portable, deterministic, inspectable, minimal unsafe,
specification-oriented, intentionally not optimized.

### Layer 2 — TPT Host

**Question:** What may a WebAssembly program *interact with*?

**Crates:** `tpt-wasm-host`, `tpt-wasm-capability`, `tpt-wasm-resource`

Characteristics: capabilities, resources, explicit authority, isolation, deterministic
host modes, TPT integration, WASI adapters. No ambient authority.

### Layer 3 — Formal Verification

**Question:** Can we *prove* that our implementation preserves the specified behavior?

**Crates:** `tpt-wasm-semantics`, `tpt-wasm-verify`; Lean 4 proofs in `formal/`

Characteristics: formal machine, transition relations, invariants, refinement,
memory safety properties, host capability properties, eventually compiler correctness.

### Layer 4 — Full Compiler

**Question:** Can we execute exactly the same semantics *efficiently*?

**Crates:** `tpt-wasm-ir`, `tpt-wasm-codegen`, `tpt-wasm-jit`, `tpt-wasm-aot`

Characteristics: independent TPT IR, baseline compilation, optimizing compilation,
JIT, AOT, multiple native architectures, eventually verified transformations.

## Crate dependency rules

```
tpt-wasm-types
      │
      ├── format
      │     └── decode
      │           └── validate
      │                 └── micro
      │                       └── runtime
      │                             └── host
      │
      └── semantics
            └── verify

validate
  └── ir
        ├── codegen
        ├── jit
        └── aot
```

**Critical:** The compiler must not depend on the interpreter.
The host must not depend on the semantic core.
The TPT ecosystem must not depend on Core Wasm.

## See also

- [layers.md](layers.md) — detailed layer descriptions
- [boundaries.md](boundaries.md) — inter-layer interfaces
- [invariants.md](invariants.md) — non-negotiable architectural invariants
