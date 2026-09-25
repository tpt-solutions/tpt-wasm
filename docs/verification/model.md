# Formal Verification Model

**Status:** M5 — executable subset; Lean proofs pending
**Proof assistant:** Lean 4 (not yet installed)
**Lean 4 source:** `formal/`

---

## Abstract machine

The Rust model in `tpt-wasm-semantics` represents a WebAssembly Core §4
configuration for the currently modeled instruction subset:

```
Configuration = {
    Store,
    FrameStack,
    OperandStack,
    ControlStack
}
```

`Configuration::step` returns `Transition::Step`, `Transition::Return`, or
`Transition::Trap`. The model is independent of the Micro interpreter and
compiler internals.

### Implemented subset

The executable model currently covers:

- `i32` and `i64` constants
- `f32` and `f64` constants
- `drop`
- `select`
- `local.get`, `local.set`, and `local.tee`
- direct calls to defined functions and call-frame return semantics
- `i32`/`i64` add, subtract, multiply, divide/remainder, shifts, rotations, comparisons, AND, OR, and XOR
- `i32`/`i64` `clz`, `ctz`, and `popcnt`
- `i32.wrap_i64`, `i64.extend_i32_s`, and `i64.extend_i32_u`
- non-trapping `f32`/`f64` integer conversions and demote/promote
- trapping `i32`/`i64` conversions from `f32`/`f64` with finite/range checks
- `i32.reinterpret_f32`, `i64.reinterpret_f64`, `f32.reinterpret_i32`, and `f64.reinterpret_i64`
- `f32`/`f64` comparisons
- `f32`/`f64` `abs`, `neg`, `ceil`, `floor`, `trunc`, `nearest`, and `sqrt`
- `f32`/`f64` `min`, `max`, and `copysign`
- `f32`/`f64` add, subtract, multiply, and divide
- `return` and `end`
- function, frame, operand, and control stack transitions
- typed operand underflow and return-arity checks

It does not yet cover the full MVP control, memory, table, reference,
imported/indirect call, and numeric instruction set.

---

## Executable V0 checks

`tpt_wasm_semantics::Configuration::check_invariants` checks the following
properties for the modeled configuration:

- active frames reference known functions;
- active program counters are within the function body;
- local and result arities agree with the function signature;
- frame operand/control bases are ordered and within the stacks;
- control-frame stack heights are representable;
- global values match their declared types.

The checker is exposed through `tpt_wasm_verify::check_v0`. These are ordinary
Rust property tests and runtime checks, not machine-checked proofs.

---

## Correspondence

For every covered instruction, the eventual V1 target is:

```
FormalRule(op) ≈ Interpreter(op)
```

No full interpreter-correspondence proof has been established yet.

---

## Verification ladder

| Level | Property | Status |
|---|---|---|
| V0 | Type/configuration invariants | Executable subset implemented |
| V1 | Interpreter correspondence | Planned; selected subset only |
| V2 | Memory safety: `Valid(M) ⇒ NoWasmOOB` | Planned |
| V3 | Host capability safety: `WasmExec ⇒ OnlyGrantedCaps` | Planned |
| V4 | IR refinement: `Wasm ≈ TPT IR` | Executable projection for modeled subset; proof pending |
| V5 | Compiler transformation proofs | Planned M9 |
| V6 | Machine-code refinement | Long-term goal |

---

## Formal invariant targets

The following remain formal targets rather than completed claims:

- **Type soundness:** `Valid(Module) ⇒ WellTyped(Execution)`
- **Memory safety:** `Valid(Module) ⇒ NoWasmMemoryOutOfBoundsAccess`
- **Control-flow safety:** `Valid(Module) ⇒ ValidControlTransfer`
- **Call safety:** `Valid(Module) ⇒ TypeCorrectCalls`
- **Host safety:** `WasmExecution ⇒ OnlyGrantedCapabilitiesInvoked`
- **Capability security:** `Authority(WasmInstance) ⊆ GrantedCapabilities(WasmInstance)`
