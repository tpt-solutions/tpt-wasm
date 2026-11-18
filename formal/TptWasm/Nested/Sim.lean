import TptWasm.Nested.Goto

/-!
# Helpers for the nested simulation

`BoundCorr c s P` is the correspondence a block entry needs: the configuration is at an empty
stack, its store is the IR state's, its frame's locals are the values of the parameters `P`, and
every parameter holds an `i32`. The simulation carries this from block to block; entering a block
through an edge keeps it (`boundCorr_bind`).

`runStmt_globals` and `runSeq_globals` keep the global-type invariant through a nested run, which
the straight-line simulation needs at every step.
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond V4Diamond

/-- The configuration `c` and the IR state `s` agree at a block whose parameters are `P`. -/
def BoundCorr (c : Configuration) (s : IR.State) (P : List IR.ValueId) : Prop :=
  c.operandStack = [] ∧ c.store = s.store ∧
    (∃ f, c.activeFrame? = some f ∧ f.locals = P.map s.values) ∧
    (∀ id ∈ P, ∃ x : Int, s.values id = .i32 x)

/-- A correspondence with a bound on the parameters gives a block correspondence. -/
theorem boundCorr_of_corresponds {c : Configuration} {s : IR.State} {P : List IR.ValueId} {n : Nat}
    (h : Corresponds c s [] P n) : BoundCorr c s P := by
  obtain ⟨hstk, -, -, hst, ⟨⟨f, hf, hfl⟩, -, hi⟩⟩ := h
  exact ⟨by simpa using hstk, hst, ⟨f, hf, hfl⟩, hi⟩

/-- Entering a block through an edge that carries the locals: the block's parameters read back the
locals, the stack is empty, and the store is unchanged. -/
theorem boundCorr_bind {c : Configuration} {s : IR.State} {P kps : List IR.ValueId}
    (hc : BoundCorr c s P) (hn : kps.Nodup) (hl : kps.length = P.length) :
    BoundCorr c (s.bindAll kps (P.map s.values)) kps := by
  obtain ⟨hstk, hst, ⟨f, hf, hfl⟩, hi⟩ := hc
  refine ⟨hstk, ?_, ⟨f, hf, ?_⟩, ?_⟩
  · rw [bindAll_store]
    exact hst
  · rw [bindAll_map kps (P.map s.values) s hn (by simp [hl])]
    exact hfl
  · intro id hid
    have hmem : (s.bindAll kps (P.map s.values)).values id ∈
        kps.map (s.bindAll kps (P.map s.values)).values :=
      List.mem_map_of_mem hid
    rw [bindAll_map kps (P.map s.values) s hn (by simp [hl])] at hmem
    obtain ⟨a, ha, heq⟩ := List.mem_map.mp hmem
    obtain ⟨x, hx⟩ := hi a ha
    exact ⟨x, by rw [← heq, hx]⟩

