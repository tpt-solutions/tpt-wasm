import TptWasm.Nested.Example

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

open V4Straight Example

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

end LoopExample
end Nested
end TptWasm
