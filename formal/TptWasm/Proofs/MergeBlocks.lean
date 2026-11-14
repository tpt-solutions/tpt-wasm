import TptWasm.Proofs.MergeGlobal

/-!
# Block merging: the block-level simulations

The forward and backward claims at each fuel, by induction on fuel, and the theorem
`merge_behaviour`. The absorbed block is the only case that changes the fuel; the others
follow the threading argument's shape, with states related by `Rel` rather than equal.
-/

namespace TptWasm
namespace IR

/-- The forward claim at fuel `j`: a block of the original, run from related states, gives
the same non-exhausted result when run in the merged function. -/
def FwdM (f : Function) (from_ to : BlockId) (tb : Block) (args : List ValueId) (j : Nat) :
    Prop :=
  ∀ X ∈ f.blocks, X.id ≠ to → ∀ (sM sO : State) (tr : List Event) (r : Result),
    Rel tb.params sM sO → NotExhausted r → runBlocks f.blocks j X sO tr = r →
      runBlocks (mergeFn f from_ to tb args).blocks j (mapBlock from_ tb args X) sM tr = r

/-- The backward claim at fuel `j`: a block of the merged function run from related states
is matched by a block of the original with some fuel. -/
def BackM (f : Function) (from_ to : BlockId) (tb : Block) (args : List ValueId) (j : Nat) :
    Prop :=
  ∀ X ∈ f.blocks, X.id ≠ to → ∀ (sM sO : State) (tr : List Event) (r : Result),
    Rel tb.params sM sO → NotExhausted r →
      runBlocks (mergeFn f from_ to tb args).blocks j (mapBlock from_ tb args X) sM tr = r →
        ∃ m, runBlocks f.blocks m X sO tr = r

theorem from_body_rel (f : Function) (from_ to : BlockId) (fb tb : Block) (args : List ValueId)
    (C : MergeCtx f from_ to fb tb args) (sM sO : State) (tr : List Event)
    (hrel : Rel tb.params sM sO) :
    RelBody (fun x => x) (fun x => x ∉ tb.params)
      (runBodyR fb.instrs sM tr) (runBodyR fb.instrs sO tr) :=
  body_agree tb.params fb.instrs sM sO tr
    (fun i hi x hx => C.hlocal fb (findBlock_mem C.hfb)
      (by rw [findBlock_id C.hfb]; exact C.hft) i hi x hx) hrel

