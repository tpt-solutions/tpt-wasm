import TptWasm.Nested.Trap

/-!
# The nested simulation, for traps

A statement that traps from `c`, lowered with continuation `K`, is entered at its entry block in a
state corresponding to `c`, and the block graph traps there with the events the Wasm run performed
before the trap, appended to the trace it started with, and the same trap. The induction is over the
statement structure and the fuel, as the success simulation is (`Simulate`): a straight-line trap is
one block (`V4Trap.trap_ir`); a condition trap is the condition's block; an arm or a loop body that
traps is reached through its branch, and a loop that traps on a later iteration is reached by the
induction on fuel, after one successful iteration (`GotoN_transT`).
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond V4Diamond

mutual

theorem trap_stmt : ∀ (k : Nat) (s : Stmt) (kid : IR.BlockId) (kps : List IR.ValueId)
    (kblk : IR.Block) (L n nb : Nat) (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block)
    (n' nb' : Nat) (G : List IR.Block) (c : Configuration) (evs : List Event) (t : Trap)
    (st : IR.State),
    lowerStmt s (kid, kps) L n nb = some (E, EP, B, n', nb') →
    SupStmtL s → kps.length = L → kps.Nodup → IR.findBlock G kid = some kblk →
    kblk.params = kps → (∀ b ∈ B, b ∈ G) → (G.map IR.Block.id).Nodup →
    V4Diamond.GlobalsI32 c.store → BoundCorr c st EP → TrapS k s c evs t →
    ∀ tr, GotoT G E st tr (tr ++ evs) t
  | k, .straight ops, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, evs, t, st, h, hs, hkL,
      hkN, hK, hkb, hG, hnd, hgl, hc, hr, tr => by
    cases hr with
    | straight _ _ _ _ _ hop =>
      obtain ⟨pre, i, post, ck, hsplit, hpre, htrap⟩ := hop
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
          have hcorr := corresponds_of_bound (n := n + L) hc (fun id hid =>
            (List.mem_range'_1.mp hid).2)
          subst hsplit
          have hT := trap_ir pre post i c ck evs t hpre htrap st [] (List.range' n L) (n + L) irOps []
            Q m tr hcorr hgl hl
          have hmemB : strBlk nb irOps kid L n Q ∈ G := hG _ (by simp [strBlk])
          have hfb : IR.findBlock G nb = some (strBlk nb irOps kid L n Q) :=
            findBlock_of_mem G _ hmemB hnd
          exact GotoT.blk nb st tr (tr ++ evs) t (strBlk nb irOps kid L n Q) hfb hT
  | k, .ifElse cond th el, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, evs, t, st, h, hs,
      hkL, hkN, hK, hkb, hG, hnd, hgl, hc, hr, tr => by
    obtain ⟨hsc, hst, hse⟩ := hs
    unfold lowerStmt at h
    cases hl : V4Straight.lower cond [] (List.range' n L) (n + L) with
    | none => simp [hl] at h
    | some q3 =>
      obtain ⟨irC, fin, Q, m⟩ := q3
      rcases fin with _ | ⟨c0, _ | ⟨c2', tl⟩⟩
      · simp [hl] at h
      · cases ht : lowerSeq th (kid, kps) L m (nb + 1) with
        | none => simp [hl, ht] at h
        | some qt =>
          obtain ⟨tE, tEP, tB, m2, nb2⟩ := qt
          cases he : lowerSeq el (kid, kps) L m2 nb2 with
          | none => simp [hl, ht, he] at h
          | some qe =>
            obtain ⟨eE, eEP, eB, m3, nb3⟩ := qe
            simp only [hl, ht, he, Option.some.injEq, Prod.mk.injEq] at h
            obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
            have hcorr := corresponds_of_bound (n := n + L) hc (fun id hid =>
              (List.mem_range'_1.mp hid).2)
            have hmemB0 : condBlk nb irC c0 L n tE eE Q ∈ G := hG _ (by simp [condBlk])
            have hfb0 : IR.findBlock G nb = some (condBlk nb irC c0 L n tE eE Q) :=
              findBlock_of_mem G _ hmemB0 hnd
            have hOKt := lowerSeq_spec th (kid, kps) L m (nb + 1) tE tEP tB m2 nb2 ht
            have hOKe := lowerSeq_spec el (kid, kps) L m2 nb2 eE eEP eB m3 nb3 he
            have hQlen : Q.length = L := by
              have := (V4Diamond.lower_props cond [] (List.range' n L) (n + L) irC [c0] Q m hl).1
              simpa using this
            cases hr with
            | condTrap _ _ _ _ _ _ _ hstk hop =>
              obtain ⟨pre, i, post, ck, hsplit, hpre, htrap⟩ := hop
              subst hsplit
              have hT := trap_ir pre post i c ck evs t hpre htrap st [] (List.range' n L) (n + L) irC
                [c0] Q m tr hcorr hgl hl
              exact GotoT.blk nb st tr (tr ++ evs) t (condBlk nb irC c0 L n tE eE Q) hfb0 hT
            | armThen _ _ _ _ c c1 e1 evs v _ hstk hcond hstack hv hseq =>
              have sim := V4Straight.simulation cond c c1 e1 st [] (List.range' n L) (n + L) irC [c0]
                Q m hcorr hgl hl hcond
              obtain ⟨st1, hrb, hcorr1⟩ := sim tr
              have hv1 : st1.values c0 = .i32 v := by
                have h1' := hcorr1.1
                rw [hstack] at h1'
                simp at h1'
                exact h1'.symm
              have hrbR := V4Diamond.runBodyR_of_runBody irC st tr st1 (tr ++ e1) hrb
              have hintP : IR.intPayload (st1.values c0) ≠ 0 := by
                rw [hv1]
                exact hv
              have hc2 := boundCorr_of_corresponds_nostack hcorr1
              have hgl1 := runWasm_globals_of_sup cond c c1 e1 hsc hgl hcond
              have hGt : ∀ b ∈ tB, b ∈ G := fun b hb =>
                hG b (List.mem_cons_of_mem _ (List.mem_append_left _ hb))
              obtain ⟨tblk, htfb, htpar, -⟩ := entry_lookup hOKt.1 hGt hnd
              obtain ⟨n0t, hEPt⟩ := lowerSeq_params th (kid, kps) L m (nb + 1) tE tEP tB m2 nb2 ht
              have hndt : tEP.Nodup := by
                rw [hEPt]
                exact List.nodup_range' (s := n0t) (n := L)
              have hBCt := boundCorr_bind hc2 hndt (by rw [hOKt.2, hQlen])
              have hT := trap_seq k th kid kps kblk L m (nb + 1) tE tEP tB m2 nb2 G
                { c1 with operandStack := [] } evs t (st1.bindAll tEP (Q.map st1.values)) ht hst hkL
                hkN hK hkb hGt hnd hgl1 hBCt hseq (tr ++ e1)
              have hgoto := GotoT.condThen hfb0 rfl hrbR hintP htfb (by rw [htpar]; exact hT)
              rw [← List.append_assoc]
              exact hgoto
            | armElse _ _ _ _ c c1 e1 evs _ hstk hcond hstack hseq =>
              have sim := V4Straight.simulation cond c c1 e1 st [] (List.range' n L) (n + L) irC [c0]
                Q m hcorr hgl hl hcond
              obtain ⟨st1, hrb, hcorr1⟩ := sim tr
              have hv1 : st1.values c0 = .i32 0 := by
                have h1' := hcorr1.1
                rw [hstack] at h1'
                simp at h1'
                exact h1'.symm
              have hrbR := V4Diamond.runBodyR_of_runBody irC st tr st1 (tr ++ e1) hrb
              have hv0 : IR.intPayload (st1.values c0) = 0 := by
                rw [hv1]
                rfl
              have hc2 := boundCorr_of_corresponds_nostack hcorr1
              have hgl1 := runWasm_globals_of_sup cond c c1 e1 hsc hgl hcond
              have hGe : ∀ b ∈ eB, b ∈ G := fun b hb =>
                hG b (List.mem_cons_of_mem _ (List.mem_append_right _ hb))
              obtain ⟨eblk, hefb, hepar, -⟩ := entry_lookup hOKe.1 hGe hnd
              obtain ⟨n0e, hEPe⟩ := lowerSeq_params el (kid, kps) L m2 nb2 eE eEP eB m3 nb3 he
              have hnde : eEP.Nodup := by
                rw [hEPe]
                exact List.nodup_range' (s := n0e) (n := L)
              have hBCe := boundCorr_bind hc2 hnde (by rw [hOKe.2, hQlen])
              have hT := trap_seq k el kid kps kblk L m2 nb2 eE eEP eB m3 nb3 G
                { c1 with operandStack := [] } evs t (st1.bindAll eEP (Q.map st1.values)) he hse hkL
                hkN hK hkb hGe hnd hgl1 hBCe hseq (tr ++ e1)
              have hgoto := GotoT.condElse hfb0 rfl hrbR hv0 hefb (by rw [hepar]; exact hT)
              rw [← List.append_assoc]
              exact hgoto
      · simp [hl] at h
  | 0, .whileNZ _ _, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, evs, t, st, h, hs, hkL,
      hkN, hK, hkb, hG, hnd, hgl, hc, hr, tr => by
    cases hr
  | k + 1, .whileNZ cond body, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, evs, t, st, h, hs,
      hkL, hkN, hK, hkb, hG, hnd, hgl, hc, hr, tr => by
    have hlow0 := h
    have hs0 := hs
    obtain ⟨hsc, hsb⟩ := hs
    unfold lowerStmt at h
    cases hl : V4Straight.lower cond [] (List.range' n L) (n + L) with
    | none => simp [hl] at h
    | some q3 =>
      obtain ⟨irC, fin, Q, m⟩ := q3
      rcases fin with _ | ⟨c0, _ | ⟨c2', tl⟩⟩
      · simp [hl] at h
      · cases hb : lowerSeq body (nb, List.range' n L) L m (nb + 1) with
        | none => simp [hl, hb] at h
        | some qb =>
          obtain ⟨bE, bEP, bB, m2, nb2⟩ := qb
          simp only [hl, hb, Option.some.injEq, Prod.mk.injEq] at h
          obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
          have hcorr := corresponds_of_bound (n := n + L) hc (fun id hid =>
            (List.mem_range'_1.mp hid).2)
          have hmemH : loopBlk nb irC c0 L n bE kid Q ∈ G := hG _ (by simp [loopBlk])
          have hfbH : IR.findBlock G nb = some (loopBlk nb irC c0 L n bE kid Q) :=
            findBlock_of_mem G _ hmemH hnd
          have hOKb := lowerSeq_spec body (nb, List.range' n L) L m (nb + 1) bE bEP bB m2 nb2 hb
          have hGb : ∀ b ∈ bB, b ∈ G := fun b hb' => hG b (List.mem_cons_of_mem _ hb')
          obtain ⟨bblk, hbfb, hbpar, -⟩ := entry_lookup hOKb.1 hGb hnd
          have hQlen : Q.length = L := by
            have := (V4Diamond.lower_props cond [] (List.range' n L) (n + L) irC [c0] Q m hl).1
            simpa using this
          have hbEP := lowerSeq_params body (nb, List.range' n L) L m (nb + 1) bE bEP bB m2 nb2 hb
          obtain ⟨n0b, hbEPeq⟩ := hbEP
          have hndb : bEP.Nodup := by
            rw [hbEPeq]
            exact List.nodup_range' (s := n0b) (n := L)
          cases hr with
          | loopCond _ _ _ _ _ _ hstk hop =>
            obtain ⟨pre, i, post, ck, hsplit, hpre, htrap⟩ := hop
            subst hsplit
            have hT := trap_ir pre post i c ck evs t hpre htrap st [] (List.range' n L) (n + L) irC
              [c0] Q m tr hcorr hgl hl
            exact GotoT.blk nb st tr (tr ++ evs) t (loopBlk nb irC c0 L n bE kid Q) hfbH hT
          | loopBody _ _ _ c c1 e1 evs v _ hstk hcond hstack hv hseq =>
            have sim := V4Straight.simulation cond c c1 e1 st [] (List.range' n L) (n + L) irC [c0]
              Q m hcorr hgl hl hcond
            obtain ⟨st1, hrb, hcorr1⟩ := sim tr
            have hv1 : st1.values c0 = .i32 v := by
              have h1' := hcorr1.1
              rw [hstack] at h1'
              simp at h1'
              exact h1'.symm
            have hrbR := V4Diamond.runBodyR_of_runBody irC st tr st1 (tr ++ e1) hrb
            have hintP : IR.intPayload (st1.values c0) ≠ 0 := by
              rw [hv1]
              exact hv
            have hc2 := boundCorr_of_corresponds_nostack hcorr1
            have hgl1 := runWasm_globals_of_sup cond c c1 e1 hsc hgl hcond
            have hBCb := boundCorr_bind hc2 hndb (by rw [hOKb.2, hQlen])
            have hT := trap_seq k body nb (List.range' n L) (loopBlk nb irC c0 L n bE kid Q) L m
              (nb + 1) bE bEP bB m2 nb2 G { c1 with operandStack := [] } evs t
              (st1.bindAll bEP (Q.map st1.values)) hb hsb (by simp)
              (List.nodup_range' (s := n) (n := L)) hfbH rfl hGb hnd hgl1 hBCb hseq (tr ++ e1)
            have hgoto := GotoT.condThen hfbH rfl hrbR hintP hbfb (by rw [hbpar]; exact hT)
            rw [← List.append_assoc]
            exact hgoto
          | loopNext _ _ _ c c1 c3 e1 e2 evs v _ hstk hcond hstack hv hrun hrest =>
            have sim := V4Straight.simulation cond c c1 e1 st [] (List.range' n L) (n + L) irC [c0]
              Q m hcorr hgl hl hcond
            obtain ⟨st1, hrb, hcorr1⟩ := sim tr
            have hv1 : st1.values c0 = .i32 v := by
              have h1' := hcorr1.1
              rw [hstack] at h1'
              simp at h1'
              exact h1'.symm
            have hrbR := V4Diamond.runBodyR_of_runBody irC st tr st1 (tr ++ e1) hrb
            have hintP : IR.intPayload (st1.values c0) ≠ 0 := by
              rw [hv1]
              exact hv
            have hc2 := boundCorr_of_corresponds_nostack hcorr1
            have hgl1 := runWasm_globals_of_sup cond c c1 e1 hsc hgl hcond
            have hBCb := boundCorr_bind hc2 hndb (by rw [hOKb.2, hQlen])
            -- one iteration of the body, back at the header
            obtain ⟨n0b', st2, hG2, hBC2⟩ := sim_seq k body nb (List.range' n L)
              (loopBlk nb irC c0 L n bE kid Q) L m (nb + 1) bE bEP bB m2 nb2 G
              { c1 with operandStack := [] } c3 e2 (st1.bindAll bEP (Q.map st1.values))
              hb hsb (by simp) (List.nodup_range' (s := n) (n := L)) hfbH rfl hGb hnd
              hgl1 hBCb hrun (tr ++ e1)
            have hgl3 := runSeq_globals k body { c1 with operandStack := [] } c3 e2 hsb hgl1 hrun
            have hgoto : GotoN G (n0b' + 1) nb st tr nb st2 ((tr ++ e1) ++ e2) :=
              GotoN.condThen (n := n0b') (ta := Q) (ea := Q) hfbH rfl hrbR hintP hbfb
                (by rw [hbpar]; exact hG2)
            -- the rest of the loop traps, from the header
            have hrestT := trap_stmt k (.whileNZ cond body) kid kps kblk L n nb _ _ _ _ _ G c3 evs t st2
              hlow0 hs0 hkL hkN hK hkb hG hnd hgl3 hBC2 hrest (tr ++ e1 ++ e2)
            have hcomb := GotoN_transT hgoto hrestT
            have hev : tr ++ (e1 ++ e2 ++ evs) = ((tr ++ e1) ++ e2) ++ evs := by
              simp [List.append_assoc]
            rw [hev]
            exact hcomb
      · simp [hl] at h

theorem trap_seq : ∀ (k : Nat) (ss : List Stmt) (kid : IR.BlockId) (kps : List IR.ValueId)
    (kblk : IR.Block) (L n nb : Nat) (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block)
    (n' nb' : Nat) (G : List IR.Block) (c : Configuration) (evs : List Event) (t : Trap)
    (st : IR.State),
    lowerSeq ss (kid, kps) L n nb = some (E, EP, B, n', nb') →
    SupSeqL ss → kps.length = L → kps.Nodup → IR.findBlock G kid = some kblk →
    kblk.params = kps → (∀ b ∈ B, b ∈ G) → (G.map IR.Block.id).Nodup →
    V4Diamond.GlobalsI32 c.store → BoundCorr c st EP → TrapSeq k ss c evs t →
    ∀ tr, GotoT G E st tr (tr ++ evs) t
  | k, [], kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, evs, t, st, h, hs, hkL, hkN, hK,
      hkb, hG, hnd, hgl, hc, hr, tr => by
    cases hr
  | k, s :: rest, kid, kps, kblk, L, n, nb, E, EP, B, n', nb', G, c, evs, t, st, h, hs, hkL, hkN,
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
        have hrestSpec := lowerSeq_spec rest (kid, kps) L n nb rE rEP rB n1 nb1 hr'
        have hrEP := lowerSeq_params rest (kid, kps) L n nb rE rEP rB n1 nb1 hr'
        obtain ⟨rblk, hrfb, hrpar, -⟩ := entry_lookup hrestSpec.1
          (fun b hb => hG b (List.mem_append_right _ hb)) hnd
        obtain ⟨n0r, hrEPeq⟩ := hrEP
        have hrNd : rEP.Nodup := by
          rw [hrEPeq]
          exact List.nodup_range' (s := n0r) (n := L)
        have hrLen : rEP.length = L := hrestSpec.2
        cases hr with
        | head _ _ _ _ _ _ htr =>
          exact trap_stmt k s rE rEP rblk L n1 nb1 sE sEP sB n2 nb2 G c evs t st hs' hs1 hrLen hrNd
            hrfb (by rw [hrpar]) (fun b hb => hG b (List.mem_append_left _ hb)) hnd hgl hc htr tr
        | tail _ _ _ c c1 e1 evs _ hrun hrest =>
          obtain ⟨n1', st1, hG1, hBC1⟩ := sim_stmt k s rE rEP rblk L n1 nb1 sE sEP sB n2 nb2 G c c1
            e1 st hs' hs1 hrLen hrNd hrfb (by rw [hrpar])
            (fun b hb => hG b (List.mem_append_left _ hb)) hnd hgl hc hrun tr
          have hgl1 := runStmt_globals k s c c1 e1 hs1 hgl hrun
          have hT := trap_seq k rest kid kps kblk L n nb rE rEP rB n1 nb1 G c1 evs t st1 hr' hs2 hkL
            hkN hK hkb (fun b hb => hG b (List.mem_append_right _ hb)) hnd hgl1 hBC1 hrest (tr ++ e1)
          have hcomb := GotoN_transT hG1 hT
          rw [← List.append_assoc]
          exact hcomb

end

end Nested
end TptWasm
