import TptWasm.Dominance
import TptWasm.Proofs.MergeGlobal
import TptWasm.Proofs.Fold

/-!
# Constant-condition folding: the whole function

`fold_behaviour`: folding every conditional branch whose condition is a known constant
leaves every function's behaviour unchanged, at every fuel. The fold changes no state
and no step count, so the two functions agree on every run.

## The invariant

The fold is correct where the condition is bound to its constant when the branch runs.
Compiled code guarantees that by SSA dominance: the constant's definition dominates every
use, and a value is bound once. The verifier checks both. `FoldCtx` states the checks the
model needs, and the invariant `Good` says a state entering a block binds every constant
whose defining block strictly dominates it. `Good` holds at the entry trivially, and it is
preserved along each edge (`good_target`), using `dom_pred`.

## What is assumed

`FoldCtx` is the verifier's checks, stated for the model: block identifiers are unique,
each constant is defined only by its `constI32` instruction in its defining block, a
constant is not a block parameter, and every use of a constant is dominated by its
definition. Each field is one of those checks; none is a claim about runs.
-/

namespace TptWasm
namespace IR

/-! ## Facts about running a body -/

theorem exec_values_ran (i : Instr) (s s1 : State) (ev : Option Event)
    (res : List (ValueId × Value)) (h : execInstr i s = .ran s1 ev res) :
    s1.values = s.values := by
  cases i <;> simp only [execInstr] at h <;> (repeat split at h) <;>
    (try obtain ⟨rfl, -, -⟩ := h) <;> rfl

theorem foldl_bind_keep (res : List (ValueId × Value)) (s : State) (v : ValueId)
    (h : ∀ p ∈ res, p.1 ≠ v) :
    (res.foldl (fun st p => st.bind p.1 p.2) s).values v = s.values v := by
  induction res generalizing s with
  | nil => rfl
  | cons p rest ih =>
    simp only [List.foldl_cons]
    rw [ih _ (fun q hq => h q (List.mem_cons_of_mem _ hq))]
    simp [State.bind, Ne.symm (h p List.mem_cons_self)]

/-- A body that defines no value `v` leaves `v` as it was. -/
theorem runBodyR_values_keep (B : List Instr) :
    ∀ (s : State) (tr : List Event) (s' : State) (tr' : List Event) (v : ValueId),
      (∀ i ∈ B, v ∉ definedBy i) → runBodyR B s tr = .inr (s', tr') →
        s'.values v = s.values v := by
  induction B with
  | nil =>
    intro s tr s' tr' v _ h
    simp only [runBodyR, Sum.inr.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    rfl
  | cons i rest ih =>
    intro s tr s' tr' v hv h
    rw [runBodyR] at h
    cases hi : execInstr i s with
    | trapped t => rw [hi] at h; simp at h
    | ran s1 ev res =>
      rw [hi] at h
      have hres : ∀ p ∈ res, p.1 ≠ v := fun p hp e =>
        hv i List.mem_cons_self (e ▸ ran_results_defined i s s1 ev res hi p hp)
      have h1 := ih _ _ s' tr' v (fun j hj => hv j (List.mem_cons_of_mem _ hj)) h
      rw [h1, foldl_bind_keep res s1 v hres, exec_values_ran i s s1 ev res hi]

/-- A body in which every definition of `c` is the constant `k` leaves `c` bound to `k` once
it is. -/
theorem body_stable (c : ValueId) (k : Int) (B : List Instr) :
    (∀ i ∈ B, c ∈ definedBy i → i = .constI32 c k) →
      ∀ (s : State) (tr : List Event) (s' : State) (tr' : List Event),
        s.values c = .i32 k → runBodyR B s tr = .inr (s', tr') → s'.values c = .i32 k := by
  induction B with
  | nil =>
    intro _ s tr s' tr' hv h
    simp only [runBodyR, Sum.inr.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    exact hv
  | cons i rest ih =>
    intro hdef s tr s' tr' hv h
    rw [runBodyR] at h
    cases hi : execInstr i s with
    | trapped t => rw [hi] at h; simp at h
    | ran s1 ev res =>
      rw [hi] at h
      by_cases hc : c ∈ definedBy i
      · have hieq := hdef i List.mem_cons_self hc
        subst hieq
        simp only [execInstr, StepResult.ran.injEq] at hi
        obtain ⟨rfl, rfl, rfl⟩ := hi
        have hv1 : (List.foldl (fun st p => st.bind p.1 p.2) s [(c, Value.i32 k)]).values c =
            .i32 k := by
          simp [State.bind]
        exact ih (fun j hj => hdef j (List.mem_cons_of_mem _ hj)) _ _ s' tr' hv1 h
      · have hres : ∀ p ∈ res, p.1 ≠ c := fun p hp e =>
          hc (e ▸ ran_results_defined i s s1 ev res hi p hp)
        have hv1 : (res.foldl (fun st p => st.bind p.1 p.2) s1).values c = .i32 k := by
          rw [foldl_bind_keep res s1 c hres, exec_values_ran i s s1 ev res hi]
          exact hv
        exact ih (fun j hj => hdef j (List.mem_cons_of_mem _ hj)) _ _ s' tr' hv1 h
