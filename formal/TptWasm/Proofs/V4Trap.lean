import TptWasm.Proofs.V4Straight
import TptWasm.Proofs.MergeGlobal
import TptWasm.Proofs.V4Diamond

/-!
# A straight-line trap, with its events and its trap

`V4Straight.trap_position` says a trapping straight-line body gives the IR block a `none` result. This
file gives the stronger form the nested model needs: the block's `runBodyR` result is `.inl`, with the
events the Wasm run performed before the trap, in order, and the trap itself. The Wasm side is the
split `pre ; i ; post` at the trapping instruction `i`, with `pre` run to `ck` and `i` trapping in
`ck`. Only memory accesses trap in the lifted subset (`i32` division is not lifted), so the trap is
`memoryOutOfBounds` in both models, and the per-instruction cases are the loads and stores.
-/

namespace TptWasm
namespace V4Straight

/-- A load that traps has the out-of-bounds trap. -/
theorem execOp_load_trap_id (op : LoadOp) (index : Nat) (arg : MemArg) (c : Configuration)
    (t : Trap) (h : execOp (.load op index arg) c = some (.trap t)) :
    t = Trap.memoryOutOfBounds := by
  simp only [execOp] at h
  split at h
  · split at h
    · cases h
    · rename_i hread
      cases h
      rfl
  · simp at h

/-- A store that traps has the out-of-bounds trap. -/
theorem execOp_store_trap_id (op : StoreOp) (index : Nat) (arg : MemArg) (c : Configuration)
    (t : Trap) (h : execOp (.store op index arg) c = some (.trap t)) :
    t = Trap.memoryOutOfBounds := by
  simp only [execOp] at h
  split at h
  · split at h
    · split at h
      · cases h
      · rename_i hwrite
        cases h
        rfl
    · simp at h
  · simp at h

/-- A load whose read fails traps with the out-of-bounds trap, having observed nothing. -/
theorem runBodyR_load_trap (r index a : Nat) (offset : Nat) (op : LoadOp) (body : List IR.Instr)
    (s : IR.State) (t : List Event) (x : Int) (m : MemoryState)
    (ha : s.values a = .i32 x) (hm : s.store.memory? index = some m)
    (hread : m.readLE? (effectiveAddress x { offset := offset }) op.width = none) :
    IR.runBodyR (IR.Instr.load r index a offset op :: body) s t =
      .inl (t, Trap.memoryOutOfBounds) := by
  simp [IR.runBodyR, IR.execInstr, IR.intPayload, ha, hm, hread]

/-- A store whose write fails traps with the out-of-bounds trap, having observed nothing. -/
theorem runBodyR_store_trap (index : Nat) (a v : IR.ValueId) (offset width : Nat)
    (body : List IR.Instr) (s : IR.State) (t : List Event) (x : Int) (operand : Value)
    (m : MemoryState) (ha : s.values a = .i32 x) (hv : s.values v = operand)
    (hm : s.store.memory? index = some m)
    (hwrite : m.writeLE? (effectiveAddress x { offset := offset }) width (storedBits operand) = none) :
    IR.runBodyR (IR.Instr.store index a v offset width :: body) s t =
      .inl (t, Trap.memoryOutOfBounds) := by
  simp [IR.runBodyR, IR.execInstr, IR.intPayload, ha, hv, hm, hwrite]

