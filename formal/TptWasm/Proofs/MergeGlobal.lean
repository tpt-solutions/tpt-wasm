import TptWasm.Proofs.Merge
import TptWasm.Proofs.Threading

/-!
# Block merging: the whole function

The theorem `merge_behaviour`: when `from` absorbs `to`, its only predecessor, the merged
function behaves as the original. A non-exhausted result is reached by the original
function with some fuel exactly when it is reached by the merged one.

The proof follows `Threading`: a forward simulation (the original's runs are the merged
function's runs) and a backward one, each by induction on fuel. What is new is the state:
the original binds the absorbed block's parameters and the merged run does not. Before the
absorbed block, the two states agree outside those parameters (`Rel`). Inside it, the
renamed body agrees with the original body (`Proofs.Merge`). After it, the states agree
outside the parameters again, because nothing outside the absorbed block reads them.

The hypotheses are the pass's own conditions, bundled in `MergeCtx`.
-/

namespace TptWasm
namespace IR

/-- The states agree outside the absorbed block's parameters `ps`. -/
abbrev Rel (ps : List ValueId) (sM sO : State) : Prop :=
  ReadsOk (fun x => x) (fun x => x ∉ ps) sM sO

/-- The conditions the merge relies on. The pass establishes each of them before it
merges `to` into `from`. -/
structure MergeCtx (f : Function) (from_ to : BlockId) (fb tb : Block) (args : List ValueId) :
    Prop where
  hfb : findBlock f.blocks from_ = some fb
  hfbterm : fb.terminator = .branch to args
  htb : findBlock f.blocks to = some tb
  hunique : ∀ X ∈ f.blocks, X.id = from_ → X = fb
  hnd : tb.params.Nodup
  hlen : tb.params.length = args.length
  hdef : ∀ i ∈ tb.instrs, ∀ r ∈ definedBy i, r ∉ tb.params ∧ r ∉ args
  hpred : ∀ X ∈ f.blocks, X.id ≠ from_ → ∀ p ∈ X.terminator.targets, p.1 ≠ to
  hlocal : ∀ X ∈ f.blocks, X.id ≠ to → ∀ i ∈ X.instrs, ∀ x ∈ usedBy i, x ∉ tb.params
  hlocalT : ∀ X ∈ f.blocks, X.id ≠ to → ∀ x ∈ X.terminator.reads, x ∉ tb.params
  hndAll : ∀ X ∈ f.blocks, X.params.Nodup
  harity : ∀ X ∈ f.blocks, ∀ p ∈ X.terminator.targets, ∀ Y,
    findBlock f.blocks p.1 = some Y → Y.params.length = p.2.length
  hft : from_ ≠ to
  hentry : to ≠ f.entry

/-! ## Small facts -/

theorem zip_exists {α β : Type} : ∀ (ps : List α) (vs : List β), ps.length = vs.length →
    ∀ x ∈ ps, ∃ v, (x, v) ∈ List.zip ps vs
  | [], [], _, x, hx => by simp at hx
  | [], _ :: _, hl, _, _ => by simp at hl
  | _ :: _, [], hl, _, _ => by simp at hl
  | p :: ps, v :: vs, hl, x, hx => by
    simp only [List.length_cons, Nat.add_right_cancel_iff] at hl
    rcases List.mem_cons.mp hx with heq | htail
    · subst heq
      exact ⟨v, by rw [List.zip_cons_cons]; exact List.mem_cons_self⟩
    · obtain ⟨w, hw⟩ := zip_exists ps vs hl x htail
      exact ⟨w, by rw [List.zip_cons_cons]; exact List.mem_cons_of_mem _ hw⟩

theorem map_self_list (l : List ValueId) : l.map (fun x => x) = l := by
  induction l with
  | nil => rfl
  | cons a l ih => simp [ih]

theorem renameTerm_self (T : Terminator) : renameTerm (fun x => x) T = T := by
  cases T with
  | branch t args => simp [renameTerm, map_self_list]
  | condBranch c a b =>
    obtain ⟨t1, a1⟩ := a
    obtain ⟨t2, a2⟩ := b
    simp [renameTerm, map_self_list]
  | ret values => simp [renameTerm, map_self_list]
  | trap t => rfl
  | unreachable => rfl

theorem mapBlock_params (from_ : BlockId) (tb : Block) (args : List ValueId) (X : Block) :
    (mapBlock from_ tb args X).params = X.params := by
  unfold mapBlock
  split <;> rfl

theorem target_args_reads (T : Terminator) (p : BlockId × List ValueId) (x : ValueId)
    (hp : p ∈ T.targets) (hx : x ∈ p.2) : x ∈ T.reads := by
  cases T with
  | branch t a =>
    simp only [Terminator.targets, List.mem_singleton] at hp
    subst hp
    simpa [Terminator.reads] using hx
  | condBranch c a b =>
    obtain ⟨t1, a1⟩ := a
    obtain ⟨t2, a2⟩ := b
    simp only [Terminator.targets, List.mem_cons] at hp
    rcases hp with rfl | rfl | h'
    · simp only [Terminator.reads] at hx ⊢
      simp [hx]
    · simp only [Terminator.reads] at hx ⊢
      simp [hx]
    · simp at h'
  | ret values => simp [Terminator.targets] at hp
  | trap t => simp [Terminator.targets] at hp
  | unreachable => simp [Terminator.targets] at hp

/-- Binding the same values to the same parameters keeps the states related outside the
absorbed block's parameters. -/
theorem rel_bind (ps tp : List ValueId) (sM sO : State) (vM vO : List Value)
    (hrel : Rel ps sM sO) (hnd : tp.Nodup) (hl : tp.length = vM.length) (hv : vM = vO) :
    Rel ps (sM.bindAll tp vM) (sO.bindAll tp vO) := by
  obtain ⟨hst, hgr, hval⟩ := hrel
  refine ⟨?_, ?_, fun x hx => ?_⟩
  · rw [(bindAll_store tp vM sM).1, (bindAll_store tp vO sO).1]; exact hst
  · rw [(bindAll_store tp vM sM).2, (bindAll_store tp vO sO).2]; exact hgr
  · show (sO.bindAll tp vO).values x = (sM.bindAll tp vM).values x
    by_cases hxt : x ∈ tp
    · obtain ⟨v, hz⟩ := zip_exists tp vM hl x hxt
      have hz' : (x, v) ∈ List.zip tp vO := by rw [← hv]; exact hz
      rw [bindAll_mem tp vM sM x v hnd hz, bindAll_mem tp vO sO x v hnd hz']
    · rw [bindAll_keep tp vM sM x hxt, bindAll_keep tp vO sO x hxt]
      exact hval x hx

/-- A relation that holds after the renamed run's states are bound. -/
theorem rel_of_ok (ps args : List ValueId) (sM sO : State)
    (h : ReadsOk (substVal ps args) (fun _ => True) sM sO) : Rel ps sM sO := by
  obtain ⟨hst, hgr, hval⟩ := h
  refine ⟨hst, hgr, fun x hx => ?_⟩
  show sO.values x = sM.values x
  rw [hval x trivial, substVal_not_mem ps args x hx]

/-- The merged run's arguments read the same values as the original's, when the
renaming relates the states. -/
theorem lists_eq_gen (σ : ValueId → ValueId) (P : ValueId → Prop) (sM sO : State)
    (h : ReadsOk σ P sM sO) (a : List ValueId) (hP : ∀ x ∈ a, P x) :
    (a.map σ).map sM.values = a.map sO.values := by
  rw [List.map_map]
  exact map_congr_mem _ _ a (fun x hx => (h.2.2 x (hP x hx)).symm)

/-- Arguments outside the absorbed block's parameters read the same values in both
states. -/
theorem lists_eq_rel (ps : List ValueId) (sM sO : State) (h : Rel ps sM sO)
    (a : List ValueId) (hP : ∀ x ∈ a, x ∉ ps) :
    a.map sM.values = a.map sO.values :=
  map_congr_mem _ _ a (fun x hx => (h.2.2 x (hP x hx)).symm)

/-- The bound state of an absorbed block, next to the merged state it came from. -/
theorem ok_bind_params (ps args : List ValueId) (sM sO : State) (hrel : Rel ps sM sO)
    (hnd : ps.Nodup) (hloc : ∀ x ∈ args, x ∉ ps) (hl : ps.length = args.length) :
    ReadsOk (substVal ps args) (fun _ => True) sM (sO.bindAll ps (args.map sO.values)) := by
  refine ⟨?_, ?_, fun x _ => ?_⟩
  · rw [(bindAll_store ps _ sO).1]; exact hrel.1
  · rw [(bindAll_store ps _ sO).2]; exact hrel.2.1
  · by_cases hx : x ∈ ps
    · obtain ⟨a, ha⟩ := zip_of_mem ps args hl x hx
      rw [substVal_mem ps args x a hnd ha,
        bindAll_mem ps (args.map sO.values) sO x (sO.values a) hnd
          (zip_map_value ps args sO.values x a ha)]
      exact hrel.2.2 a (hloc a (List.of_mem_zip ha).2)
    · rw [substVal_not_mem ps args x hx, bindAll_keep ps _ sO x hx]
      exact hrel.2.2 x hx

/-- Running a body then another is running their concatenation. -/
theorem runBodyR_append (xs ys : List Instr) (s : State) (tr : List Event) :
    runBodyR (xs ++ ys) s tr =
      match runBodyR xs s tr with
      | .inl p => .inl p
      | .inr (s1, t1) => runBodyR ys s1 t1 := by
  induction xs generalizing s tr with
  | nil => rfl
  | cons i rest ih =>
    simp only [List.cons_append, runBodyR]
    cases execInstr i s with
    | trapped t => rfl
    | ran s' ev res =>
      simp only []
      exact ih _ _

/-! ## Lookup at the run level -/

/-- A run from an entry-like lookup in the original, at a block the merge does not
touch, is the run of the same lookup in the merged function, with states related. -/
theorem runAt_fwd_rel (f : Function) (from_ to : BlockId) (fb tb : Block)
    (args : List ValueId) (C : MergeCtx f from_ to fb tb args) (k : Nat) (t : BlockId)
    (ht : t ≠ to) (aM aO : List ValueId) (sM sO : State) (tr : List Event) (r : Result)
    (hne : NotExhausted r) (hrel : Rel tb.params sM sO)
    (hv : aM.map sM.values = aO.map sO.values)
    (hl : ∀ Y, findBlock f.blocks t = some Y → Y.params.length = aO.length)
    (IH : ∀ X ∈ f.blocks, X.id ≠ to → ∀ (s1 s2 : State) (tr' : List Event) (r' : Result),
      Rel tb.params s1 s2 → NotExhausted r' →
        runBlocks f.blocks k X s2 tr' = r' →
        runBlocks (mergeFn f from_ to tb args).blocks k (mapBlock from_ tb args X) s1 tr' = r')
    (hrun : runAt f.blocks k t aO sO tr = r) :
    runAt (mergeFn f from_ to tb args).blocks k t aM sM tr = r := by
  rcases Option.eq_none_or_eq_some (findBlock f.blocks t) with hf | ⟨X, hf⟩
  · rw [runAt_none hf] at hrun
    rw [runAt_none (by rw [findBlock_mergeFn_ne f from_ to tb args t ht, hf]; rfl)]
    exact hrun
  · rw [runAt_some hf] at hrun
    rw [runAt_some (c := mapBlock from_ tb args X)
      (by rw [findBlock_mergeFn_ne f from_ to tb args t ht, hf]; rfl), mapBlock_params]
    have hXt : X.id ≠ to := by rw [findBlock_id hf]; exact ht
    have hlen : X.params.length = (aM.map sM.values).length := by
      have hlm : aM.length = aO.length := by
        have := congrArg List.length hv
        simpa using this
      simp only [List.length_map]
      rw [hlm]
      exact hl X hf
    exact IH X (findBlock_mem hf) hXt _ _ tr r
      (rel_bind tb.params X.params sM sO _ _ hrel (C.hndAll X (findBlock_mem hf)) hlen hv)
      hne hrun

theorem runAt_bwd_rel (f : Function) (from_ to : BlockId) (fb tb : Block)
    (args : List ValueId) (C : MergeCtx f from_ to fb tb args) (k : Nat) (t : BlockId)
    (ht : t ≠ to) (aM aO : List ValueId) (sM sO : State) (tr : List Event) (r : Result)
    (hne : NotExhausted r) (hrel : Rel tb.params sM sO)
    (hv : aM.map sM.values = aO.map sO.values)
    (hl : ∀ Y, findBlock f.blocks t = some Y → Y.params.length = aO.length)
    (IH : ∀ X ∈ f.blocks, X.id ≠ to → ∀ (s1 s2 : State) (tr' : List Event) (r' : Result),
      Rel tb.params s1 s2 → NotExhausted r' →
        runBlocks (mergeFn f from_ to tb args).blocks k (mapBlock from_ tb args X) s1 tr' = r' →
        ∃ m, runBlocks f.blocks m X s2 tr' = r')
    (hrun : runAt (mergeFn f from_ to tb args).blocks k t aM sM tr = r) :
    ∃ m, runAt f.blocks m t aO sO tr = r := by
  rcases Option.eq_none_or_eq_some (findBlock f.blocks t) with hf | ⟨X, hf⟩
  · have hMn : findBlock (mergeFn f from_ to tb args).blocks t = none := by
      rw [findBlock_mergeFn_ne f from_ to tb args t ht, hf]; rfl
    rw [runAt_none hMn] at hrun
    exact ⟨0, by rw [runAt_none hf]; exact hrun⟩
  · have hMs : findBlock (mergeFn f from_ to tb args).blocks t =
        some (mapBlock from_ tb args X) := by
      rw [findBlock_mergeFn_ne f from_ to tb args t ht, hf]; rfl
    rw [runAt_some hMs, mapBlock_params] at hrun
    have hXt : X.id ≠ to := by rw [findBlock_id hf]; exact ht
    have hlen : X.params.length = (aM.map sM.values).length := by
      have hlm : aM.length = aO.length := by
        have := congrArg List.length hv
        simpa using this
      simp only [List.length_map]
      rw [hlm]
      exact hl X hf
    obtain ⟨m, hm⟩ := IH X (findBlock_mem hf) hXt _ _ tr r
      (rel_bind tb.params X.params sM sO _ _ hrel (C.hndAll X (findBlock_mem hf)) hlen hv)
      hne hrun
    exact ⟨m, by rw [runAt_some hf]; exact hm⟩

/-! ## Terminators -/

/-- A terminator, run in the merged state with its arguments renamed, gives what the
original terminator gives. -/
theorem term_fwd_gen (σ : ValueId → ValueId) (P : ValueId → Prop) (G F : List Block)
    (sM sO : State) (tr : List Event) (T : Terminator) (k : Nat) (r : Result)
    (hne : NotExhausted r) (hread : ∀ x ∈ T.reads, P x) (hr : ReadsOk σ P sM sO)
    (hrun : ∀ p ∈ T.targets, ∀ r'', NotExhausted r'' →
      runAt F k p.1 p.2 sO tr = r'' → runAt G k p.1 (p.2.map σ) sM tr = r'')
    (h : termResult F k T sO tr = r) :
    termResult G k (renameTerm σ T) sM tr = r := by
  cases T with
  | ret values =>
    have hv := lists_eq_gen σ P sM sO hr values hread
    simp only [termResult, renameTerm] at h ⊢
    rw [hv]
    exact h
  | trap t => exact h
  | unreachable => exact h
  | branch t a =>
    exact hrun (t, a) (by simp [Terminator.targets]) r hne h
  | condBranch c a b =>
    obtain ⟨t1, a1⟩ := a
    obtain ⟨t2, a2⟩ := b
    have hc : intPayload (sM.values (σ c)) = intPayload (sO.values c) := by
      rw [hr.2.2 c (hread c (by simp [Terminator.reads]))]
    simp only [termResult, renameTerm] at h ⊢
    rw [← hc] at h
    by_cases hz : intPayload (sM.values (σ c)) = 0
    · simp only [hz, ↓reduceIte] at h ⊢
      exact hrun (t2, a2) (by simp [Terminator.targets]) r hne h
    · simp only [hz, ↓reduceIte] at h ⊢
      exact hrun (t1, a1) (by simp [Terminator.targets]) r hne h

/-- The backward direction of `term_fwd_gen`. -/
theorem term_bwd_gen (σ : ValueId → ValueId) (P : ValueId → Prop) (G F : List Block)
    (sM sO : State) (tr : List Event) (T : Terminator) (k : Nat) (r : Result)
    (hne : NotExhausted r) (hread : ∀ x ∈ T.reads, P x) (hr : ReadsOk σ P sM sO)
    (hrun : ∀ p ∈ T.targets, ∀ r'', NotExhausted r'' →
      runAt G k p.1 (p.2.map σ) sM tr = r'' → ∃ m, runAt F m p.1 p.2 sO tr = r'')
    (h : termResult G k (renameTerm σ T) sM tr = r) :
    ∃ m, termResult F m T sO tr = r := by
  cases T with
  | ret values =>
    have hv := lists_eq_gen σ P sM sO hr values hread
    simp only [termResult, renameTerm] at h
    refine ⟨0, ?_⟩
    simp only [termResult]
    rw [← hv]
    exact h
  | trap t => exact ⟨0, h⟩
  | unreachable => exact ⟨0, h⟩
  | branch t a =>
    obtain ⟨m, hm⟩ := hrun (t, a) (by simp [Terminator.targets]) r hne h
    exact ⟨m, hm⟩
  | condBranch c a b =>
    obtain ⟨t1, a1⟩ := a
    obtain ⟨t2, a2⟩ := b
    have hc : intPayload (sM.values (σ c)) = intPayload (sO.values c) := by
      rw [hr.2.2 c (hread c (by simp [Terminator.reads]))]
    simp only [termResult, renameTerm] at h
    rw [hc] at h
    by_cases hz : intPayload (sO.values c) = 0
    · simp only [hz, ↓reduceIte] at h
      obtain ⟨m, hm⟩ := hrun (t2, a2) (by simp [Terminator.targets]) r hne h
      exact ⟨m, by simp only [termResult, hz, ↓reduceIte]; exact hm⟩
    · simp only [hz, ↓reduceIte] at h
      obtain ⟨m, hm⟩ := hrun (t1, a1) (by simp [Terminator.targets]) r hne h
      exact ⟨m, by simp only [termResult, hz, ↓reduceIte]; exact hm⟩
