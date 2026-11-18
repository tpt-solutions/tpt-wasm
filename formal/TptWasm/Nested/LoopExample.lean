import TptWasm.Nested.Example
import TptWasm.Nested.Program

/-!
# A concrete loop

The program sets local 0 to `3`, then loops while the local is non-zero, subtracting one each time:

```text
local.set 0 3
loop while (local.get 0): local.set 0 (local.get 0 - 1)
return local.get 0
```

The loop runs three times, so the list semantics needs fuel four: each iteration uses one unit, and
the final test of the condition uses one more. The lowered program returns `0`, and the simulation
applies, which shows the loop theorem is inhabited.
-/

namespace TptWasm
namespace Nested
namespace LoopExample

open V4Straight MachineDiamond Example

/-- The loop program: set local 0 to 3, then count it down to zero. -/
def loopEx : List Stmt :=
  [.straight [.i32Const 3, .localSet 0],
   .whileNZ [.localGet 0]
     [.straight [.localGet 0, .i32Const 1, .i32Bin .sub, .localSet 0]]]

/-- The configuration after the loop: local 0 holds `i32 0`. -/
def c1loop : Configuration :=
  { store := storeEx, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The configuration after the return expression: the stack holds `i32 0`. -/
def c2loop : Configuration := { c1loop with operandStack := [.i32 0] }

theorem supLoop : SupSeqL loopEx := by
  simp [loopEx, SupSeqL, SupStmtL, V4Diamond.Supported]

/-- The loop program, lowered, returns `0`, with no events. -/
theorem loop_returns_zero :
    ∃ lf, lowerProg loopEx retEx 1 = some lf ∧
      ∃ n0, IR.runFunction lf (n0 + 2) sEx = .returned [] [.i32 0] := by
  obtain ⟨lf, hl⟩ : ∃ lf, lowerProg loopEx retEx 1 = some lf := ⟨_, rfl⟩
  have h1 : runSeq 4 loopEx cEx = some (c1loop, []) := by
    simp only [runSeq, runStmt, loopEx, cEx]
    rfl
  have h2 : runWasm retEx c1loop = some (c2loop, []) := rfl
  obtain ⟨n0, hn⟩ := nested_ir_run 4 loopEx retEx 1 lf hl cEx c1loop c2loop [] [] sEx
    bc_start globals_start supLoop supRet h1 h2
  exact ⟨lf, hl, n0, by rw [hn, List.nil_append]; rfl⟩

/-- The function that runs the loop program: its body is the flat layout of the program, then the
return expression. -/
def fnLoop : FunctionState :=
  { params := [], results := [.i32], localTypes := [.i32], body := flatSeq loopEx ++ retEx }

/-- The store holds the loop's function as index 0. -/
def storeLoop : Store := { functions := [fnLoop] }

/-- The machine's start: the loop's frame, with local 0 at `i32 0`. -/
def cL : Configuration :=
  { store := storeLoop, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The IR state the lowering starts from. -/
def sL : IR.State := { values := fun _ => .i32 0, store := storeLoop }

/-- The configuration after the loop: local 0 holds `i32 0`. -/
def c1L : Configuration :=
  { store := storeLoop, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The configuration after the return expression: the stack holds `i32 0`. -/
def c2L : Configuration := { c1L with operandStack := [.i32 0] }

theorem bcL : BoundCorr cL sL (List.range' 0 1) := by
  refine ⟨rfl, rfl, ⟨⟨0, [.i32 0], 0, 0, 0, 0⟩, rfl, rfl⟩, ?_⟩
  intro id hid
  simp at hid
  subst hid
  exact ⟨0, rfl⟩

theorem globalsL : V4Diamond.GlobalsI32 cL.store := by
  intro g hg
  simp [cL, storeLoop] at hg

/-- The machine and the IR agree on the loop program: the machine reaches the end of the flat code
with the stack holding `i32 0`, and the lowered function returns `i32 0`. -/
theorem loop_triangle :
    (∃ cEnd, MReach (cL.setActivePc 0) cEnd [] ∧ cEnd.currentInstr? = none ∧
      cEnd.operandStack = [.i32 0]) ∧
    ∃ lf, lowerProg loopEx retEx 1 = some lf ∧
      ∃ n0, IR.runFunction lf (n0 + 2) sL = .returned [] [.i32 0] := by
  obtain ⟨lf, hl⟩ : ∃ lf, lowerProg loopEx retEx 1 = some lf := ⟨_, rfl⟩
  have h1 : runSeq 4 loopEx cL = some (c1L, []) := by
    simp only [runSeq, runStmt, loopEx, cL, c1L]
    rfl
  have h2 : runWasm retEx c1L = some (c2L, []) := rfl
  obtain ⟨hm, hir⟩ := nested_triangle 4 loopEx retEx 1 lf hl cL c1L c2L [] [] sL
    ⟨0, [.i32 0], 0, 0, 0, 0⟩ fnLoop bcL globalsL rfl rfl rfl
    (by simp [loopEx, SupSeq, SupStmt, V4Diamond.Supported]) supRet h1 h2
  obtain ⟨n0, hn⟩ := hir
  exact ⟨hm, lf, hl, n0, by rw [hn, List.nil_append]; rfl⟩

end LoopExample
end Nested
end TptWasm
