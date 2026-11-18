import TptWasm.Nested.Machine

/-!
# Lowering nested statements to blocks, with continuations

A statement is lowered given the block it continues to: its id and the parameters that block takes
(the locals, one per local). A straight-line statement becomes one block that runs the code and
branches to the continuation with the locals it left. An `if` becomes a block that runs the
condition and branches on it, with each arm lowered with the same continuation. So no join block is
needed: both arms branch straight to what follows the `if`.

A statement list is lowered from its end back to its start: the last statement continues to the
continuation, and each earlier statement continues to the entry of the rest.

Every block gets a fresh id from a counter, and every parameter and temporary a fresh value id, so
the blocks of one lowering have distinct ids in a known range. `lowerStmt_spec` and
`lowerSeq_spec` record that range, the membership of each entry, and the distinctness of ids.
-/

namespace TptWasm
namespace Nested

open V4Straight

/-- The result of lowering: the entry block's id, the entry's parameters, the blocks, the next
fresh value id, and the next fresh block id. -/
abbrev Lowered := IR.BlockId × List IR.ValueId × List IR.Block × Nat × Nat

mutual

/-- Lower one statement, continuing to block `K` (its id and parameters), with `L` locals. The
entry takes the `L` locals as parameters, fresh from value id `n`; the next free block id is `nb`. -/
def lowerStmt : Stmt → IR.BlockId × List IR.ValueId → Nat → Nat → Nat → Option Lowered
  | .straight ops, (kid, _), L, n, nb =>
    match V4Straight.lower ops [] (List.range' n L) (n + L) with
    | some (irOps, [], Q, m) =>
      some (nb, List.range' n L,
        [{ id := nb, params := List.range' n L, instrs := irOps, terminator := .branch kid Q }],
        m, nb + 1)
    | _ => none
  | .ifElse cond t el, (kid, kps), L, n, nb =>
    match V4Straight.lower cond [] (List.range' n L) (n + L) with
    | some (irC, [c], Q, m) =>
      match lowerSeq t (kid, kps) L m (nb + 1) with
      | some (tE, _, tB, m2, nb2) =>
        match lowerSeq el (kid, kps) L m2 nb2 with
        | some (eE, _, eB, m3, nb3) =>
          some (nb, List.range' n L,
            { id := nb, params := List.range' n L, instrs := irC,
              terminator := .condBranch c (tE, Q) (eE, Q) } :: (tB ++ eB),
            m3, nb3)
        | none => none
      | none => none
    | _ => none

/-- Lower a statement list, continuing to block `K`. -/
def lowerSeq : List Stmt → IR.BlockId × List IR.ValueId → Nat → Nat → Nat → Option Lowered
  | [], (kid, _), L, n, nb =>
    some (nb, List.range' n L,
      [{ id := nb, params := List.range' n L, instrs := [], terminator := .branch kid (List.range' n L) }],
      n + L, nb + 1)
  | s :: rest, K, L, n, nb =>
    match lowerSeq rest K L n nb with
    | some (rE, rEP, rB, n1, nb1) =>
      match lowerStmt s (rE, rEP) L n1 nb1 with
      | some (sE, sEP, sB, n2, nb2) => some (sE, sEP, sB ++ rB, n2, nb2)
      | none => none
    | none => none

end

/-- The blocks of a lowering have ids in their range, distinct ids, and the entry among them with
its parameters. -/
def LoweredOK (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block) (nb nb' : Nat) : Prop :=
  nb ≤ E ∧ E < nb' ∧ (∀ b ∈ B, nb ≤ b.id ∧ b.id < nb') ∧ (B.map IR.Block.id).Nodup ∧
    (∃ b ∈ B, b.id = E ∧ b.params = EP)

/-- Two lists of blocks whose ids are split by `mid` (the first below it, the second at or above)
have distinct ids together. -/
theorem nodup_ranges (A B : List IR.Block) (mid : Nat)
    (hA : ∀ b ∈ A, b.id < mid) (hB : ∀ b ∈ B, mid ≤ b.id)
    (hAn : (A.map IR.Block.id).Nodup) (hBn : (B.map IR.Block.id).Nodup) :
    ((A ++ B).map IR.Block.id).Nodup := by
  rw [List.map_append, List.nodup_append]
  refine ⟨hAn, hBn, ?_⟩
  intro x hx y hy hxy
  obtain ⟨a, ha, hax⟩ := List.mem_map.mp hx
  obtain ⟨b, hb, hby⟩ := List.mem_map.mp hy
  have h1 : a.id < mid := hA a ha
  have h2 : mid ≤ b.id := hB b hb
  have hlt : a.id < b.id := Nat.lt_of_lt_of_le h1 h2
  rw [hax, hby] at hlt
  exact Nat.lt_irrefl _ (hxy ▸ hlt)

mutual

theorem lowerStmt_spec : ∀ (s : Stmt) (K : IR.BlockId × List IR.ValueId) (L n nb : Nat)
    (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block) (n' nb' : Nat),
    lowerStmt s K L n nb = some (E, EP, B, n', nb') →
    EP = List.range' n L ∧ LoweredOK E EP B nb nb'
  | .straight ops, (kid, kps), L, n, nb, E, EP, B, n', nb', h => by
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
        refine ⟨rfl, ?_⟩
        unfold LoweredOK
        refine ⟨Nat.le_refl _, Nat.lt_succ_self _, ?_, by simp, ?_⟩
        · intro b hb
          simp at hb
          subst hb
          exact ⟨Nat.le_refl _, Nat.lt_succ_self _⟩
        · exact ⟨_, List.mem_singleton_self _, rfl, rfl⟩
  | .ifElse cond t el, (kid, kps), L, n, nb, E, EP, B, n', nb', h => by
    unfold lowerStmt at h
    cases hl : V4Straight.lower cond [] (List.range' n L) (n + L) with
    | none => simp [hl] at h
    | some q =>
      obtain ⟨irC, fin, Q, m⟩ := q
      rcases fin with _ | ⟨c, _ | ⟨c2, tl⟩⟩
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
            have tOK := (lowerSeq_spec t (kid, kps) L m (nb + 1) tE tEP tB m2 nb2 ht).1
            have eOK := (lowerSeq_spec el (kid, kps) L m2 nb2 eE eEP eB m3 nb3 he).1
            unfold LoweredOK at tOK eOK
            obtain ⟨tlo, tlh, tlr, tlnd, -⟩ := tOK
            obtain ⟨elo, elh, elr, elnd, -⟩ := eOK
            -- the arms sit above the condition's block and below the join of the two ranges
            have hnb2 : nb + 1 ≤ nb2 := Nat.le_trans tlo (Nat.le_of_lt tlh)
            have hnb3 : nb < nb3 := Nat.lt_of_lt_of_le (Nat.lt_succ_self nb) (Nat.le_trans hnb2 (Nat.le_trans elo (Nat.le_of_lt elh)))
            refine ⟨rfl, ?_⟩
            unfold LoweredOK
            refine ⟨Nat.le_refl _, hnb3, ?_, ?_, ⟨_, List.mem_cons_self, rfl, rfl⟩⟩
            · intro b hb
              simp only [List.mem_cons, List.mem_append] at hb
              rcases hb with rfl | hb | hb
              · exact ⟨Nat.le_refl _, hnb3⟩
              · exact ⟨Nat.le_trans (Nat.le_succ nb) (tlr b hb).1, Nat.lt_trans (tlr b hb).2 (Nat.lt_of_le_of_lt elo elh)⟩
              · exact ⟨Nat.le_trans (Nat.le_trans (Nat.le_succ nb) hnb2) (elr b hb).1, (elr b hb).2⟩
            · simp only [List.map_cons]
              refine List.nodup_cons.mpr ⟨?_, ?_⟩
              · intro hmem
                rw [List.map_append, List.mem_append] at hmem
                rcases hmem with hmem | hmem
                · obtain ⟨b, hb, hbi⟩ := List.mem_map.mp hmem
                  have h3 : nb < b.id := Nat.lt_of_lt_of_le (Nat.lt_succ_self nb) (tlr b hb).1
                  rw [hbi] at h3
                  exact Nat.lt_irrefl _ h3
                · obtain ⟨b, hb, hbi⟩ := List.mem_map.mp hmem
                  have h3 : nb < b.id := Nat.lt_of_lt_of_le (Nat.lt_of_lt_of_le (Nat.lt_succ_self nb) hnb2) (elr b hb).1
                  rw [hbi] at h3
                  exact Nat.lt_irrefl _ h3
              · exact nodup_ranges tB eB nb2 (fun b hb => (tlr b hb).2) (fun b hb => (elr b hb).1) tlnd elnd
      · simp [hl] at h

theorem lowerSeq_spec : ∀ (ss : List Stmt) (K : IR.BlockId × List IR.ValueId) (L n nb : Nat)
    (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block) (n' nb' : Nat),
    lowerSeq ss K L n nb = some (E, EP, B, n', nb') →
    LoweredOK E EP B nb nb' ∧ EP.length = L
  | [], (kid, _), L, n, nb, E, EP, B, n', nb', h => by
    simp only [lowerSeq, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
    refine ⟨?_, by simp⟩
    unfold LoweredOK
    refine ⟨Nat.le_refl _, Nat.lt_succ_self _, ?_, by simp, ⟨_, List.mem_singleton_self _, rfl, rfl⟩⟩
    intro b hb
    simp at hb
    subst hb
    exact ⟨Nat.le_refl _, Nat.lt_succ_self _⟩
  | s :: rest, K, L, n, nb, E, EP, B, n', nb', h => by
    unfold lowerSeq at h
    cases hr : lowerSeq rest K L n nb with
    | none => simp [hr] at h
    | some qr =>
      obtain ⟨rE, rEP, rB, n1, nb1⟩ := qr
      cases hs : lowerStmt s (rE, rEP) L n1 nb1 with
      | none => simp [hr, hs] at h
      | some qs =>
        obtain ⟨sE, sEP, sB, n2, nb2⟩ := qs
        simp only [hr, hs, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
        have rOK := (lowerSeq_spec rest K L n nb rE rEP rB n1 nb1 hr).1
        have sSpec := lowerStmt_spec s (rE, rEP) L n1 nb1 sE sEP sB n2 nb2 hs
        have sLen : sEP.length = L := by
          rw [sSpec.1]
          simp
        have sOK := sSpec.2
        unfold LoweredOK at rOK sOK
        obtain ⟨rlo, rlh, rlr, rlnd, -⟩ := rOK
        obtain ⟨slo, slh, slr, slnd, slex⟩ := sOK
        have hnb1 : nb ≤ nb1 := Nat.le_trans rlo (Nat.le_of_lt rlh)
        refine ⟨?_, sLen⟩
        unfold LoweredOK
        refine ⟨Nat.le_trans hnb1 slo, slh, ?_, ?_, ?_⟩
        · intro b hb
          rw [List.mem_append] at hb
          rcases hb with hb | hb
          · exact ⟨Nat.le_trans hnb1 (slr b hb).1, (slr b hb).2⟩
          · exact ⟨(rlr b hb).1, Nat.lt_trans (rlr b hb).2 (Nat.lt_of_le_of_lt slo slh)⟩
        · have hnd := nodup_ranges rB sB nb1 (fun b hb => (rlr b hb).2) (fun b hb => (slr b hb).1)
            rlnd slnd
          exact (List.perm_append_comm.map _).nodup_iff.mpr hnd
        · obtain ⟨b, hb, hbi, hbp⟩ := slex
          exact ⟨b, List.mem_append_left _ hb, hbi, hbp⟩

end

end Nested
end TptWasm