/-- The straight-line trap with its events and its trap: the block's result is the trace before the
trap, extended by the events of `pre`, and the trap. -/
theorem trap_ir (pre post : List Instr) (i : Instr) (c ck : Configuration) (evs : List Event)
    (t : Trap) (hpre : runWasm pre c = some (ck, evs)) (htrap : execOp i ck = some (.trap t)) :
    ∀ (s : IR.State) (stack locs : List IR.ValueId) (n : Nat) (ir : List IR.Instr)
      (final finalLocs : List IR.ValueId) (m : Nat) (trace : List Event),
      Corresponds c s stack locs n →
      (∀ g ∈ c.store.globals, g.ty = .i32 ∧ g.value.type = .i32) →
      lower (pre ++ i :: post) stack locs n = some (ir, final, finalLocs, m) →
      IR.runBodyR ir s trace = .inl (trace ++ evs, t) := by
  intro s stack locs n ir final finalLocs m trace hc hgl hl
  obtain ⟨b1, st1, l1, k1, b2, hlp, hlq, hir⟩ :=
    lower_append pre (i :: post) stack locs n ir final finalLocs m hl
  obtain ⟨s1, hrun1, hc1⟩ :=
    simulation pre c ck evs s stack locs n b1 st1 l1 k1 hc hgl hlp hpre trace
  have hstk1 : ck.operandStack = (st1.map s1.values) := hc1.1
  have hst1 : ck.store = s1.store := hc1.2.2.2.1
  rw [hir, IR.runBodyR_append, V4Diamond.runBodyR_of_runBody b1 s trace s1 (trace ++ evs) hrun1]
  show IR.runBodyR b2 s1 (trace ++ evs) = _
  cases i with
  | i32Const v =>
    simp only [execOp] at htrap
    cases htrap
  | i32Bin op =>
    have hun := i32Bin_trap_unlifted op ck t htrap
    rcases st1 with _ | ⟨r, _ | ⟨l, st0⟩⟩
    · simp [lower] at hlq
    · simp [lower] at hlq
    · simp [lower, hun] at hlq
  | i32Cmp op =>
    simp only [execOp] at htrap
    split at htrap <;> simp at htrap
  | load op index arg =>
    obtain ⟨x, rest, mem, hsk, hm, hread⟩ := execOp_load_trap op index arg ck t htrap
    rcases st1 with _ | ⟨a, st0⟩
    · simp [lower] at hlq
    obtain ⟨body', hb2⟩ := lower_head_load op index arg post a st0 l1 k1 b2 final finalLocs m hlq
    have hsk' : (a :: st0).map s1.values = .i32 x :: rest := by rw [← hstk1]; exact hsk
    have hax : s1.values a = .i32 x := by
      simp only [List.map_cons, List.cons.injEq] at hsk'
      exact hsk'.1
    have hm1 : s1.store.memory? index = some mem := by rw [← hst1]; exact hm
    have harg : ({ offset := arg.offset } : MemArg) = arg := by cases arg; rfl
    rw [hb2, runBodyR_load_trap k1 index a arg.offset op body' s1 (trace ++ evs) x mem hax hm1
      (by rw [harg]; exact hread), execOp_load_trap_id op index arg ck t htrap]
  | store op index arg =>
    obtain ⟨operand, x, rest, mem, hsk, hm, hwrite⟩ := execOp_store_trap op index arg ck t htrap
    rcases st1 with _ | ⟨v, _ | ⟨a, st0⟩⟩
    · simp [lower] at hlq
    · simp [lower] at hlq
    obtain ⟨body', hb2⟩ := lower_head_store op index arg post v a st0 l1 k1 b2 final finalLocs m hlq
    have hsk' : (v :: a :: st0).map s1.values = operand :: .i32 x :: rest := by
      rw [← hstk1]; exact hsk
    have hvv : s1.values v = operand := by
      simp only [List.map_cons, List.cons.injEq] at hsk'
      exact hsk'.1
    have hax : s1.values a = .i32 x := by
      simp only [List.map_cons, List.cons.injEq] at hsk'
      exact hsk'.2.1
    have hm1 : s1.store.memory? index = some mem := by rw [← hst1]; exact hm
    have harg : ({ offset := arg.offset } : MemArg) = arg := by cases arg; rfl
    rw [hb2, runBodyR_store_trap index a v arg.offset op.width body' s1 (trace ++ evs) x operand mem
      hax hvv hm1 (by rw [harg]; exact hwrite), execOp_store_trap_id op index arg ck t htrap]
  | globalGet index =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | globalSet index =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | localGet k =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | localSet k =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | localTee k =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | _ => simp [lower] at hlq

end V4Straight
end TptWasm