/-- The forward step. -/
theorem fwd_step (f : Function) (from_ to : BlockId) (fb tb : Block) (args : List ValueId)
    (C : MergeCtx f from_ to fb tb args) (m : Nat)
    (ih : ∀ j, j ≤ m → FwdM f from_ to tb args j) :
    FwdM f from_ to tb args (m + 1) := by
  intro X hX hXt sM sO tr r hrel hne h
  have hfbid : fb.id = from_ := findBlock_id C.hfb
  have hfbmem : fb ∈ f.blocks := findBlock_mem C.hfb
  have hfbt : fb.id ≠ to := by rw [hfbid]; exact C.hft
  have htbmem : tb ∈ f.blocks := findBlock_mem C.htb
  have hargs : ∀ x ∈ args, x ∉ tb.params := fun x hx =>
    C.hlocalT fb hfbmem hfbt x (by rw [C.hfbterm]; exact hx)
  by_cases hfrom : X.id = from_
  · -- The absorbed block itself: `from`, which now also runs `to`.
    have hXfb : X = fb := C.hunique X hX hfrom
    rw [hXfb] at h ⊢
    have hmapX : mapBlock from_ tb args fb = mergeBlock fb tb args := by
      unfold mapBlock
      simp [hfbid]
    rw [hmapX, runBlocks_succ]
    rw [runBlocks_succ] at h
    dsimp only [mergeBlock]
    rw [runBodyR_append]
    have hb1 := from_body_rel f from_ to fb tb args C sM sO tr hrel
    generalize hA : runBodyR fb.instrs sO tr = resO at h hb1
    generalize hB : runBodyR fb.instrs sM tr = resM at hb1 ⊢
    rcases resO with ⟨tO, xO⟩ | ⟨sO1, trO1⟩ <;> rcases resM with ⟨tM, xM⟩ | ⟨sM1, trM1⟩
    · simp only [RelBody] at hb1
      obtain ⟨rfl, rfl⟩ := hb1
      exact h
    · simp only [RelBody] at hb1
    · simp only [RelBody] at hb1
    · simp only [RelBody] at hb1
      obtain ⟨htr, hrel1⟩ := hb1
      rw [htr]
      dsimp only at h ⊢
      rw [C.hfbterm] at h
      simp only [termResult] at h
      rw [runAt_some C.htb] at h
      cases m with
      | zero =>
        rw [runBlocks_zero] at h
        subst h
        simp [NotExhausted] at hne
      | succ k =>
        rw [runBlocks_succ] at h
        have hb2 := runBodyR_renamed (substVal tb.params args) (fun _ => True) tb.instrs sM1
          (sO1.bindAll tb.params (args.map sO1.values)) trO1 (fun _ _ _ _ => trivial)
          (ok_bind_params tb.params args sM1 sO1 hrel1 C.hnd hargs C.hlen)
          (fun i hi r hr => fresh_of tb.params args C.hlen r (C.hdef i hi r hr).1
            (C.hdef i hi r hr).2)
        generalize hD : runBodyR (tb.instrs.map (renameInstr (substVal tb.params args))) sM1 trO1
          = resD at hb2 ⊢
        generalize hC : runBodyR tb.instrs (sO1.bindAll tb.params (args.map sO1.values)) trO1
          = resC at hb2 h
        rcases resD with ⟨tD, xD⟩ | ⟨sM2, trM2⟩ <;> rcases resC with ⟨tC, xC⟩ | ⟨sO2, trO2⟩
        · simp only [RelBody] at hb2
          obtain ⟨rfl, rfl⟩ := hb2
          exact h
        · simp only [RelBody] at hb2
        · simp only [RelBody] at hb2
        · simp only [RelBody] at hb2
          obtain ⟨htr2, hok⟩ := hb2
          subst htr2
          dsimp only at h ⊢
          have hrun : ∀ p ∈ tb.terminator.targets, ∀ r'', NotExhausted r'' →
              runAt f.blocks k p.1 p.2 sO2 trM2 = r'' →
                runAt (mergeFn f from_ to tb args).blocks k p.1
                  (p.2.map (substVal tb.params args)) sM2 trM2 = r'' := by
            intro p hp r'' hne' h''
            exact runAt_fwd_rel f from_ to fb tb args C k p.1
              (C.hpred tb htbmem (by rw [findBlock_id C.htb]; exact Ne.symm C.hft) p hp)
              (p.2.map (substVal tb.params args)) p.2 sM2 sO2 trM2 r'' hne'
              (rel_of_ok _ _ sM2 sO2 hok)
              (lists_eq_gen (substVal tb.params args) (fun _ => True) sM2 sO2 hok p.2
                (fun _ _ => trivial))
              (C.harity tb htbmem p hp)
              (fun X' hX' hXt' s1 s2 tr' r' hr' hne'' h''' =>
                ih k (by omega) X' hX' hXt' s1 s2 tr' r' hr' hne'' h''')
              h''
          have hT := term_fwd_gen (substVal tb.params args) (fun _ => True)
            (mergeFn f from_ to tb args).blocks f.blocks sM2 sO2 trM2 tb.terminator k r hne
            (fun _ _ => trivial) hok hrun h
          exact termResult_mono (mergeFn f from_ to tb args).blocks k
            (renameTerm (substVal tb.params args) tb.terminator) sM2 trM2 r
            (fun id a s' tr' r' hne' h' d => runAt_mono_of' _ k id a s' tr' r' hne' h' d)
            hne hT 1
  · -- Any other block keeps its identity and its terminator.
    have hmapX : mapBlock from_ tb args X = X := by
      unfold mapBlock
      simp [hfrom]
    rw [hmapX, runBlocks_succ]
    rw [runBlocks_succ] at h
    have hb := body_agree tb.params X.instrs sM sO tr
      (fun i hi x hx => C.hlocal X hX hXt i hi x hx) hrel
    generalize hA : runBodyR X.instrs sO tr = resO at h hb
    generalize hB : runBodyR X.instrs sM tr = resM at hb ⊢
    rcases resO with ⟨tO, xO⟩ | ⟨sO1, trO1⟩ <;> rcases resM with ⟨tM, xM⟩ | ⟨sM1, trM1⟩
    · simp only [RelBody] at hb
      obtain ⟨rfl, rfl⟩ := hb
      exact h
    · simp only [RelBody] at hb
    · simp only [RelBody] at hb
    · simp only [RelBody] at hb
      obtain ⟨htr, hrel1⟩ := hb
      rw [htr]
      dsimp only at h ⊢
      have hrun : ∀ p ∈ X.terminator.targets, ∀ r'', NotExhausted r'' →
          runAt f.blocks m p.1 p.2 sO1 trO1 = r'' →
            runAt (mergeFn f from_ to tb args).blocks m p.1 (p.2.map (fun x => x)) sM1 trO1 = r'' := by
        intro p hp r'' hne' h''
        rw [map_self_list]
        exact runAt_fwd_rel f from_ to fb tb args C m p.1 (C.hpred X hX hfrom p hp) p.2 p.2 sM1 sO1
          trO1 r'' hne' hrel1
          (lists_eq_rel tb.params sM1 sO1 hrel1 p.2
            (fun x hx => C.hlocalT X hX hXt x (target_args_reads _ p x hp hx)))
          (C.harity X hX p hp)
          (fun X' hX' hXt' s1 s2 tr' r' hr' hne'' h''' =>
            ih m (Nat.le_refl m) X' hX' hXt' s1 s2 tr' r' hr' hne'' h''')
          h''
      have hT := term_fwd_gen (fun x => x) (fun x => x ∉ tb.params)
        (mergeFn f from_ to tb args).blocks f.blocks sM1 sO1 trO1 X.terminator m r hne
        (fun x hx => C.hlocalT X hX hXt x hx) hrel1 hrun h
      rw [renameTerm_self] at hT
      exact hT

/-- The backward step. -/
theorem bwd_step (f : Function) (from_ to : BlockId) (fb tb : Block) (args : List ValueId)
    (C : MergeCtx f from_ to fb tb args) (m : Nat) (ih : BackM f from_ to tb args m) :
    BackM f from_ to tb args (m + 1) := by
  intro X hX hXt sM sO tr r hrel hne h
  have hfbid : fb.id = from_ := findBlock_id C.hfb
  have hfbmem : fb ∈ f.blocks := findBlock_mem C.hfb
  have hfbt : fb.id ≠ to := by rw [hfbid]; exact C.hft
  have htbmem : tb ∈ f.blocks := findBlock_mem C.htb
  have hargs : ∀ x ∈ args, x ∉ tb.params := fun x hx =>
    C.hlocalT fb hfbmem hfbt x (by rw [C.hfbterm]; exact hx)
  by_cases hfrom : X.id = from_
  · have hXfb : X = fb := C.hunique X hX hfrom
    rw [hXfb] at h ⊢
    have hmapX : mapBlock from_ tb args fb = mergeBlock fb tb args := by
      unfold mapBlock
      simp [hfbid]
    rw [hmapX, runBlocks_succ] at h
    dsimp only [mergeBlock] at h
    rw [runBodyR_append] at h
    have hb1 := from_body_rel f from_ to fb tb args C sM sO tr hrel
    generalize hB : runBodyR fb.instrs sM tr = resM at h hb1
    generalize hA : runBodyR fb.instrs sO tr = resO at hb1
    rcases resM with ⟨tM, xM⟩ | ⟨sM1, trM1⟩ <;> rcases resO with ⟨tO, xO⟩ | ⟨sO1, trO1⟩
    · simp only [RelBody] at hb1
      obtain ⟨rfl, rfl⟩ := hb1
      refine ⟨m + 1 + 1, ?_⟩
      rw [runBlocks_succ, hA]
      exact h
    · simp only [RelBody] at hb1
    · simp only [RelBody] at hb1
    · simp only [RelBody] at hb1
      obtain ⟨htr, hrel1⟩ := hb1
      rw [htr] at h
      dsimp only at h
      have hb2 := runBodyR_renamed (substVal tb.params args) (fun _ => True) tb.instrs sM1
        (sO1.bindAll tb.params (args.map sO1.values)) trO1 (fun _ _ _ _ => trivial)
        (ok_bind_params tb.params args sM1 sO1 hrel1 C.hnd hargs C.hlen)
        (fun i hi r hr => fresh_of tb.params args C.hlen r (C.hdef i hi r hr).1
          (C.hdef i hi r hr).2)
      generalize hD : runBodyR (tb.instrs.map (renameInstr (substVal tb.params args))) sM1 trO1
        = resD at h hb2
      generalize hC : runBodyR tb.instrs (sO1.bindAll tb.params (args.map sO1.values)) trO1
        = resC at hb2
      rcases resD with ⟨tD, xD⟩ | ⟨sM2, trM2⟩ <;> rcases resC with ⟨tC, xC⟩ | ⟨sO2, trO2⟩
      · simp only [RelBody] at hb2
        obtain ⟨rfl, rfl⟩ := hb2
        refine ⟨m + 1 + 1, ?_⟩
        rw [runBlocks_succ, hA]
        dsimp only
        rw [C.hfbterm]
        simp only [termResult]
        rw [runAt_some C.htb, runBlocks_succ, hC]
        dsimp only
        exact h
      · simp only [RelBody] at hb2
      · simp only [RelBody] at hb2
      · simp only [RelBody] at hb2
        obtain ⟨htr2, hok⟩ := hb2
        subst htr2
        dsimp only at h
        have hrun : ∀ p ∈ tb.terminator.targets, ∀ r'', NotExhausted r'' →
            runAt (mergeFn f from_ to tb args).blocks m p.1
              (p.2.map (substVal tb.params args)) sM2 trM2 = r'' →
              ∃ m', runAt f.blocks m' p.1 p.2 sO2 trM2 = r'' := by
          intro p hp r'' hne' h''
          exact runAt_bwd_rel f from_ to fb tb args C m p.1
            (C.hpred tb htbmem (by rw [findBlock_id C.htb]; exact Ne.symm C.hft) p hp)
            (p.2.map (substVal tb.params args)) p.2 sM2 sO2 trM2 r'' hne'
            (rel_of_ok _ _ sM2 sO2 hok)
            (lists_eq_gen (substVal tb.params args) (fun _ => True) sM2 sO2 hok p.2
              (fun _ _ => trivial))
            (C.harity tb htbmem p hp) ih h''
        obtain ⟨m', hm'⟩ := term_bwd_gen (substVal tb.params args) (fun _ => True)
          (mergeFn f from_ to tb args).blocks f.blocks sM2 sO2 trM2 tb.terminator m r hne
          (fun _ _ => trivial) hok hrun h
        refine ⟨m' + 1 + 1, ?_⟩
        rw [runBlocks_succ, hA]
        dsimp only
        rw [C.hfbterm]
        simp only [termResult]
        rw [runAt_some C.htb, runBlocks_succ, hC]
        dsimp only
        exact hm'
  · have hmapX : mapBlock from_ tb args X = X := by
      unfold mapBlock
      simp [hfrom]
    rw [hmapX, runBlocks_succ] at h
    have hb := body_agree tb.params X.instrs sM sO tr
      (fun i hi x hx => C.hlocal X hX hXt i hi x hx) hrel
    generalize hB : runBodyR X.instrs sM tr = resM at h hb
    generalize hA : runBodyR X.instrs sO tr = resO at hb
    rcases resM with ⟨tM, xM⟩ | ⟨sM1, trM1⟩ <;> rcases resO with ⟨tO, xO⟩ | ⟨sO1, trO1⟩
    · simp only [RelBody] at hb
      obtain ⟨rfl, rfl⟩ := hb
      exact ⟨m + 1, by rw [runBlocks_succ, hA]; exact h⟩
    · simp only [RelBody] at hb
    · simp only [RelBody] at hb
    · simp only [RelBody] at hb
      obtain ⟨htr, hrel1⟩ := hb
      rw [htr] at h
      dsimp only at h
      have hrun : ∀ p ∈ X.terminator.targets, ∀ r'', NotExhausted r'' →
          runAt (mergeFn f from_ to tb args).blocks m p.1 (p.2.map (fun x => x)) sM1 trO1 = r'' →
            ∃ m', runAt f.blocks m' p.1 p.2 sO1 trO1 = r'' := by
        intro p hp r'' hne' h''
        rw [map_self_list] at h''
        exact runAt_bwd_rel f from_ to fb tb args C m p.1 (C.hpred X hX hfrom p hp) p.2 p.2 sM1 sO1
          trO1 r'' hne' hrel1
          (lists_eq_rel tb.params sM1 sO1 hrel1 p.2
            (fun x hx => C.hlocalT X hX hXt x (target_args_reads _ p x hp hx)))
          (C.harity X hX p hp) ih h''
      obtain ⟨m', hm'⟩ := term_bwd_gen (fun x => x) (fun x => x ∉ tb.params)
        (mergeFn f from_ to tb args).blocks f.blocks sM1 sO1 trO1 X.terminator m r hne
        (fun x hx => C.hlocalT X hX hXt x hx) hrel1 hrun (by rw [renameTerm_self]; exact h)
      exact ⟨m' + 1, by rw [runBlocks_succ, hA]; exact hm'⟩

/-- The forward claim at every fuel, by induction on fuel. -/
theorem fwd_all (f : Function) (from_ to : BlockId) (fb tb : Block) (args : List ValueId)
    (C : MergeCtx f from_ to fb tb args) :
    ∀ m j, j ≤ m → FwdM f from_ to tb args j := by
  intro m
  induction m with
  | zero =>
    intro j hj
    have hj0 : j = 0 := by omega
    subst hj0
    intro X _ _ sM sO tr r hrel hne h
    rw [runBlocks_zero] at h
    subst h
    simp [NotExhausted] at hne
  | succ m ih =>
    intro j hj
    rcases Nat.lt_or_ge j (m + 1) with hlt | hge
    · exact ih j (by omega)
    · have hj' : j = m + 1 := by omega
      subst hj'
      exact fwd_step f from_ to fb tb args C m ih

/-- The backward claim at every fuel, by induction on fuel. -/
theorem bwd_all (f : Function) (from_ to : BlockId) (fb tb : Block) (args : List ValueId)
    (C : MergeCtx f from_ to fb tb args) :
    ∀ m, BackM f from_ to tb args m := by
  intro m
  induction m with
  | zero =>
    intro X _ _ sM sO tr r hrel hne h
    rw [runBlocks_zero] at h
    subst h
    simp [NotExhausted] at hne
  | succ m ih => exact bwd_step f from_ to fb tb args C m ih

/-- The merge preserves behaviour: for every non-exhausted result, the original function
reaches it with some fuel exactly when the merged function does. -/
theorem merge_behaviour (f : Function) (from_ to : BlockId) (fb tb : Block) (args : List ValueId)
    (C : MergeCtx f from_ to fb tb args) (s : State) (r : Result) (hne : NotExhausted r) :
    (∃ n, runFunction f n s = r) ↔ (∃ n, runFunction (mergeFn f from_ to tb args) n s = r) := by
  have hrel : Rel tb.params s s := ⟨rfl, rfl, fun _ _ => rfl⟩
  have entry_eq : ∀ (blocks : List Block) (n : Nat) (X : Block), findBlock blocks f.entry = some X →
      runAt blocks n f.entry [] s [] = runBlocks blocks n X s [] := by
    intro blocks n X hX
    rw [runAt_some hX, List.map_nil, State.bindAll_nil_vals]
  have hMentry : findBlock (mergeFn f from_ to tb args).blocks f.entry =
      (findBlock f.blocks f.entry).map (mapBlock from_ tb args) :=
    findBlock_mergeFn_ne f from_ to tb args f.entry (Ne.symm C.hentry)
  have hMe : (mergeFn f from_ to tb args).entry = f.entry := rfl
  constructor
  · rintro ⟨n, h⟩
    rw [runFunction_eq_runAt] at h
    refine ⟨n, ?_⟩
    rw [runFunction_eq_runAt, hMe]
    rcases Option.eq_none_or_eq_some (findBlock f.blocks f.entry) with hf | ⟨X, hf⟩
    · have hMn : findBlock (mergeFn f from_ to tb args).blocks f.entry = none := by
        rw [hMentry, hf]; rfl
      rw [runAt_none hf] at h
      rw [runAt_none hMn]
      exact h
    · rw [entry_eq f.blocks n X hf] at h
      have hM : findBlock (mergeFn f from_ to tb args).blocks f.entry =
          some (mapBlock from_ tb args X) := by
        rw [hMentry, hf]; rfl
      rw [entry_eq _ n (mapBlock from_ tb args X) hM]
      exact fwd_all f from_ to fb tb args C n n (Nat.le_refl n) X (findBlock_mem hf)
        (by rw [findBlock_id hf]; exact Ne.symm C.hentry) s s [] r hrel hne h
  · rintro ⟨n, h⟩
    rw [runFunction_eq_runAt, hMe] at h
    rcases Option.eq_none_or_eq_some (findBlock f.blocks f.entry) with hf | ⟨X, hf⟩
    · have hMn : findBlock (mergeFn f from_ to tb args).blocks f.entry = none := by
        rw [hMentry, hf]; rfl
      rw [runAt_none hMn] at h
      refine ⟨0, ?_⟩
      rw [runFunction_eq_runAt, runAt_none hf]
      exact h
    · have hM : findBlock (mergeFn f from_ to tb args).blocks f.entry =
          some (mapBlock from_ tb args X) := by
        rw [hMentry, hf]; rfl
      rw [entry_eq _ n (mapBlock from_ tb args X) hM] at h
      obtain ⟨m, hm⟩ := bwd_all f from_ to fb tb args C n X (findBlock_mem hf)
        (by rw [findBlock_id hf]; exact Ne.symm C.hentry) s s [] r hrel hne h
      refine ⟨m, ?_⟩
      rw [runFunction_eq_runAt, entry_eq _ m X hf]
      exact hm

end IR
end TptWasm
