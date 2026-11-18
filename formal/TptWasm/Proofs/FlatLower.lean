import TptWasm.Proofs.Triangle

/-!
# The lowering, stated over the flat body the machine runs

`TptWasm.Diamond.lowerShape` lowers a `Shape`: four lists. The machine, in
`TptWasm.Proofs.MachineDiamond`, runs the flat body `pre ; if ; then ; else ; post`, with the
markers that carry the arm lengths. Until now the two were related only through the list semantics.

This file defines `decompose`, which reads a flat body back into a `Shape`, and `lowerFlat`, the
lowering from the flat body: `lowerFlat body = lowerShape (decompose body)`. It proves that
decomposing the layout of a shape gives the shape back, so `lowerFlat` of the flat body the machine
runs is the lowering of the diamond. `triangleFlat` is the triangle stated with that lowering.

## What `decompose` accepts

A flat body is read as the straight-line prefix before the first `ifInstr`, then the `t` instructions
that follow it as the then-arm, then an `elseInstr e` marker, then `e` instructions as the else-arm,
and the rest as `post`. A body without a marker, or whose markers do not fit the body, has no
decomposition, and `lowerFlat` refuses it.
-/

namespace TptWasm
namespace FlatLower

open V4Straight MachineDiamond

/-- The arm length an `if` marker carries, when the instruction is an `if`. -/
def ifArg : Instr → Option Nat
  | .ifInstr t => some t
  | _ => none

/-- Given the `if`'s arm length `t` and the instructions after it, read the then-arm, the marker,
the else-arm and `post`. -/
def mkShape (pre : List Instr) (t : Nat) (rest : List Instr) : Option Diamond.Shape :=
  if t ≤ rest.length then
    match rest.drop t with
    | .elseInstr e :: tail =>
      if e ≤ tail.length then
        some { pre := pre, thenArm := rest.take t, elseArm := tail.take e, post := tail.drop e }
      else none
    | _ => none
  else none

/-- Read a flat body: the prefix before the first `if` is the `pre` piece, accumulated in `acc`. -/
def decomposeAux (acc : List Instr) : List Instr → Option Diamond.Shape
  | [] => none
  | x :: xs =>
    match ifArg x with
    | some t => mkShape acc t xs
    | none => decomposeAux (acc ++ [x]) xs

/-- Read a flat body as a diamond. -/
def decompose (body : List Instr) : Option Diamond.Shape :=
  decomposeAux [] body

/-- The lowering, from the flat body the machine runs. -/
def lowerFlat (body : List Instr) (locs : List IR.ValueId) (n : Nat) : Option IR.Function :=
  match decompose body with
  | some d => Diamond.lowerShape d locs n
  | none => none

/-- A supported instruction is not an `if`. -/
theorem ifArg_of_supported (i : Instr) (h : V4Diamond.Supported i) : ifArg i = none := by
  cases i <;> simp_all [V4Diamond.Supported, ifArg]

/-- Reading the `if` and its arms back, when the arms have the lengths the markers carry. -/
theorem mkShape_layout (pre thenBody elseBody post : List Instr) (t e : Nat)
    (ht : thenBody.length = t) (he : elseBody.length = e) :
    mkShape pre t (thenBody ++ .elseInstr e :: (elseBody ++ post)) =
      some { pre := pre, thenArm := thenBody, elseArm := elseBody, post := post } := by
  have h1 : t ≤ (thenBody ++ .elseInstr e :: (elseBody ++ post)).length := by
    simp
    omega
  unfold mkShape
  rw [if_pos h1, List.drop_left' ht]
  have h2 : e ≤ (elseBody ++ post).length := by
    simp
    omega
  simp only [List.cons.injEq, List.take_left' ht, List.drop_left' ht]
  rw [if_pos h2]
  simp [List.take_left' he, List.drop_left' he]

/-- Reading a flat body whose prefix has no `if`: the decomposition finds the first `if`. -/
theorem decomposeAux_prefix : ∀ (pre : List Instr) (acc : List Instr) (t : Nat) (R : List Instr),
    (∀ x ∈ pre, ifArg x = none) →
      decomposeAux acc (pre ++ .ifInstr t :: R) = mkShape (acc ++ pre) t R
  | [], acc, t, R, _ => by
    simp [decomposeAux, ifArg]
  | x :: pre, acc, t, R, h => by
    have hx : ifArg x = none := h x (by simp)
    have ih := decomposeAux_prefix pre (acc ++ [x]) t R (fun y hy => h y (by simp [hy]))
    simp only [List.cons_append, decomposeAux, hx]
    rw [ih]
    simp [List.append_assoc]

/-- Decomposing the layout of a diamond gives the diamond back. -/
theorem decompose_layout (d : Diamond.Shape) (hpre : ∀ i ∈ d.pre, ifArg i = none) :
    decompose (IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length
      d.elseArm.length) = some d := by
  have hL : IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length d.elseArm.length =
      d.pre ++ .ifInstr d.thenArm.length :: (d.thenArm ++ .elseInstr d.elseArm.length ::
        (d.elseArm ++ d.post)) := by
    simp [IfControl.layout, List.append_assoc]
  rw [hL, decompose, decomposeAux_prefix d.pre [] _ _ hpre]
  rw [show ([] : List Instr) ++ d.pre = d.pre from rfl]
  rw [mkShape_layout d.pre d.thenArm d.elseArm d.post _ _ rfl rfl]

/-- The lowering of the flat layout is the lowering of the diamond. -/
theorem lowerFlat_layout (d : Diamond.Shape) (hpre : ∀ i ∈ d.pre, ifArg i = none)
    (locs : List IR.ValueId) (n : Nat) :
    lowerFlat (IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length
      d.elseArm.length) locs n = Diamond.lowerShape d locs n := by
  unfold lowerFlat
  rw [decompose_layout d hpre]

/-- The triangle, with the lowering taken from the flat body the machine runs. -/
theorem triangleFlat (d : Diamond.Shape) (c c' : Configuration) (evs : List Event)
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
    (hl : lowerFlat fn.body locs n = some lf)
    (hrun : Diamond.runShape d c = some (c', evs)) :
    ∃ cEnd, MReach (c.setActivePc 0) cEnd evs ∧ cEnd.currentInstr? = none ∧
      IR.runFunction lf (k + 3) s = .returned evs cEnd.operandStack := by
  rw [hbody, lowerFlat_layout d (fun i hi => ifArg_of_supported i (hpre i hi))] at hl
  exact Triangle.triangle d c c' evs f fn hf hfn hbody hpre hthen helse hpost locs n lf s k hc hg hl hrun

end FlatLower
end TptWasm
