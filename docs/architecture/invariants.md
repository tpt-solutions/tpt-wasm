# Architectural Invariants

These invariants define the non-negotiable constraints of the tpt-wasm architecture.
Any change that violates an invariant is an architectural regression.

---

## Invariant 1 — Core Wasm semantics do not depend on TPT

The `tpt-wasm-types`, `tpt-wasm-format`, `tpt-wasm-decode`, `tpt-wasm-validate`,
`tpt-wasm-micro`, and `tpt-wasm-runtime` crates must have zero dependency on any TPT
domain crate (`tpt-system`, `tpt-physics`, `tpt-math`, etc.).

The dependency flows outward: `tpt-wasm → tpt-system → domain crates`.
Not inward.

---

## Invariant 2 — The host has no ambient authority

A WebAssembly program may only interact with host resources through explicitly granted
capabilities. No filesystem access, no network access, no clock access, and no POSIX
access is available by default.

Formally: `Authority(WasmInstance) ⊆ GrantedCapabilities(WasmInstance)`

---

## Invariant 3 — The Micro Interpreter defines observable semantics

The Micro Interpreter is the Golden Machine. It is the canonical, executable definition
of what a WebAssembly program means. All other implementations must produce identical
observable behavior.

Observable behavior includes: return values, traps, memory effects, global effects,
table effects, host effects, and observable ordering.

---

## Invariant 4 — The compiler must preserve Micro semantics

For all valid WebAssembly modules `m` and all inputs:

```
Micro(m, input) == Compiled(m, input)
```

for all externally observable behavior.

This includes trap correctness: an optimization that changes *when* a trap occurs is
incorrect even if the eventual return value looks identical.

---

## Invariant 5 — The formal model is independent of compiler implementation details

The formal semantics (`tpt-wasm-semantics`) must not depend on compiler internals,
IR design decisions, or register allocation. It mirrors the WebAssembly specification,
not the compiled output.

---

## Invariant 6 — TPT domain systems integrate through host/capability boundaries

Physics, FEM, robotics, and other TPT domain systems are clients of the TPT Host.
They are not part of Core Wasm. They do not reach into interpreter internals,
compiler internals, or machine code.

Integration path: `domain crate → capability → tpt-wasm-host → tpt-wasm-runtime`

---

## Invariant 7 — External runtimes are test/reference dependencies, not source dependencies

Wasmtime, Wasmer, WasmEdge, WAMR, and the official reference interpreter may be used for:
- compatibility testing
- behavioral comparison
- differential testing
- performance comparison

They must not be used as:
- source code dependencies
- vendored implementations
- architectural templates copied mechanically

---

## Invariant 8 — Original TPT-Wasm implementation remains independently attributable

Every significant piece of code in this repository can be traced to its origin in
`docs/licensing/provenance.md`. No external runtime implementation code has been
incorporated without explicit license review.
