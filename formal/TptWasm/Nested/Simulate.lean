import TptWasm.Nested.Sim

/-!
# The nested simulation

For a statement or statement list, lowered with continuation `K`, and a Wasm run of it from `c`
that ends at `c'` with events `evs`: from the entry block, in the IR state corresponding to `c`, the
block graph reaches `K` with the events `evs` appended to any trace, and in a state corresponding to
`c'`. The induction is over the statement structure: a straight-line statement is one block (the
operators, by `V4Straight.simulation`); an `if` is a block that runs the condition and branches,
with each arm handled by the induction; a list composes its statements (`GotoN_trans`).
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond V4Diamond

/-- The block a straight-line statement lowers to. -/
def strBlk (nb : Nat) (irOps : List IR.Instr) (kid : IR.BlockId) (L n : Nat) (Q : List IR.ValueId) :
    IR.Block :=
  { id := nb, params := List.range' n L, instrs := irOps, terminator := .branch kid Q }

/-- The block an empty statement list lowers to: a pass-through to the continuation. -/
def emptyBlk (nb : Nat) (kid : IR.BlockId) (L n : Nat) : IR.Block :=
  { id := nb, params := List.range' n L, instrs := [], terminator := .branch kid (List.range' n L) }

/-- The block an `if` lowers to: the condition, and a branch on it to the two arms. -/
def condBlk (nb : Nat) (irC : List IR.Instr) (c0 : IR.ValueId) (L n : Nat) (tE eE : IR.BlockId)
    (Q : List IR.ValueId) : IR.Block :=
  { id := nb, params := List.range' n L, instrs := irC,
    terminator := .condBranch c0 (tE, Q) (eE, Q) }

mutual

