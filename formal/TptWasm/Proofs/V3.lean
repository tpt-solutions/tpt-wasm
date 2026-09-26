import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store
import TptWasm.Config
import TptWasm.Transition
import TptWasm.Invariants

/-!
# V3: host capability safety

The property named in `spec.txt` section 28:

```
WasmExecution => OnlyGrantedCapabilitiesInvoked
```

and the capability invariant from `GOVERNANCE.md`:

```
Authority(WasmInstance) ⊆ GrantedCapabilities(WasmInstance)
```

The host boundary in this model is the `callHost` instruction, which names a
capability. The rule that invokes it carries the grant as a premise, so a host
capability can only be invoked when the embedder granted it, and execution
cannot produce a grant for itself.
-/

namespace TptWasm

/-- V3: invoking a host capability requires the grant.

This is the whole safety argument. `callHost` is the only way to reach the host
in the modeled subset, and its rule carries `granted : capability ∈ c.grants` as
a premise, so a derivation of the rule is a derivation of the grant. -/
theorem host_call_requires_grant {c c' : Configuration} {capability : String}
    (h : OpStep (.callHost capability) c (.step c')) : capability ∈ c.grants := by
  cases h with
  | callHost _ _ granted => exact granted

/-- V3: an ungranted host capability has no rule, so the machine is stuck rather
than reaching the host. -/
theorem ungranted_host_call_cannot_step {c c' : Configuration} {capability : String}
    (hnone : capability ∉ c.grants) : ¬ OpStep (.callHost capability) c (.step c') := by
  intro h
  exact hnone (host_call_requires_grant h)

/-- V3: execution cannot widen its own authority.

A step leaves the granted capabilities exactly as it found them, so a
configuration that starts with no grants never gains any, and a grant can
neither be lost nor transferred by running. -/
theorem execution_preserves_authority {c c' : Configuration} {instr : Instr}
    (h : OpStep instr c (.step c')) : c'.grants = c.grants :=
  (opStep_disciplined h).2

/-- V3, as a closure property: no grants in, no grants out. -/
theorem no_grants_stay_no_grants {c c' : Configuration} {instr : Instr}
    (h : OpStep instr c (.step c')) (hnone : c.grants = []) : c'.grants = [] := by
  rw [execution_preserves_authority h, hnone]

/-- V3, at the machine level, where the step also advances a program counter. -/
theorem machine_preserves_authority {c c' : Configuration} (h : Step c (.step c')) :
    c'.grants = c.grants := by
  cases h with
  | run fetched rule =>
      simpa only [GrantsUnchanged, advance_grants c] using
        (show GrantsUnchanged (c.advance) c' from (opStep_disciplined rule).2)

end TptWasm