import TptWasm.Nested.Simulate

/-!
# A nested program, end to end

A program is a statement list followed by a straight-line return expression. Its lowering is a
function with three kinds of block: an entry block (id 0) that copies the function's initial locals
into the body's parameters, the body's blocks, and the return block (id 1) that takes the locals,
runs the return expression, and returns its values.

`nested_ir_run` says the lowered function returns the values and events the Wasm run returns.
`nested_machine` says the machine, run over the program's flat body, reaches its end with the same
events and operand stack. `nested_triangle` states both for one run.
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond IfControl V4Diamond

/-- The entry block: branches to the body's entry, passing the initial locals. -/
def topBlk (bE : IR.BlockId) (L : Nat) : IR.Block :=
  { id := 0, params := [], instrs := [], terminator := .branch bE (List.range' 0 L) }

/-- The return block: takes the locals, runs the return expression, and returns its values. -/
def retBlk (L : Nat) (fin : List IR.ValueId) (irRet : List IR.Instr) : IR.Block :=
  { id := 1, params := List.range' L L, instrs := irRet, terminator := .ret fin }

/-- Lower a program with `L` locals: the body continues to the return block. -/
def lowerProg (body : List Stmt) (ret : List Instr) (L : Nat) : Option IR.Function :=
  match V4Straight.lower ret [] (List.range' L L) (L + L) with
  | some (irRet, fin, _, m) =>
    match lowerSeq body (1, List.range' L L) L m 2 with
    | some (bE, _, bB, _, _) =>
      some { params := List.range' 0 L, locals := [], declared := [],
             blocks := topBlk bE L :: retBlk L fin irRet :: bB, entry := 0 }
    | none => none
  | none => none

/-- The IR run of the lowered program returns the Wasm run's events and values. -/
theorem nested_ir_run (body : List Stmt) (ret : List Instr) (L : Nat) (lf : IR.Function)
    (hl : lowerProg body ret L = some lf) (c c1 c2 : Configuration) (e1 e2 : List Event)
    (s : IR.State) (hbc : BoundCorr c s (List.range' 0 L)) (hg : V4Diamond.GlobalsI32 c.store)
    (hsup : SupSeq body) (_hsupR : ∀ i ∈ ret, V4Diamond.Supported i)
    (hrun1 : runSeq body c = some (c1, e1)) (hrun2 : runWasm ret c1 = some (c2, e2)) :
    ∃ n0, IR.runFunction lf (n0 + 2) s = .returned (e1 ++ e2) c2.operandStack := by
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
      -- the blocks of the program, and their ids
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
      -- the return block, its lookup, and the entry of the body
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
      -- the body reaches the return block
      obtain ⟨n0, st1, hG1, hBC1⟩ := sim_seq body 1 (List.range' L L) (retBlk L fin irRet) L m 2 bE
        bEP bB m2 nb2 (topBlk bE L :: retBlk L fin irRet :: bB) c c1 e1
        (s.bindAll bEP ((List.range' 0 L).map s.values)) hb hsup hkL hkN hKfb hkb
        (fun b hb => hGmem b hb) hnd hg hbcTop hrun1 []
      simp only [List.nil_append] at hG1
      -- the return block runs the return expression
      have hcorrRet : Corresponds c1 st1 [] (List.range' L L) (L + L) :=
        corresponds_of_bound (n := L + L) hBC1 (fun id hid => by
          rw [List.mem_range'_1] at hid
          exact hid.2)
      have hgl1 := runSeq_globals body c c1 e1 hsup hg hrun1
      have sim := V4Straight.simulation ret c1 c2 e2 st1 [] (List.range' L L) (L + L) irRet fin Q m
        hcorrRet hgl1 hr hrun2
      obtain ⟨st2, hrb2, hcorr2⟩ := sim e1
      have hB2 := V4Diamond.runBodyR_of_runBody irRet st1 e1 st2 (e1 ++ e2) hrb2
      have hret : IR.runBlocks (topBlk bE L :: retBlk L fin irRet :: bB) 1 (retBlk L fin irRet) st1 e1 =
          .returned (e1 ++ e2) (fin.map st2.values) :=
        V4Diamond.runBlocks_ret _ 0 (retBlk L fin irRet) st1 st2 e1 (e1 ++ e2) fin hB2 rfl
      -- the body's run, then the return block's, with the fuel the body needs
      have hrunB := GotoN_runBlocks hG1 bblk (retBlk L fin irRet) 1 hbfb hKfb
      -- the entry block branches into the body
      refine ⟨n0, ?_⟩
      have hstart := runFunction_entry (IR.Function.mk (List.range' 0 L) [] [] (topBlk bE L :: retBlk L fin irRet :: bB) 0) (n0 + 2) s (topBlk bE L) rfl
      rw [hstart]
      show IR.runBlocks (topBlk bE L :: retBlk L fin irRet :: bB) (n0 + 1 + 1) (topBlk bE L) s [] = _
      rw [V4Diamond.runBlocks_branch (topBlk bE L :: retBlk L fin irRet :: bB) (n0 + 1)
        (topBlk bE L) bblk s s [] [] bE (List.range' 0 L) rfl rfl hbfb]
      rw [hbpar, hrunB, hret, hcorr2.1]

/-- The machine runs the flat program: the body, then the return expression, and reaches the end of
the flat code with the events and stack the Wasm run gives. -/
theorem nested_machine (body : List Stmt) (ret : List Instr) (c c1 c2 : Configuration)
    (e1 e2 : List Event) (f : Frame) (fn : FunctionState)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = flatSeq body ++ ret) (hsup : SupSeq body)
    (hsupR : ∀ i ∈ ret, V4Diamond.Supported i)
    (hrun1 : runSeq body c = some (c1, e1)) (hrun2 : runWasm ret c1 = some (c2, e2)) :
    ∃ cEnd, MReach (c.setActivePc 0) cEnd (e1 ++ e2) ∧ cEnd.currentInstr? = none ∧
      cEnd.operandStack = c2.operandStack := by
  obtain ⟨hfs1, hfr1⟩ := runSeq_frames body c c1 e1 hsup hrun1
  obtain ⟨g, hg1, hgi⟩ := hfr1 f hf
  have hfn1 : c1.store.function? g.functionIndex = some fn := by
    rw [function?_of_functions c.store c1.store hfs1, hgi]
    exact hfn
  have hA := machine_seq body 0 [] ret c c1 e1 f fn (by simp) hsup hf hfn
    (by rw [hbody]; simp) hrun1
  simp only [Nat.zero_add] at hA
  have hB := MachineDiamond.segment ret (flatSeq body).length (flatSeq body) [] c1 c2 e2 g fn rfl
    hsupR hg1 hfn1 (by rw [hbody]; simp) hrun2
  obtain ⟨hfs2, hfr2⟩ := runWasm_frames ret c1 c2 e2 hsupR hrun2
  obtain ⟨g2, hg2, hgi2⟩ := hfr2 g hg1
  have hfn2 : c2.store.function? g2.functionIndex = some fn := by
    rw [function?_of_functions c.store c2.store (hfs2.trans hfs1), hgi2, hgi]
    exact hfn
  refine ⟨c2.setActivePc ((flatSeq body).length + ret.length), hA.trans hB, ?_,
    setActivePc_operandStack' _ _⟩
  rw [currentInstr_of _ { g2 with pc := (flatSeq body).length + ret.length } fn _
    (by rw [activeFrame_setActivePc, hg2]; rfl)
    (by rw [setActivePc_store]; exact hfn2) (by rw [hbody])]
  apply List.getElem?_eq_none
  simp

/-- The triangle for nested programs: the lowered program's IR run and the machine's run of the flat
program agree with the Wasm run, on events and on the returned values. -/
theorem nested_triangle (body : List Stmt) (ret : List Instr) (L : Nat) (lf : IR.Function)
    (hl : lowerProg body ret L = some lf) (c c1 c2 : Configuration) (e1 e2 : List Event)
    (s : IR.State) (f : Frame) (fn : FunctionState)
    (hbc : BoundCorr c s (List.range' 0 L)) (hg : V4Diamond.GlobalsI32 c.store)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = flatSeq body ++ ret) (hsup : SupSeq body)
    (hsupR : ∀ i ∈ ret, V4Diamond.Supported i)
    (hrun1 : runSeq body c = some (c1, e1)) (hrun2 : runWasm ret c1 = some (c2, e2)) :
    (∃ cEnd, MReach (c.setActivePc 0) cEnd (e1 ++ e2) ∧ cEnd.currentInstr? = none ∧
      cEnd.operandStack = c2.operandStack) ∧
    ∃ n0, IR.runFunction lf (n0 + 2) s = .returned (e1 ++ e2) c2.operandStack :=
  ⟨nested_machine body ret c c1 c2 e1 e2 f fn hf hfn hbody hsup hsupR hrun1 hrun2,
    nested_ir_run body ret L lf hl c c1 c2 e1 e2 s hbc hg hsup hsupR hrun1 hrun2⟩

end Nested
end TptWasm