theorem sim_stmt : ∀ (s : Stmt) (kid : IR.BlockId) (kps : List IR.ValueId) (kblk : IR.Block)
    (L n nb : Nat) (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block) (n' nb' : Nat)
    (G : List IR.Block) (c c' : Configuration) (evs : List Event) (st : IR.State),
    lowerStmt s (kid, kps) L n nb = some (E, EP, B, n', nb') →
    SupStmt s → kps.length = L → kps.Nodup → IR.findBlock G kid = some kblk →
    kblk.params = kps → (∀ b ∈ B, b ∈ G) → (G.map IR.Block.id).Nodup →
    V4Diamond.GlobalsI32 c.store → BoundCorr c st EP → runStmt s c = some (c', evs) →
    ∀ tr, ∃ n0 st', GotoN G n0 E st tr kid st' (tr ++ evs) ∧ BoundCorr c' st' kps
  | .straight ops, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, c', evs, st, h, hs, hkL,
      hkN, hK, hkb, hG, hnd, hgl, hc, hr, tr => by
    obtain ⟨hw, hstk0, hstk1⟩ := runStmt_straight_extract hr
    have hspec := lowerStmt_spec (.straight ops) (kid, kps) L n nb E EP B n' nb' h
    unfold lowerStmt at h
    cases hl : V4Straight.lower ops [] (List.range' n L) (n + L) with
    | none => simp [hl] at h
    | some q =>
      obtain ⟨irOps, fin, Q, m⟩ := q
      cases fin with
      | cons _ _ => simp [hl] at h
      | nil =>
        simp only [hl, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
        have hcorr := corresponds_of_bound (n := n + L) hc (fun id hid => by
          have hid2 := (List.mem_range'_1.mp hid).2
          exact hid2)
        have sim := V4Straight.simulation ops c c' evs st [] (List.range' n L) (n + L) irOps []
          Q m hcorr hgl hl hw
        obtain ⟨st1, hrb, hcorr1⟩ := sim tr
        have hrbR := V4Diamond.runBodyR_of_runBody irOps st tr st1 (tr ++ evs) hrb
        have hQlen : Q.length = L := by
          have := (V4Diamond.lower_props ops [] (List.range' n L) (n + L) irOps [] Q m hl).1
          simpa using this
        have hmemB : strBlk nb irOps kid L n Q ∈ G := hG _ (by simp [strBlk])
        have hfb : IR.findBlock G nb = some (strBlk nb irOps kid L n Q) :=
          findBlock_of_mem G _ hmemB hnd
        have hstep := GotoN.branch (n := 0) (args := Q) hfb rfl hrbR hK
          (GotoN.here kid _ (tr ++ evs))
        rw [hkb] at hstep
        refine ⟨0 + 1, _, hstep, ?_⟩
        have hBC1 := boundCorr_of_corresponds hcorr1
        exact boundCorr_bind hBC1 hkN (by rw [hQlen, hkL])
  | .ifElse cond t el, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, c', evs, st, h, hs,
      hkL, hkN, hK, hkb, hG, hnd, hgl, hc, hr, tr => by
    obtain ⟨hsc, hst, hse⟩ := hs
    unfold runStmt at hr
    by_cases hstk : c.operandStack = []
    · simp only [hstk, ↓reduceIte] at hr
      cases hcond : runWasm cond c with
      | none => simp [hcond] at hr
      | some q =>
        obtain ⟨c1, e1⟩ := q
        simp only [hcond] at hr
        split at hr
        next v hop =>
          cases harm : (if v = 0 then runSeq el { c1 with operandStack := [] }
              else runSeq t { c1 with operandStack := [] }) with
          | none => simp [harm] at hr
          | some q2 =>
            obtain ⟨c3, e2⟩ := q2
            simp only [harm] at hr
            split at hr
            next hemp =>
              simp only [Option.some.injEq, Prod.mk.injEq] at hr
              obtain ⟨rfl, rfl⟩ := hr
              unfold lowerStmt at h
              cases hl : V4Straight.lower cond [] (List.range' n L) (n + L) with
              | none => simp [hl] at h
              | some q3 =>
                obtain ⟨irC, fin, Q, m⟩ := q3
                rcases fin with _ | ⟨c0, _ | ⟨c2', tl⟩⟩
                · simp [hl] at h
                · cases ht : lowerSeq t (kid, kps) L m (nb + 1) with
                  | none => simp [hl, ht] at h
                  | some qt =>
                    obtain ⟨tE, tEP, tB, m2, nb2⟩ := qt
                    cases he : lowerSeq el (kid, kps) L m2 nb2 with
                    | none => simp [hl, ht, he] at h
                    | some qe =>
                      obtain ⟨eE, eEP, eB, m3, nb3⟩ := qe
                      simp only [hl, ht, he, Option.some.injEq, Prod.mk.injEq] at h
                      obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
                      -- the condition runs, and its block branches on the value it leaves
                      have hcorr := corresponds_of_bound (n := n + L) hc (fun id hid => by
                        have hid2 := (List.mem_range'_1.mp hid).2
                        exact hid2)
                      have sim := V4Straight.simulation cond c c1 e1 st [] (List.range' n L)
                        (n + L) irC [c0] Q m hcorr hgl hl hcond
                      obtain ⟨st1, hrb, hcorr1⟩ := sim tr
                      have hv1 : st1.values c0 = .i32 v := by
                        have h1' := hcorr1.1
                        rw [hop] at h1'
                        simp at h1'
                        exact h1'.symm
                      have hrbR := V4Diamond.runBodyR_of_runBody irC st tr st1 (tr ++ e1) hrb
                      have hQlen : Q.length = L := by
                        have := (V4Diamond.lower_props cond [] (List.range' n L) (n + L) irC
                          [c0] Q m hl).1
                        simpa using this
                      have hmemB0 : condBlk nb irC c0 L n tE eE Q ∈ G :=
                        hG _ (by simp [condBlk])
                      have hfb0 : IR.findBlock G nb = some (condBlk nb irC c0 L n tE eE Q) :=
                        findBlock_of_mem G _ hmemB0 hnd
                      have hOKt := lowerSeq_spec t (kid, kps) L m (nb + 1) tE tEP tB m2 nb2 ht
                      have hOKe := lowerSeq_spec el (kid, kps) L m2 nb2 eE eEP eB m3 nb3 he
                      obtain ⟨tblk, htfb, htpar, -⟩ := entry_lookup hOKt.1
                        (fun b hb => hG b (List.mem_cons_of_mem _ (List.mem_append_left _ hb))) hnd
                      obtain ⟨eblk, hefb, hepar, -⟩ := entry_lookup hOKe.1
                        (fun b hb => hG b (List.mem_cons_of_mem _ (List.mem_append_right _ hb))) hnd
                      have hc2 := boundCorr_of_corresponds_nostack hcorr1
                      have hGt : ∀ b ∈ tB, b ∈ G := fun b hb =>
                        hG b (List.mem_cons_of_mem _ (List.mem_append_left _ hb))
                      have hGe : ∀ b ∈ eB, b ∈ G := fun b hb =>
                        hG b (List.mem_cons_of_mem _ (List.mem_append_right _ hb))
                      have hgl1 := runWasm_globals_of_sup cond c c1 e1 hsc hgl hcond
                      obtain ⟨n0t, hEPt⟩ := lowerSeq_params t (kid, kps) L m (nb + 1) tE tEP tB m2 nb2 ht
                      obtain ⟨n0e, hEPe⟩ := lowerSeq_params el (kid, kps) L m2 nb2 eE eEP eB m3 nb3 he
                      have hndt : tEP.Nodup := by
                        rw [hEPt]
                        exact List.nodup_range' (s := n0t) (n := L)
                      have hnde : eEP.Nodup := by
                        rw [hEPe]
                        exact List.nodup_range' (s := n0e) (n := L)
                      have hBCt := boundCorr_bind hc2 hndt (by rw [hOKt.2, hQlen])
                      have hBCe := boundCorr_bind hc2 hnde (by rw [hOKe.2, hQlen])
                      by_cases hv : v = 0
                      · -- the condition is zero: the else arm runs
                        subst hv
                        simp only [↓reduceIte] at harm
                        have hv0 : IR.intPayload (st1.values c0) = 0 := by
                          rw [hv1]
                          rfl
                        obtain ⟨n', s', hG2, hBC3⟩ := sim_seq el kid kps kblk L m2 nb2 eE eEP eB m3
                          nb3 G { c1 with operandStack := [] } c3 e2 (st1.bindAll eEP (Q.map st1.values))
                          he hse hkL hkN hK hkb hGe hnd hgl1 hBCe harm (tr ++ e1)
                        refine ⟨n' + 1, s', ?_, hBC3⟩
                        have hgoto := GotoN.condElse (n := n') (ta := Q) (ea := Q) hfb0 rfl hrbR hv0 hefb
                          (by rw [hepar]; exact hG2)
                        rw [← List.append_assoc]
                        exact hgoto
                      · -- the condition is non-zero: the then arm runs
                        simp only [hv, ↓reduceIte] at harm
                        have hv0 : IR.intPayload (st1.values c0) ≠ 0 := by
                          rw [hv1]
                          exact hv
                        obtain ⟨n', s', hG2, hBC3⟩ := sim_seq t kid kps kblk L m (nb + 1) tE tEP tB m2
                          nb2 G { c1 with operandStack := [] } c3 e2 (st1.bindAll tEP (Q.map st1.values))
                          ht hst hkL hkN hK hkb hGt hnd hgl1 hBCt harm (tr ++ e1)
                        refine ⟨n' + 1, s', ?_, hBC3⟩
                        have hgoto := GotoN.condThen (n := n') (ta := Q) (ea := Q) hfb0 rfl hrbR hv0 htfb
                          (by rw [htpar]; exact hG2)
                        rw [← List.append_assoc]
                        exact hgoto
                · simp [hl] at h
            next => simp at hr
        next => simp at hr
    · simp [hstk] at hr

theorem sim_seq : ∀ (ss : List Stmt) (kid : IR.BlockId) (kps : List IR.ValueId) (kblk : IR.Block)
    (L n nb : Nat) (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block) (n' nb' : Nat)
    (G : List IR.Block) (c c' : Configuration) (evs : List Event) (st : IR.State),
    lowerSeq ss (kid, kps) L n nb = some (E, EP, B, n', nb') →
    SupSeq ss → kps.length = L → kps.Nodup → IR.findBlock G kid = some kblk →
    kblk.params = kps → (∀ b ∈ B, b ∈ G) → (G.map IR.Block.id).Nodup →
    V4Diamond.GlobalsI32 c.store → BoundCorr c st EP → runSeq ss c = some (c', evs) →
    ∀ tr, ∃ n0 st', GotoN G n0 E st tr kid st' (tr ++ evs) ∧ BoundCorr c' st' kps
  | [], kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, c', evs, st, h, hs, hkL, hkN, hK,
      hkb, hG, hnd, hgl, hc, hr, tr => by
    simp only [lowerSeq, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
    simp only [runSeq, Option.some.injEq, Prod.mk.injEq] at hr
    obtain ⟨rfl, rfl⟩ := hr
    have hmemB : emptyBlk nb kid L n ∈ G := hG _ (by simp [emptyBlk])
    have hfb : IR.findBlock G nb = some (emptyBlk nb kid L n) :=
      findBlock_of_mem G _ hmemB hnd
    have hB : IR.runBodyR [] st tr = .inr (st, tr) := rfl
    have hstep := GotoN.branch (n := 0) (args := List.range' n L) hfb rfl hB hK
      (GotoN.here kid _ tr)
    rw [hkb] at hstep
    refine ⟨0 + 1, _, by simpa [emptyBlk] using hstep, ?_⟩
    exact boundCorr_bind hc hkN (by simp [hkL])
  | s :: rest, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, c', evs, st, h, hs, hkL, hkN,
      hK, hkb, hG, hnd, hgl, hc, hr, tr => by
    obtain ⟨hs1, hs2⟩ := hs
    unfold lowerSeq at h
    cases hr' : lowerSeq rest (kid, kps) L n nb with
    | none => simp [hr'] at h
    | some qr =>
      obtain ⟨rE, rEP, rB, n1, nb1⟩ := qr
      cases hs' : lowerStmt s (rE, rEP) L n1 nb1 with
      | none => simp [hr', hs'] at h
      | some qs =>
        obtain ⟨sE, sEP, sB, n2, nb2⟩ := qs
        simp only [hr', hs', Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
        unfold runSeq at hr
        cases h1 : runStmt s c with
        | none => simp [h1] at hr
        | some q1 =>
          obtain ⟨c1, e1⟩ := q1
          simp only [h1] at hr
          cases h2 : runSeq rest c1 with
          | none => simp [h2] at hr
          | some q2 =>
            obtain ⟨c2, e2⟩ := q2
            simp only [h2, Option.some.injEq, Prod.mk.injEq] at hr
            obtain ⟨rfl, rfl⟩ := hr
            have hrestSpec := lowerSeq_spec rest (kid, kps) L n nb rE rEP rB n1 nb1 hr'
            have hrEP := lowerSeq_params rest (kid, kps) L n nb rE rEP rB n1 nb1 hr'
            obtain ⟨rblk, hrfb, hrpar, -⟩ := entry_lookup hrestSpec.1
              (fun b hb => hG b (List.mem_append_right _ hb)) hnd
            obtain ⟨n0r, hrEPeq⟩ := hrEP
            have hrNd : rEP.Nodup := by
              rw [hrEPeq]
              exact List.nodup_range' (s := n0r) (n := L)
            have hrLen : rEP.length = L := hrestSpec.2
            obtain ⟨n1', st1, hG1, hBC1⟩ := sim_stmt s rE rEP rblk L n1 nb1 sE sEP sB n2 nb2 G c c1
              e1 st hs' hs1 hrLen hrNd hrfb (by rw [hrpar])
              (fun b hb => hG b (List.mem_append_left _ hb)) hnd hgl hc h1 tr
            have hgl1 := runStmt_globals s c c1 e1 hs1 hgl h1
            obtain ⟨n2', st2, hG2, hBC2⟩ := sim_seq rest kid kps kblk L n nb rE rEP rB n1 nb1 G c1 c2
              e2 st1 hr' hs2 hkL hkN hK hkb (fun b hb => hG b (List.mem_append_right _ hb)) hnd hgl1
              hBC1 h2 (tr ++ e1)
            refine ⟨n1' + n2', st2, ?_, hBC2⟩
            rw [← List.append_assoc]
            exact GotoN_trans hG1 hG2

end

end Nested
end TptWasm
