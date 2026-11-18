import TptWasm.Proofs.V4Diamond

/-!
# A worked instance of the diamond simulation

`TptWasm.Proofs.V4Diamond.diamond_simulation` is stated for every diamond. This file instantiates
it at one concrete diamond and discharges each hypothesis, so the theorem is shown to apply rather
than to hold vacuously.

The diamond: `i32.const 1`; `if` with then-arm `i32.const 5; local.set 0` and else-arm
`i32.const 7; local.set 0`; then `local.get 0`. The condition is 1, so the then arm runs and the
function returns `5`, with no memory or global events.
-/

namespace TptWasm
namespace V4DiamondExample

open V4Straight Diamond

/-- The configuration the diamond starts in: one frame whose local 0 holds `i32 0`. -/
def c0 : Configuration :=
  { store := {}, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The example diamond. -/
def dEx : Shape :=
  { pre := [.i32Const 1], thenArm := [.i32Const 5, .localSet 0],
    elseArm := [.i32Const 7, .localSet 0], post := [.localGet 0] }

/-- The IR state the lowering starts from: local 0 is the value id 0, holding `i32 0`. -/
def st : IR.State := { values := fun _ => .i32 0, store := {} }

/-- The configuration and the IR state correspond at the start. -/
theorem corr_start : Corresponds c0 st [] [0] 1 := by
  refine ⟨rfl, by simp, by simp, rfl, ⟨⟨⟨0, [.i32 0], 0, 0, 0, 0⟩, rfl, rfl⟩, by simp, ?_⟩⟩
  intro id hid
  simp at hid
  subst hid
  exact ⟨0, rfl⟩

/-- The example's globals are `i32`-typed, since it has none. -/
theorem globals_start : V4Diamond.GlobalsI32 c0.store := by
  intro g hg
  simp [c0] at hg

/-- The example lowers to a four-block function. -/
theorem lowers : ∃ f, lowerShape dEx [0] 1 = some f := ⟨_, rfl⟩

/-- The example's Wasm run, and the configuration it ends in. -/
theorem runs : ∃ c' evs, runShape dEx c0 = some (c', evs) := ⟨_, _, rfl⟩

/-- The simulation applies to the example, and the lowered function returns `5` with no events. -/
theorem example_returns_five :
    ∀ f, lowerShape dEx [0] 1 = some f → IR.runFunction f 3 st = .returned [] [.i32 5] := by
  intro f hf
  obtain ⟨c', evs, hr⟩ := runs
  have hsim : IR.runFunction f 3 st = .returned evs c'.operandStack :=
    V4Diamond.diamond_simulation dEx c0 c' evs st [0] 1 f 0 corr_start globals_start hf hr
  have hstack : (runShape dEx c0).map (fun p => p.1.operandStack) = some [.i32 5] := by rfl
  have hev : (runShape dEx c0).map (fun p => p.2) = some [] := by rfl
  rw [hr] at hstack hev
  simp only [Option.map_some, Option.some.injEq] at hstack hev
  rw [hsim, hstack, hev]

end V4DiamondExample
end TptWasm
