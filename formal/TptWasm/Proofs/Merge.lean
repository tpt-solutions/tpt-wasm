import TptWasm.Merge

/-!
# Block merging: the bodies run the same

This file proves the value half of block merging's soundness: running a block's body in
the state the merged function gives it computes what the original computes.

## The relation

The merged run and the original run can differ on the parameters of the absorbed block:
the original binds them, the merged run does not, and its renamed reads go to the edge's
arguments instead. `ReadsOk σ P sM sO` says the two states agree on every value the
predicate `P` selects, in the sense the renamed run needs: an original read of `x` finds in
the merged state the value at `σ x`. It also requires the same store and grants.

Two instances matter:

* `P = True` with `σ = substVal ps args`: inside the absorbed block, after its parameters
  are bound by the edge.
* `P = (· ∉ ps)` with `σ = id`: everywhere else. Outside the absorbed block nothing reads
  its parameters (the pass's guard), so the two runs agree on every value that is read.

## What is proved

* `exec_renamed`: one instruction, renamed, computes what the original computes.
* `runBodyR_renamed`: a whole body does. The relation is maintained across the results
  a body defines, which must be fresh: `σ` fixes each result and never names it.
* `body_renamed`: the absorbed block's renamed body, run from the predecessor's state,
  matches its own body run from the state the edge binds.
* `body_agree`: a body that reads no parameter of the absorbed block runs the same from
  two states that agree outside those parameters.

The whole-function theorem, which also needs the terminators and the fuel accounting,
is the next step.
-/

namespace TptWasm
namespace IR

/-- The two states agree on every value `P` selects, once `σ` is applied to the merged
side, and they hold the same store and grants. -/
def ReadsOk (σ : ValueId → ValueId) (P : ValueId → Prop) (sM sO : State) : Prop :=
  sM.store = sO.store ∧ sM.grants = sO.grants ∧ ∀ x, P x → sO.values x = sM.values (σ x)

/-! ## Facts about the renaming -/

theorem substVal_not_mem (ps as : List ValueId) (x : ValueId) (h : x ∉ ps) :
    substVal ps as x = x := by
  induction ps generalizing as with
  | nil => cases as <;> rfl
  | cons p ps ih =>
    cases as with
    | nil => rfl
    | cons a as =>
      have hxp : x ≠ p := fun e => h (e ▸ List.mem_cons_self)
      have hxps : x ∉ ps := fun e => h (List.mem_cons_of_mem _ e)
      simp only [substVal, hxp, ↓reduceIte]
      exact ih as hxps

theorem substVal_mem (ps as : List ValueId) (x a : ValueId) (hnd : ps.Nodup)
    (hz : (x, a) ∈ List.zip ps as) : substVal ps as x = a := by
  induction ps generalizing as with
  | nil => simp at hz
  | cons p ps ih =>
    cases as with
    | nil => simp at hz
    | cons a' as =>
      rw [List.zip_cons_cons] at hz
      rcases List.mem_cons.mp hz with heq | htail
      · obtain ⟨rfl, rfl⟩ := Prod.mk.inj heq
        simp [substVal]
      · have hxp : x ≠ p := by
          intro e
          subst e
          exact (List.nodup_cons.mp hnd).1 (List.of_mem_zip htail).1
        simp only [substVal, hxp, ↓reduceIte]
        exact ih as (List.nodup_cons.mp hnd).2 htail

theorem substVal_in_args (ps as : List ValueId) (x : ValueId) (hlen : ps.length = as.length)
    (hx : x ∈ ps) : substVal ps as x ∈ as := by
  induction ps generalizing as with
  | nil => simp at hx
  | cons p ps ih =>
    cases as with
    | nil => simp at hlen
    | cons a as =>
      simp only [List.length_cons, Nat.add_right_cancel_iff] at hlen
      rcases List.mem_cons.mp hx with heq | htail
      · subst heq
        simp [substVal]
      · simp only [substVal]
        split
        · exact List.mem_cons_self
        · exact List.mem_cons_of_mem _ (ih as hlen htail)

/-- A renaming that fixes a fresh result and never names it. -/
theorem fresh_of (ps as : List ValueId) (hlen : ps.length = as.length) (r : ValueId)
    (hps : r ∉ ps) (has : r ∉ as) :
    substVal ps as r = r ∧ ∀ x, x ≠ r → substVal ps as x ≠ r := by
  refine ⟨substVal_not_mem ps as r hps, fun x hxr hsub => ?_⟩
  by_cases hxp : x ∈ ps
  · exact has (hsub ▸ substVal_in_args ps as x hlen hxp)
  · rw [substVal_not_mem ps as x hxp] at hsub
    exact hxr hsub

/-- Every parameter has a pair in the zip with the list it is zipped against. -/
theorem zip_of_mem (ps as : List ValueId) (hl : ps.length = as.length) (x : ValueId)
    (hx : x ∈ ps) : ∃ a, (x, a) ∈ List.zip ps as := by
  induction ps generalizing as with
  | nil => simp at hx
  | cons p ps ih =>
    cases as with
    | nil => simp at hl
    | cons a as =>
      simp only [List.length_cons, Nat.add_right_cancel_iff] at hl
      rcases List.mem_cons.mp hx with heq | htail
      · subst heq
        exact ⟨a, by rw [List.zip_cons_cons]; exact List.mem_cons_self⟩
      · obtain ⟨b, hb⟩ := ih as hl htail
        exact ⟨b, by rw [List.zip_cons_cons]; exact List.mem_cons_of_mem _ hb⟩

/-- Mapping the values of a zip keeps its pairs. -/
theorem zip_map_value (ps as : List ValueId) (f : ValueId → Value) (x a : ValueId)
    (h : (x, a) ∈ List.zip ps as) : (x, f a) ∈ List.zip ps (as.map f) := by
  induction ps generalizing as with
  | nil => simp at h
  | cons p ps ih =>
    cases as with
    | nil => simp at h
    | cons a' as =>
      rw [List.zip_cons_cons] at h
      rw [List.map_cons, List.zip_cons_cons]
      rcases List.mem_cons.mp h with heq | htail
      · obtain ⟨rfl, rfl⟩ := Prod.mk.inj heq
        exact List.mem_cons_self
      · exact List.mem_cons_of_mem _ (ih as htail)

/-- Two maps that agree on every element of a list agree on the list. -/
theorem map_congr_mem {α β : Type} (f g : α → β) :
    ∀ (l : List α), (∀ x ∈ l, f x = g x) → l.map f = l.map g
  | [], _ => rfl
  | a :: l, h => by
    rw [List.map_cons, List.map_cons, h a List.mem_cons_self,
      map_congr_mem f g l (fun x hx => h x (List.mem_cons_of_mem _ hx))]

/-- Renaming every operand by the identity changes nothing. -/
theorem renameInstr_self (i : Instr) : renameInstr (fun x => x) i = i := by
  cases i <;> simp [renameInstr]

/-! ## The state relation: binding, and a result defined in a body -/

theorem ReadsOk_bind {σ : ValueId → ValueId} {P : ValueId → Prop} {sM sO : State}
    (h : ReadsOk σ P sM sO) (r : ValueId) (v : Value) (hr : σ r = r)
    (hfree : ∀ x, x ≠ r → σ x ≠ r) :
    ReadsOk σ P (sM.bind r v) (sO.bind r v) := by
  obtain ⟨hst, hgr, hval⟩ := h
  refine ⟨hst, hgr, fun x hPx => ?_⟩
  by_cases hx : x = r
  · subst hx
    simp [State.bind, hr]
  · have hsx : σ x ≠ r := hfree x hx
    simp only [State.bind, hx, hsx, ↓reduceIte]
    exact hval x hPx

/-- Binding a list of results, each fresh, keeps the relation. -/
theorem ReadsOk_foldl_bind {σ : ValueId → ValueId} {P : ValueId → Prop} :
    ∀ (res : List (ValueId × Value)) (sM sO : State), ReadsOk σ P sM sO →
      (∀ p ∈ res, σ p.1 = p.1 ∧ ∀ x, x ≠ p.1 → σ x ≠ p.1) →
      ReadsOk σ P (res.foldl (fun st p => st.bind p.1 p.2) sM)
        (res.foldl (fun st p => st.bind p.1 p.2) sO) := by
  intro res
  induction res with
  | nil => intro sM sO h _; exact h
  | cons p rest ih =>
    intro sM sO h hfresh
    simp only [List.foldl_cons]
    apply ih
    · exact ReadsOk_bind h p.1 p.2 (hfresh p List.mem_cons_self).1
        (hfresh p List.mem_cons_self).2
    · intro q hq
      exact hfresh q (List.mem_cons_of_mem _ hq)

/-- A relation that keeps a store change on both sides. -/
theorem ReadsOk_with_store {σ : ValueId → ValueId} {P : ValueId → Prop} {sM sO : State}
    (h : ReadsOk σ P sM sO) (X : Store) :
    ReadsOk σ P { sM with store := X } { sO with store := X } :=
  ⟨rfl, h.2.1, h.2.2⟩

/-! ## Binding parameters -/

theorem bindAll_store (ids : List ValueId) (vs : List Value) (s : State) :
    (s.bindAll ids vs).store = s.store ∧ (s.bindAll ids vs).grants = s.grants := by
  induction ids generalizing s vs with
  | nil => exact ⟨rfl, rfl⟩
  | cons b bs ih =>
    cases vs with
    | nil => exact ⟨rfl, rfl⟩
    | cons v vs =>
      exact ih vs (s.bind b v)

theorem bindAll_keep (ids : List ValueId) (vs : List Value) (s : State) (p : ValueId)
    (hp : p ∉ ids) : (s.bindAll ids vs).values p = s.values p := by
  induction ids generalizing s vs with
  | nil => cases vs <;> rfl
  | cons b bs ih =>
    cases vs with
    | nil => rfl
    | cons v vs =>
      have hpb : p ≠ b := fun e => hp (e ▸ List.mem_cons_self)
      have hpbs : p ∉ bs := fun e => hp (List.mem_cons_of_mem _ e)
      rw [show (s.bindAll (b :: bs) (v :: vs)) = (s.bind b v).bindAll bs vs from rfl]
      rw [ih vs (s.bind b v) hpbs]
      simp [State.bind, hpb]

theorem bindAll_mem (ids : List ValueId) (vs : List Value) (s : State) (p : ValueId) (v : Value)
    (hnd : ids.Nodup) (hz : (p, v) ∈ List.zip ids vs) : (s.bindAll ids vs).values p = v := by
  induction ids generalizing s vs with
  | nil => simp at hz
  | cons b bs ih =>
    cases vs with
    | nil => simp at hz
    | cons v' vs =>
      rw [show (s.bindAll (b :: bs) (v' :: vs)) = (s.bind b v').bindAll bs vs from rfl]
      rw [List.zip_cons_cons] at hz
      rcases List.mem_cons.mp hz with heq | htail
      · obtain ⟨rfl, rfl⟩ := Prod.mk.inj heq
        rw [bindAll_keep bs vs _ p (List.nodup_cons.mp hnd).1]
        simp [State.bind]
      · exact ih vs (s.bind b v') (List.nodup_cons.mp hnd).2 htail

/-- Binding a block's parameters to an edge's arguments relates the bound state to the
state the edge leaves, through the renaming the binding stands for. -/
theorem ReadsOk_bindAll (ps args : List ValueId) (s : State) (hnd : ps.Nodup)
    (hlen : ps.length = args.length) :
    ReadsOk (substVal ps args) (fun _ => True) s (s.bindAll ps (args.map s.values)) := by
  refine ⟨(bindAll_store ps _ s).1.symm, (bindAll_store ps _ s).2.symm, fun x _ => ?_⟩
  by_cases hx : x ∈ ps
  · obtain ⟨a, ha⟩ := zip_of_mem ps args hlen x hx
    rw [substVal_mem ps args x a hnd ha,
      bindAll_mem ps (args.map s.values) s x (s.values a) hnd (zip_map_value ps args s.values x a ha)]
  · rw [substVal_not_mem ps args x hx, bindAll_keep ps _ s x hx]

/-! ## One instruction -/

/-- The result of a step in the merged run relates to the result of the original step. -/
def RelR (σ : ValueId → ValueId) (P : ValueId → Prop) :
    StepResult → StepResult → Prop
  | .trapped t', .trapped t => t' = t
  | .ran sM ev' res', .ran sO ev res => ev' = ev ∧ res' = res ∧ ReadsOk σ P sM sO
  | _, _ => False

theorem relR_trapped {σ : ValueId → ValueId} {P : ValueId → Prop} {r' : StepResult} {t : Trap}
    (h : RelR σ P r' (.trapped t)) : r' = .trapped t := by
  cases r' with
  | trapped t' => exact congrArg StepResult.trapped h
  | ran _ _ _ => exact h.elim

theorem relR_ran {σ : ValueId → ValueId} {P : ValueId → Prop} {r' : StepResult} {s : State}
    {ev : Option Event} {res : List (ValueId × Value)} (h : RelR σ P r' (.ran s ev res)) :
    ∃ sM, r' = .ran sM ev res ∧ ReadsOk σ P sM s := by
  cases r' with
  | trapped _ => exact h.elim
  | ran sM ev' res' =>
    obtain ⟨he, hr, hok⟩ := h
    exact ⟨sM, by rw [he, hr], hok⟩

/-- A renamed instruction, run in the merged state, produces what the original instruction
produces in the original state, provided every value it reads is selected by `P`. -/
theorem exec_renamed (σ : ValueId → ValueId) (P : ValueId → Prop) (i : Instr) (sM sO : State)
    (h : ReadsOk σ P sM sO) (hP : ∀ x ∈ usedBy i, P x) :
    RelR σ P (execInstr (renameInstr σ i) sM) (execInstr i sO) := by
  obtain ⟨hst, hgr, hval⟩ := h
  cases i with
  | constI32 r v => exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | constI64 r v => exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | binI32 r op l rr =>
    have h1 := hval l (hP l (by simp [usedBy]))
    have h2 := hval rr (hP rr (by simp [usedBy]))
    simp only [execInstr, renameInstr, h1, h2]
    exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | binI64 r op l rr =>
    have h1 := hval l (hP l (by simp [usedBy]))
    have h2 := hval rr (hP rr (by simp [usedBy]))
    simp only [execInstr, renameInstr, h1, h2]
    exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | cmpI32 r op l rr =>
    have h1 := hval l (hP l (by simp [usedBy]))
    have h2 := hval rr (hP rr (by simp [usedBy]))
    simp only [execInstr, renameInstr, h1, h2]
    exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | load r m a w =>
    have h1 := hval a (hP a (by simp [usedBy]))
    simp only [execInstr, renameInstr, h1, ← hst]
    split
    · split
      · exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
      · rfl
    · rfl
  | store m a v w =>
    have h1 := hval a (hP a (by simp [usedBy]))
    have h2 := hval v (hP v (by simp [usedBy]))
    simp only [execInstr, renameInstr, h1, h2, ← hst]
    split
    · split
      · exact ⟨rfl, rfl, ReadsOk_with_store ⟨hst, hgr, hval⟩ _⟩
      · rfl
    · rfl
  | globalGet r g =>
    simp only [execInstr, renameInstr, ← hst]
    split
    · exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
    · exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | globalSet g v =>
    have h1 := hval v (hP v (by simp [usedBy]))
    simp only [execInstr, renameInstr, h1, ← hst]
    split
    · exact ⟨rfl, rfl, ReadsOk_with_store ⟨hst, hgr, hval⟩ _⟩
    · exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | call index args results =>
    exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | callHost name granted args results =>
    have hargs : (args.map σ).map sM.values = args.map sO.values := by
      rw [List.map_map]
      exact (map_congr_mem _ _ args (fun x hx => (hval x (hP x (by simp [usedBy, hx]))).symm))
    simp only [execInstr, renameInstr, hargs]
    cases granted <;> exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩
  | drop v =>
    simp only [execInstr, renameInstr]
    exact ⟨rfl, rfl, ⟨hst, hgr, hval⟩⟩

/-- What a successful step defines is only what its instruction declares. -/
theorem ran_results_defined (i : Instr) (s s' : State) (ev : Option Event)
    (res : List (ValueId × Value)) (h : execInstr i s = .ran s' ev res) :
    ∀ p ∈ res, p.1 ∈ definedBy i := by
  intro p hp
  cases i <;> simp only [execInstr] at h <;> (repeat split at h) <;>
    (try obtain ⟨-, -, rfl⟩ := h) <;> simp_all [definedBy]

/-- The relation between two body results. A trap must be the same trap at the same
trace; a normal finish must have the same trace and related states. -/
def RelBody (σ : ValueId → ValueId) (P : ValueId → Prop) :
    Sum (List Event × Trap) (State × List Event) → Sum (List Event × Trap) (State × List Event) →
      Prop
  | .inl (t1, x1), .inl (t2, x2) => t1 = t2 ∧ x1 = x2
  | .inr (s1, t1), .inr (s2, t2) => t1 = t2 ∧ ReadsOk σ P s1 s2
  | _, _ => False

/-- A renamed body, run from the merged state, relates to the original body run from the
original state, provided the body's reads are selected by `P` and each result it defines
is fresh under the renaming. -/
theorem runBodyR_renamed (σ : ValueId → ValueId) (P : ValueId → Prop) :
    ∀ (B : List Instr) (sM sO : State) (tr : List Event),
      (∀ i ∈ B, ∀ x ∈ usedBy i, P x) → ReadsOk σ P sM sO →
      (∀ i ∈ B, ∀ r ∈ definedBy i, σ r = r ∧ ∀ x, x ≠ r → σ x ≠ r) →
      RelBody σ P (runBodyR (B.map (renameInstr σ)) sM tr) (runBodyR B sO tr) := by
  intro B
  induction B with
  | nil =>
    intro sM sO tr _ h _
    simp only [List.map_nil, runBodyR, RelBody]
    exact ⟨trivial, h⟩
  | cons i rest ih =>
    intro sM sO tr hreads h hfr
    have hi := exec_renamed σ P i sM sO h (hreads i List.mem_cons_self)
    have hfr_i := hfr i List.mem_cons_self
    have hreads_rest : ∀ j ∈ rest, ∀ x ∈ usedBy j, P x :=
      fun j hj => hreads j (List.mem_cons_of_mem _ hj)
    have hfr_rest : ∀ j ∈ rest, ∀ r ∈ definedBy j, σ r = r ∧ ∀ x, x ≠ r → σ x ≠ r :=
      fun j hj => hfr j (List.mem_cons_of_mem _ hj)
    rw [List.map_cons]
    simp only [runBodyR]
    cases hO : execInstr i sO with
    | trapped t =>
      rw [hO] at hi
      have hM := relR_trapped hi
      rw [hM]
      dsimp only
      exact ⟨rfl, rfl⟩
    | ran s' ev res =>
      have hi' := hi
      rw [hO] at hi'
      obtain ⟨sM', hM, hok⟩ := relR_ran hi'
      rw [hM]
      dsimp only
      have hfres : ∀ p ∈ res, σ p.1 = p.1 ∧ ∀ x, x ≠ p.1 → σ x ≠ p.1 :=
        fun p hp => hfr_i p.1 (ran_results_defined i sO s' ev res hO p hp)
      exact ih _ _ _ hreads_rest (ReadsOk_foldl_bind res sM' s' hok hfres) hfr_rest

/-- The renamed body of a block, run from the predecessor's state, relates to the block's
own body run from the state its edge binds. -/
theorem body_renamed (tb : Block) (args : List ValueId) (s : State) (tr : List Event)
    (hnd : tb.params.Nodup) (hlen : tb.params.length = args.length)
    (hdef : ∀ i ∈ tb.instrs, ∀ r ∈ definedBy i, r ∉ tb.params ∧ r ∉ args) :
    RelBody (substVal tb.params args) (fun _ => True)
      (runBodyR (tb.instrs.map (renameInstr (substVal tb.params args))) s tr)
      (runBodyR tb.instrs (s.bindAll tb.params (args.map s.values)) tr) := by
  apply runBodyR_renamed
  · intro i _ x _
    trivial
  · exact ReadsOk_bindAll tb.params args s hnd hlen
  · intro i hi r hr
    exact fresh_of tb.params args hlen r (hdef i hi r hr).1 (hdef i hi r hr).2

/-- The phase before the absorbed block is entered. A body that reads no parameter of the
absorbed block runs the same from two states that agree outside those parameters, and
the states it leaves agree in the same way. -/
theorem body_agree (ps : List ValueId) (B : List Instr) (sM sO : State) (tr : List Event)
    (hreads : ∀ i ∈ B, ∀ x ∈ usedBy i, x ∉ ps)
    (h : ReadsOk (fun x => x) (fun x => x ∉ ps) sM sO) :
    RelBody (fun x => x) (fun x => x ∉ ps) (runBodyR B sM tr) (runBodyR B sO tr) := by
  have key := runBodyR_renamed (fun x => x) (fun x => x ∉ ps) B sM sO tr
    (fun i hi x hx => hreads i hi x hx) h
    (fun _ _ r _ => ⟨rfl, fun _ hx => hx⟩)
  have hmap : B.map (renameInstr (fun x => x)) = B := by
    rw [map_congr_mem (renameInstr (fun x => x)) id B (fun i _ => renameInstr_self i)]
    exact List.map_id B
  rw [hmap] at key
  exact key

/-! ## Block lookup in the merged function -/

theorem mapBlock_id (from_ : BlockId) (tb : Block) (args : List ValueId) (b : Block) :
    (mapBlock from_ tb args b).id = b.id := by
  unfold mapBlock
  split <;> rfl

theorem findBlock_filter_map (l : List Block) (to from_ : BlockId) (tb : Block)
    (args : List ValueId) (id : BlockId) (hid : id ≠ to) :
    findBlock ((l.filter (fun b => decide (b.id ≠ to))).map (mapBlock from_ tb args)) id =
      (findBlock l id).map (mapBlock from_ tb args) := by
  induction l with
  | nil => rfl
  | cons b rest ih =>
    unfold findBlock at ih ⊢
    by_cases hbt : b.id = to
    · have hf : (b :: rest).filter (fun c => decide (c.id ≠ to)) =
          rest.filter (fun c => decide (c.id ≠ to)) := by
        simp [hbt]
      rw [hf, ih]
      have hne : (b.id == id) = false := by
        simp [hbt, Ne.symm hid]
      simp [hne]
    · have hf : (b :: rest).filter (fun c => decide (c.id ≠ to)) =
          b :: rest.filter (fun c => decide (c.id ≠ to)) := by
        simp [hbt]
      rw [hf]
      simp only [List.map_cons, List.find?_cons, mapBlock_id]
      cases hb : (b.id == id) with
      | true => rfl
      | false => simpa using ih

theorem findBlock_filter_map_to (l : List Block) (to from_ : BlockId) (tb : Block)
    (args : List ValueId) :
    findBlock ((l.filter (fun b => decide (b.id ≠ to))).map (mapBlock from_ tb args)) to = none := by
  induction l with
  | nil => rfl
  | cons b rest ih =>
    unfold findBlock at ih ⊢
    by_cases hbt : b.id = to
    · have hf : (b :: rest).filter (fun c => decide (c.id ≠ to)) =
          rest.filter (fun c => decide (c.id ≠ to)) := by
        simp [hbt]
      rw [hf]
      exact ih
    · have hf : (b :: rest).filter (fun c => decide (c.id ≠ to)) =
          b :: rest.filter (fun c => decide (c.id ≠ to)) := by
        simp [hbt]
      rw [hf]
      have hne : (b.id == to) = false := by simp [hbt]
      simp only [List.map_cons, List.find?_cons, mapBlock_id, hne]
      exact ih

theorem findBlock_mergeFn_ne (f : Function) (from_ to : BlockId) (tb : Block)
    (args : List ValueId) (id : BlockId) (hid : id ≠ to) :
    findBlock (mergeFn f from_ to tb args).blocks id =
      (findBlock f.blocks id).map (mapBlock from_ tb args) :=
  findBlock_filter_map f.blocks to from_ tb args id hid

theorem findBlock_mergeFn_to (f : Function) (from_ to : BlockId) (tb : Block)
    (args : List ValueId) :
    findBlock (mergeFn f from_ to tb args).blocks to = none :=
  findBlock_filter_map_to f.blocks to from_ tb args

end IR
end TptWasm