/-- A statement list's entry parameters are a fresh range, for any statement list. -/
theorem lowerSeq_params (ss : List Stmt) (K : IR.BlockId × List IR.ValueId) (L n nb : Nat)
    (E : IR.BlockId) (EP : List IR.ValueId) (B : List IR.Block) (n' nb' : Nat)
    (h : lowerSeq ss K L n nb = some (E, EP, B, n', nb')) :
    ∃ n0, EP = List.range' n0 L := by
  cases ss with
  | nil =>
    simp only [lowerSeq, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨-, hEP, -, -, -⟩ := h
    exact ⟨n, hEP.symm⟩
  | cons s rest =>
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
        obtain ⟨rfl, rfl, -⟩ := h
        exact ⟨n1, (lowerStmt_spec s (rE, rEP) L n1 nb1 sE sEP sB n2 nb2 hs).1⟩

/-- A block of a lowering with entry `E` is in the global list, and the global lookup finds it. -/
theorem entry_lookup {G : List IR.Block} {E : IR.BlockId} {EP : List IR.ValueId} {B : List IR.Block}
    {nb nb' : Nat} (hOK : LoweredOK E EP B nb nb') (hG : ∀ b ∈ B, b ∈ G)
    (hnd : (G.map IR.Block.id).Nodup) :
    ∃ blk, IR.findBlock G E = some blk ∧ blk.params = EP ∧ blk ∈ B := by
  unfold LoweredOK at hOK
  obtain ⟨-, -, -, -, blk, hblk, hid, hpar⟩ := hOK
  refine ⟨blk, ?_, hpar, hblk⟩
  rw [← hid]
  exact findBlock_of_mem G blk (hG blk hblk) hnd

/-- Global types are preserved by a successful statement list step. -/
theorem runWasm_globals_of_sup (xs : List Instr) (c c' : Configuration) (evs : List Event)
    (hs : ∀ i ∈ xs, V4Diamond.Supported i) (hg : V4Diamond.GlobalsI32 c.store)
    (h : runWasm xs c = some (c', evs)) : V4Diamond.GlobalsI32 c'.store :=
  V4Diamond.runWasm_globals xs c c' evs hs hg h

mutual

theorem runStmt_globals : ∀ (s : Stmt) (c c' : Configuration) (e : List Event),
    SupStmt s → V4Diamond.GlobalsI32 c.store → runStmt s c = some (c', e) →
    V4Diamond.GlobalsI32 c'.store
  | .straight ops, c, c', e, hs, hg, h => by
    unfold runStmt at h
    cases hw : runWasm ops c with
    | none => simp [hw] at h
    | some q =>
      obtain ⟨c1, e1⟩ := q
      simp only [hw] at h
      split at h
      · simp only [Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, -⟩ := h
        exact runWasm_globals_of_sup ops c c1 e1 hs hg hw
      · simp at h
  | .ifElse cond t el, c, c', e, hs, hg, h => by
    obtain ⟨hsc, hst, hse⟩ := hs
    unfold runStmt at h
    by_cases hstk : c.operandStack = []
    · simp only [hstk, ↓reduceIte] at h
      cases hcond : runWasm cond c with
      | none => simp [hcond] at h
      | some q =>
        obtain ⟨c1, e1⟩ := q
        simp only [hcond] at h
        split at h
        next v hop =>
          cases harm : (if v = 0 then runSeq el { c1 with operandStack := [] }
              else runSeq t { c1 with operandStack := [] }) with
          | none => simp [harm] at h
          | some q2 =>
            obtain ⟨c3, e2⟩ := q2
            simp only [harm] at h
            split at h
            next hemp =>
              simp only [Option.some.injEq, Prod.mk.injEq] at h
              obtain ⟨rfl, -⟩ := h
              have hg1 := runWasm_globals_of_sup cond c c1 e1 hsc hg hcond
              by_cases hv : v = 0
              · simp only [hv, ↓reduceIte] at harm
                exact runSeq_globals el { c1 with operandStack := [] } c3 e2 hse hg1 harm
              · simp only [hv, ↓reduceIte] at harm
                exact runSeq_globals t { c1 with operandStack := [] } c3 e2 hst hg1 harm
            next => simp at h
        next => simp at h
    · simp [hstk] at h

theorem runSeq_globals : ∀ (ss : List Stmt) (c c' : Configuration) (e : List Event),
    SupSeq ss → V4Diamond.GlobalsI32 c.store → runSeq ss c = some (c', e) →
    V4Diamond.GlobalsI32 c'.store
  | [], c, c', e, _, hg, h => by
    simp only [runSeq, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    exact hg
  | s :: rest, c, c', e, hs, hg, h => by
    obtain ⟨hs1, hs2⟩ := hs
    unfold runSeq at h
    cases h1 : runStmt s c with
    | none => simp [h1] at h
    | some q =>
      obtain ⟨c1, e1⟩ := q
      simp only [h1] at h
      cases h2 : runSeq rest c1 with
      | none => simp [h2] at h
      | some q2 =>
        obtain ⟨c2, e2⟩ := q2
        simp only [h2, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, -⟩ := h
        exact runSeq_globals rest c1 c2 e2 hs2 (runStmt_globals s c c1 e1 hs1 hg h1) h2

end

end Nested
end TptWasm

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond V4Diamond

/-- A block correspondence gives the straight-line correspondence, given a bound on the parameters. -/
theorem corresponds_of_bound {c : Configuration} {s : IR.State} {P : List IR.ValueId} {n : Nat}
    (h : BoundCorr c s P) (hb : ∀ id ∈ P, id < n) : Corresponds c s [] P n := by
  obtain ⟨hstk, hst, ⟨f, hf, hfl⟩, hi⟩ := h
  exact ⟨by simp [hstk], by simp, by simp, hst, ⟨⟨f, hf, hfl⟩, hb, hi⟩⟩

/-- The locals of a correspondence, with the stack ignored: the configuration with an empty stack
corresponds to the same state at the same locals. -/
theorem boundCorr_of_corresponds_nostack {c : Configuration} {s : IR.State} {stack : List IR.ValueId}
    {P : List IR.ValueId} {n : Nat} (h : Corresponds c s stack P n) :
    BoundCorr { c with operandStack := [] } s P := by
  obtain ⟨-, -, -, hst, ⟨⟨f, hf, hfl⟩, -, hi⟩⟩ := h
  exact ⟨rfl, hst, ⟨f, hf, hfl⟩, hi⟩

/-- A straight-line statement's run is its operator run, with empty stacks at both ends. -/
theorem runStmt_straight_extract {ops : List Instr} {c c' : Configuration} {e : List Event}
    (h : runStmt (.straight ops) c = some (c', e)) :
    runWasm ops c = some (c', e) ∧ c.operandStack = [] ∧ c'.operandStack = [] := by
  unfold runStmt at h
  cases hw : runWasm ops c with
  | none => simp [hw] at h
  | some q =>
    obtain ⟨c1, e1⟩ := q
    simp only [hw] at h
    split at h
    next h12 =>
      simp only [Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      exact ⟨rfl, h12.1, h12.2⟩
    next => simp at h

end Nested
end TptWasm
