import TptWasm.Proofs.MachineDiamond
import TptWasm.Proofs.V4Diamond
import TptWasm.Proofs.V4DiamondExample

/-!
# The diamond, end to end: the machine and the block graph agree

`TptWasm.Proofs.MachineDiamond.machine_diamond` says the machine, run over the flat body of a
diamond, reaches the end of the body with the events and operand stack that `runShape` gives.
`TptWasm.Proofs.V4Diamond.diamond_simulation` says the lowered block graph returns those same
events and values. Both are about the same Wasm run, so they can be stated as one theorem, which
is what `triangle` does: from one configuration, the machine's final stack is the block graph's
returned values, and the events agree.

The two sides are linked through `runShape`, the list semantics, not directly to each other. That
is the honest shape of the link at this level of the model: the machine is related to the list
semantics, and the block graph is related to the list semantics, so they are related to each other.
-/

namespace TptWasm
namespace Triangle

open V4Straight MachineDiamond

/-- The machine, the list semantics, and the block graph agree on one diamond from one start. -/
theorem triangle (d : Diamond.Shape) (c c' : Configuration) (evs : List Event)
    (f : Frame) (fn : FunctionState)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length
      d.elseArm.length)
    (hpre : ∀ i ∈ d.pre, V4Diamond.Supported i)
    (hthen : ∀ i ∈ d.thenArm, V4Diamond.Supported i)
    (helse : ∀ i ∈ d.elseArm, V4Diamond.Supported i)
    (hpost : ∀ i ∈ d.post, V4Diamond.Supported i)
    (locs : List IR.ValueId) (n : Nat) (lf : IR.Function) (s : IR.State) (k : Nat)
    (hc : Corresponds c s [] locs n) (hg : V4Diamond.GlobalsI32 c.store)
    (hl : Diamond.lowerShape d locs n = some lf)
    (hrun : Diamond.runShape d c = some (c', evs)) :
    ∃ cEnd, MReach (c.setActivePc 0) cEnd evs ∧ cEnd.currentInstr? = none ∧
      IR.runFunction lf (k + 3) s = .returned evs cEnd.operandStack := by
  obtain ⟨cEnd, hreach, hend, hstack⟩ :=
    MachineDiamond.machine_diamond d c c' evs f fn hf hfn hbody hpre hthen helse hpost hrun
  refine ⟨cEnd, hreach, hend, ?_⟩
  rw [hstack]
  exact V4Diamond.diamond_simulation d c c' evs s locs n lf k hc hg hl hrun

/-! ## The concrete diamond, with its function in the store -/

/-- The example diamond of `TptWasm.V4DiamondExample`. -/
abbrev dEx : Diamond.Shape := V4DiamondExample.dEx

/-- The example's function: its body is the flat layout of the diamond. -/
def fnEx : FunctionState :=
  { params := [], results := [.i32], localTypes := [.i32],
    body := IfControl.layout dEx.pre dEx.thenArm dEx.elseArm dEx.post dEx.thenArm.length
      dEx.elseArm.length }

/-- The example's store holds that function, as index 0. -/
def storeEx : Store := { functions := [fnEx] }

/-- The start configuration: one frame running function 0, local 0 holding `i32 0`. -/
def cEx : Configuration :=
  { store := storeEx, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The IR state the lowering starts from. -/
def sEx : IR.State := { values := fun _ => .i32 0, store := storeEx }

theorem supported_pre : ∀ i ∈ dEx.pre, V4Diamond.Supported i := by
  intro i hi
  simp [dEx, V4DiamondExample.dEx] at hi
  subst hi
  simp [V4Diamond.Supported]

theorem supported_then : ∀ i ∈ dEx.thenArm, V4Diamond.Supported i := by
  intro i hi
  simp [dEx, V4DiamondExample.dEx] at hi
  rcases hi with rfl | rfl <;> simp [V4Diamond.Supported]

theorem supported_else : ∀ i ∈ dEx.elseArm, V4Diamond.Supported i := by
  intro i hi
  simp [dEx, V4DiamondExample.dEx] at hi
  rcases hi with rfl | rfl <;> simp [V4Diamond.Supported]

theorem supported_post : ∀ i ∈ dEx.post, V4Diamond.Supported i := by
  intro i hi
  simp [dEx, V4DiamondExample.dEx] at hi
  subst hi
  simp [V4Diamond.Supported]

/-- The configuration and the IR state correspond at the start of the example. -/
theorem corr_ex : Corresponds cEx sEx [] [0] 1 := by
  refine ⟨rfl, by simp, by simp, rfl, ⟨⟨⟨0, [.i32 0], 0, 0, 0, 0⟩, rfl, rfl⟩, by simp, ?_⟩⟩
  intro id hid
  simp at hid
  subst hid
  exact ⟨0, rfl⟩

theorem globals_ex : V4Diamond.GlobalsI32 cEx.store := by
  intro g hg
  simp [cEx, storeEx] at hg

/-- The example, end to end: the machine ends with the value `5` on its stack, and the block
graph returns the same `5`, with no events. -/
theorem example_triangle :
    ∃ cEnd, MReach (cEx.setActivePc 0) cEnd [] ∧ cEnd.currentInstr? = none ∧
      cEnd.operandStack = [.i32 5] := by
  obtain ⟨c', evs, hrun⟩ := V4DiamondExample.runs
  have hev : (Diamond.runShape dEx cEx).map (fun p => p.2) = some [] := by rfl
  obtain ⟨c'', evs'', hrun'⟩ : ∃ c'' evs'', Diamond.runShape dEx cEx = some (c'', evs'') :=
    ⟨_, _, rfl⟩
  have hevs : evs'' = [] := by
    rw [hrun'] at hev
    simpa using hev
  -- the lowered function, computed: it returns `5`
  have key : ∃ x, Diamond.lowerShape dEx [0] 1 = some x ∧
      IR.runFunction x 3 sEx = .returned [] [.i32 5] := ⟨_, rfl, by rfl⟩
  obtain ⟨x, hx, hxval⟩ := key
  obtain ⟨lf, hl⟩ := V4DiamondExample.lowers
  have hlf : lf = x := Option.some.inj (hl.symm.trans hx)
  subst hlf
  obtain ⟨cEnd, hreach, hend, hir⟩ :=
    triangle dEx cEx c'' evs'' ⟨0, [.i32 0], 0, 0, 0, 0⟩ fnEx rfl rfl rfl
      supported_pre supported_then supported_else supported_post [0] 1 lf sEx 0
      corr_ex globals_ex hl hrun'
  subst hevs
  have hIR : (IR.Result.returned [] cEnd.operandStack : IR.Result) =
      IR.Result.returned [] [.i32 5] := hir.symm.trans hxval
  injection hIR with _ h2
  exact ⟨cEnd, hreach, hend, h2⟩

end Triangle
end TptWasm
