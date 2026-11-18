import TptWasm.Nested.TrapSim
import TptWasm.Nested.Program

/-!
# A trapping nested program, end to end

If a nested body traps (`TrapSeq`), its lowered program, run from the entry block, returns
`trapped` with the events the Wasm run performed before the trap and the trap itself. The body's
block graph traps (`GotoT`, from the body's entry block), and the entry block branches into it.
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond IfControl V4Diamond

/-- A nested body that traps makes its lowered program trap with the same events and trap. -/
theorem nested_trap (k : Nat) (body : List Stmt) (ret : List Instr) (L : Nat) (lf : IR.Function)
    (hl : lowerProg body ret L = some lf) (c : Configuration) (evs : List Event) (t : Trap)
    (s : IR.State) (hbc : BoundCorr c s (List.range' 0 L)) (hg : V4Diamond.GlobalsI32 c.store)
    (hsup : SupSeqL body) (htrap : TrapSeq k body c evs t) :
    ∃ n0, IR.runFunction lf (n0 + 2) s = .trapped evs t := by
  unfold lowerProg at hl
  cases hr : V4Straight.lower ret [] (List.range' L L) (L + L) with
  | none => simp [hr] at hl
  | some q =>
    obtain ⟨irRet, fin, Q, m⟩ := q
    cases hb : lowerSeq body (1, List.range' L L) L m 2 with
    | none => simp [hr, hb] at hl
    | some qb =>
      obtain ⟨bE, bEP, bB, m2, nb2⟩ := qb
      simp only [hr, hb, Option.some.injEq] at hl
      subst hl
      have hbOK := lowerSeq_spec body (1, List.range' L L) L m 2 bE bEP bB m2 nb2 hb
      have hbParams := lowerSeq_params body (1, List.range' L L) L m 2 bE bEP bB m2 nb2 hb
      obtain ⟨n0b, hbEPeq⟩ := hbParams
      have hGmem : ∀ b ∈ bB, b ∈ (topBlk bE L :: retBlk L fin irRet :: bB) :=
        fun b hb => List.mem_cons_of_mem _ (List.mem_cons_of_mem _ hb)
      have hbids : ∀ b ∈ bB, 2 ≤ b.id := fun b hb => (hbOK.1.2.2.1 b hb).1
      have hids : ((topBlk bE L :: retBlk L fin irRet :: bB).map IR.Block.id) =
          0 :: 1 :: bB.map IR.Block.id := by
        simp [topBlk, retBlk]
      have hnd : ((topBlk bE L :: retBlk L fin irRet :: bB).map IR.Block.id).Nodup := by
        rw [hids]
        refine List.nodup_cons.mpr ⟨?_, List.nodup_cons.mpr ⟨?_, hbOK.1.2.2.2.1⟩⟩
        · intro hmem
          rcases List.mem_cons.mp hmem with h0 | hmem
          · exact absurd h0 (by decide)
          · obtain ⟨b, hb, hbi⟩ := List.mem_map.mp hmem
            have := hbids b hb
            rw [hbi] at this
            exact absurd this (by decide)
        · intro hmem
          obtain ⟨b, hb, hbi⟩ := List.mem_map.mp hmem
          have := hbids b hb
          rw [hbi] at this
          exact absurd this (by decide)
      have hKfb : IR.findBlock (topBlk bE L :: retBlk L fin irRet :: bB) 1 =
          some (retBlk L fin irRet) :=
        findBlock_of_mem _ _ (List.mem_cons_of_mem _ (List.mem_cons_self)) hnd
      have hkL : (List.range' L L).length = L := by simp
      have hkN : (List.range' L L).Nodup := List.nodup_range' (s := L) (n := L)
      have hkb : (retBlk L fin irRet).params = List.range' L L := rfl
      obtain ⟨bblk, hbfb, hbpar, -⟩ := entry_lookup hbOK.1 hGmem hnd
      have hbcTop : BoundCorr c (s.bindAll bEP ((List.range' 0 L).map s.values)) bEP :=
        boundCorr_bind hbc (by rw [hbEPeq]; exact List.nodup_range' (s := n0b) (n := L))
          (by rw [hbOK.2]; simp)
      -- the body traps, reaching its trap from the entry block of the body
      have hgoto := trap_seq k body 1 (List.range' L L) (retBlk L fin irRet) L m 2 bE bEP bB m2 nb2
        (topBlk bE L :: retBlk L fin irRet :: bB) c evs t (s.bindAll bEP ((List.range' 0 L).map s.values))
        hb hsup hkL hkN hKfb hkb hGmem hnd hg hbcTop htrap []
      obtain ⟨m', hm'⟩ := GotoT_runBlocks hgoto bblk hbfb
      simp only [List.nil_append] at hm'
      cases m' with
      | zero => simp [IR.runBlocks] at hm'
      | succ n0 =>
        refine ⟨n0, ?_⟩
        have hstart := runFunction_entry
          (IR.Function.mk (List.range' 0 L) [] [] (topBlk bE L :: retBlk L fin irRet :: bB) 0)
          (n0 + 2) s (topBlk bE L) rfl
        rw [hstart]
        show IR.runBlocks (topBlk bE L :: retBlk L fin irRet :: bB) (n0 + 1 + 1) (topBlk bE L) s []
          = _
        rw [V4Diamond.runBlocks_branch (topBlk bE L :: retBlk L fin irRet :: bB) (n0 + 1)
          (topBlk bE L) bblk s s [] [] bE (List.range' 0 L) rfl rfl hbfb]
        rw [hbpar]
        exact hm'

end Nested
end TptWasm
