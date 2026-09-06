# Formal Verification Model

**Status:** M5 — planned  
**Proof assistant:** Lean 4  
**Lean 4 source:** `formal/`

---

## Abstract machine

The abstract machine Configuration mirrors WebAssembly Core spec §4:

```
Configuration = {
    Store,
    FrameStack,
    OperandStack,
    ControlStack
}
```

Transitions: `C → C'`

---

## Correspondence

For every WebAssembly instruction `op`, the formal rule and the Rust interpreter step
must correspond:

```
FormalRule(op) ≈ Interpreter(op)
```

This is the verification target for V1.

---

## Verification ladder

| Level | Property | Status |
|---|---|---|
| V0 | Type invariants | Planned M5 |
| V1 | Interpreter correspondence | Planned M5 |
| V2 | Memory safety: `Valid(M) ⇒ NoWasmOOB` | Planned M5 |
| V3 | Host capability safety: `WasmExec ⇒ OnlyGrantedCaps` | Planned M5 |
| V4 | IR refinement: `Wasm ≈ TPT IR` | Planned M9 |
| V5 | Compiler transformation proofs | Planned M9 |
| V6 | Machine-code refinement | Long-term goal |

---

## Formal invariants (targets)

**Type soundness:**
```
Valid(Module) ⇒ WellTyped(Execution)
```

**Memory safety:**
```
Valid(Module) ⇒ NoWasmMemoryOutOfBoundsAccess
```

**Control-flow safety:**
```
Valid(Module) ⇒ ValidControlTransfer
```

**Call safety:**
```
Valid(Module) ⇒ TypeCorrectCalls
```

**Host safety:**
```
WasmExecution ⇒ OnlyGrantedCapabilitiesInvoked
```

**Capability security:**
```
Authority(WasmInstance) ⊆ GrantedCapabilities(WasmInstance)
```
