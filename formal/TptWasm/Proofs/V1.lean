import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store
import TptWasm.Config
import TptWasm.Transition

/-!
# V1: interpreter correspondence

The target is the one named in `docs/verification/model.md`:

```
FormalRule(op) ~= Interpreter(op)
```

`OpStep` is the rule, `execOp` is the interpreter, and this file proves the two
agree: the interpreter returns exactly the configuration the rule names. The
claim is about the modeled instruction subset, mirrored from
`tpt_wasm_semantics::Instruction` in the Rust model.

*Soundness*: every rule is respected by the interpreter, so running the
interpreter is at least as faithful as applying the rules.

*Determinism*: at most one outcome is derivable for a configuration, so the
semantics is not a nondeterministic relation in disguise. This is what lets the
Rust model expose a single `step()` and the runtime expose a single call result.
-/

namespace TptWasm

/-- Looking up the last entry of a list appended with one more element. -/
theorem get?_snoc (xs : List Value) (v : Value) :
    Store.get? (xs ++ v :: []) xs.length = some v := by
  induction xs with
  | nil => rfl
  | cons x xs ih => simp [Store.get?, ih]

/-- The length of a mapped list follows from the mapped list itself. -/
theorem length_map_congr {α β : Type} (f : α → β) {l : List α} {m : List β}
    (h : l.map f = m) : l.length = m.length := by
  have := congrArg List.length h
  simpa using this

/-- Taking as many entries as the first part leaves, keeps the first part. -/
theorem take_append_self (xs ys : List Value) : (xs ++ ys).take xs.length = xs := by simp

/-- Dropping as many entries as the first part has, leaves the second part. -/
theorem drop_append_self (xs ys : List Value) : (xs ++ ys).drop xs.length = ys := by simp

/-- Replacing the last entry of a list with the same value changes nothing. -/
theorem replaceAt?_snoc (xs : List Value) (v : Value) :
    Configuration.replaceAt? (xs ++ v :: []) xs.length v = some (xs ++ v :: []) := by
  induction xs with
  | nil => rfl
  | cons x xs ih => simp [Configuration.replaceAt?, ih]

/-- The results a frame returns are the first `arity` entries of the stack. -/
theorem take_snoc (results rest : List Value) :
    (results ++ rest).take results.length = results := by simp

/-- V1, soundness: the interpreter agrees with every rule. -/
theorem execOp_sound {instr : Instr} {c : Configuration} {out : Outcome}
    (h : OpStep instr c out) : execOp instr c = some out := by
  cases h <;>
    simp_all [execOp, execReturn, Configuration.push, Configuration.pushCall,
      Configuration.setLocal?, get?_snoc, replaceAt?_snoc, maxCallDepth] <;>

    (try split) <;> omega <;>
    simp_all [execReturn, take_snoc, maxCallDepth, length_map_congr,
      take_append_self, drop_append_self] <;>
    omega

/-- V1, determinism: two derivations from the same configuration agree. -/
theorem OpStep.deterministic {instr : Instr} {c : Configuration} {o1 o2 : Outcome}
    (h1 : OpStep instr c o1) (h2 : OpStep instr c o2) : o1 = o2 := by
  have e1 := execOp_sound h1
  have e2 := execOp_sound h2
  rw [e1] at e2
  exact Option.some.inj e2

/-- The machine transition inherits determinism from the rules. -/
theorem Step.deterministic {c : Configuration} {o1 o2 : Outcome}
    (h1 : Step c o1) (h2 : Step c o2) : o1 = o2 := by
  cases h1
  cases h2
  simp_all
  exact OpStep.deterministic (by assumption) (by assumption)

/-- V1, soundness for the machine: the interpreter agrees with every step. -/
theorem exec_sound {c : Configuration} {out : Outcome} (h : Step c out) :
    exec c = some out := by
  cases h with
  | run fetched rule =>
    simp only [exec]
    rw [fetched]
    exact execOp_sound rule

end TptWasm
