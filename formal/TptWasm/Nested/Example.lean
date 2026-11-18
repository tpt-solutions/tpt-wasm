import TptWasm.Nested.Program

/-!
# A concrete nested program

The program has one local and an `if` nested in the then-arm of another `if`:

```text
if (i32.const 1)
  then: if (i32.const 0) then (local.set 0 5) else (local.set 0 7)
  else: (local.set 0 9)
return local.get 0
```

The outer condition is 1, so the then-arm runs; its inner condition is 0, so the else-arm runs and
the local becomes 7. The program returns 7 with no memory or global events. `nested_triangle`
applies to it, which shows the theorem is inhabited at a nesting depth of two.
-/

namespace TptWasm
namespace Nested
namespace Example

open V4Straight MachineDiamond

/-- The program body. -/
def bodyEx : List Stmt :=
  [.ifElse [.i32Const 1]
    [.ifElse [.i32Const 0] [.straight [.i32Const 5, .localSet 0]]
      [.straight [.i32Const 7, .localSet 0]]]
    [.straight [.i32Const 9, .localSet 0]]]

/-- The return expression. -/
def retEx : List Instr := [.localGet 0]

/-- The function: its body is the flat layout of the program. -/
def fnEx : FunctionState :=
  { params := [], results := [.i32], localTypes := [.i32], body := flatSeq bodyEx ++ retEx }

/-- The store holds the function as index 0. -/
def storeEx : Store := { functions := [fnEx] }

/-- The start configuration: one frame, local 0 holding `i32 0`. -/
def cEx : Configuration :=
  { store := storeEx, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The IR state the lowering starts from. -/
def sEx : IR.State := { values := fun _ => .i32 0, store := storeEx }

theorem supBody : SupSeq bodyEx := by
  simp [bodyEx, SupSeq, SupStmt, V4Diamond.Supported]

theorem supRet : ∀ i ∈ retEx, V4Diamond.Supported i := by
  simp [retEx, V4Diamond.Supported]

theorem bc_start : BoundCorr cEx sEx (List.range' 0 1) := by
  refine ⟨rfl, rfl, ⟨⟨0, [.i32 0], 0, 0, 0, 0⟩, rfl, rfl⟩, ?_⟩
  intro id hid
  simp at hid
  subst hid
  exact ⟨0, rfl⟩

theorem globals_start : V4Diamond.GlobalsI32 cEx.store := by
  intro g hg
  simp [cEx, storeEx] at hg

/-- The configuration after the body: local 0 holds `i32 7`. -/
def c1ex : Configuration :=
  { store := storeEx, frames := [⟨0, [.i32 7], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The configuration after the return expression: the stack holds `i32 7`. -/
def c2ex : Configuration := { c1ex with operandStack := [.i32 7] }

/-- The example, end to end: the program lowers, the lowered function returns `7`, and the machine
ends with `7` on its stack. Both runs have no events. -/
theorem example_triangle :
    ∃ lf, lowerProg bodyEx retEx 1 = some lf ∧
      (∃ cEnd, MReach (cEx.setActivePc 0) cEnd [] ∧ cEnd.currentInstr? = none ∧
        cEnd.operandStack = [.i32 7]) ∧
      ∃ n0, IR.runFunction lf (n0 + 2) sEx = .returned [] [.i32 7] := by
  obtain ⟨lf, hl⟩ : ∃ lf, lowerProg bodyEx retEx 1 = some lf := ⟨_, rfl⟩
  have h1 : runSeq 1 bodyEx cEx = some (c1ex, []) := by
    simp only [runSeq, runStmt, bodyEx, cEx]
    rfl
  have h2 : runWasm retEx c1ex = some (c2ex, []) := rfl
  obtain ⟨hm, hir⟩ := nested_triangle 1 bodyEx retEx 1 lf hl cEx c1ex c2ex [] [] sEx
    ⟨0, [.i32 0], 0, 0, 0, 0⟩ fnEx bc_start globals_start rfl rfl rfl supBody supRet h1 h2
  obtain ⟨cEnd, hr, hend, hst⟩ := hm
  obtain ⟨n0, hn⟩ := hir
  have hst' : c2ex.operandStack = [.i32 7] := rfl
  exact ⟨lf, hl, ⟨cEnd, hr, hend, by rw [hst, hst']⟩, n0, by rw [hn, hst', List.nil_append]⟩

end Example
end Nested
end TptWasm
