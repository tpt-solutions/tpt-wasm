# Layer Descriptions

See [overview.md](overview.md) for the diagram.

## Layer 1 — Micro Interpreter

The Micro Interpreter answers: *What does WebAssembly mean?*

It is the executable definition of the WebAssembly operational semantics (spec §4).
Every instruction is implemented for clarity, not performance.

Target:
- `unsafe` blocks: 0
- Wasm traps: never a Rust panic
- Differential testing: always the golden result

The public API is `step(&mut Machine) -> Step`.
This creates a direct bridge to the formal semantics.

## Layer 2 — TPT Host

The host answers: *What may a WebAssembly program interact with?*

WebAssembly itself provides no ambient access to the host. All interaction happens
through imported, embedder-provided functionality. tpt-wasm preserves this principle
and makes it explicit through the capability architecture.

Capabilities are not POSIX abstractions. They are named, granted permissions with
explicit operations.

## Layer 3 — Formal Verification

The formal layer answers: *Can we prove our implementation is correct?*

The verification ladder (V0–V6) provides a progressive approach:

- V0: Type invariants
- V1: Interpreter correspondence (formal rule ≈ `step()`)
- V2: Memory safety
- V3: Host capability safety
- V4: IR refinement
- V5: Compiler transformation correctness
- V6: Machine-code refinement

Lean 4 is the proof assistant. Rust property tests bridge to the formal model.

## Layer 4 — Full Compiler

The compiler answers: *Can we run the same semantics efficiently?*

The TPT IR is the compiler's internal representation, independent of the interpreter.
Every optimization must preserve the Micro Interpreter's observable semantics.

Architecture-independent IR → architecture-specific lowering → native code.
