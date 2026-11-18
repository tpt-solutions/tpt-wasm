import TptWasm.Nested.Syntax

/-!
# Nested statements keep the frame and the function table

A successful run of a supported statement or statement list leaves the store's function table
alone and keeps the active frame's function index. These are the facts the machine proof needs at
every step of a nested run.
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond

mutual

theorem runStmt_frames : ∀ (s : Stmt) (c c' : Configuration) (e : List Event),
    SupStmt s → runStmt s c = some (c', e) →
    c'.store.functions = c.store.functions ∧
      ∀ f, c.activeFrame? = some f → ∃ g, c'.activeFrame? = some g ∧ g.functionIndex = f.functionIndex
  | .straight ops, c, c', e, hs, h => by
    unfold runStmt at h
    cases hw : runWasm ops c with
    | none => simp [hw] at h
    | some q =>
      obtain ⟨c1, e1⟩ := q
      simp only [hw] at h
      split at h
      · simp only [Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, -⟩ := h
        exact runWasm_frames ops c c1 e1 hs hw
      · simp at h
  | .ifElse cond t el, c, c', e, hs, h => by
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
              obtain ⟨hf1, hr1⟩ := runWasm_frames cond c c1 e1 hsc hcond
              have harmF : c3.store.functions = c1.store.functions ∧
                  ∀ f, c1.activeFrame? = some f → ∃ g, c3.activeFrame? = some g ∧
                    g.functionIndex = f.functionIndex := by
                by_cases hv : v = 0
                · simp only [hv, ↓reduceIte] at harm
                  exact runSeq_frames el { c1 with operandStack := [] } c3 e2 hse harm
                · simp only [hv, ↓reduceIte] at harm
                  exact runSeq_frames t { c1 with operandStack := [] } c3 e2 hst harm
              obtain ⟨hf3, hr3⟩ := harmF
              refine ⟨hf3.trans hf1, fun f hf => ?_⟩
              obtain ⟨g, hg, hgi⟩ := hr1 f hf
              obtain ⟨g3, hg3, hgi3⟩ := hr3 g hg
              exact ⟨g3, hg3, hgi3.trans hgi⟩
            next => simp at h
        next => simp at h
    · simp [hstk] at h

theorem runSeq_frames : ∀ (ss : List Stmt) (c c' : Configuration) (e : List Event),
    SupSeq ss → runSeq ss c = some (c', e) →
    c'.store.functions = c.store.functions ∧
      ∀ f, c.activeFrame? = some f → ∃ g, c'.activeFrame? = some g ∧ g.functionIndex = f.functionIndex
  | [], c, c', e, _, h => by
    simp only [runSeq, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    exact ⟨rfl, fun f hf => ⟨f, hf, rfl⟩⟩
  | s :: rest, c, c', e, hs, h => by
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
        obtain ⟨f1, r1⟩ := runStmt_frames s c c1 e1 hs1 h1
        obtain ⟨f2, r2⟩ := runSeq_frames rest c1 c2 e2 hs2 h2
        refine ⟨f2.trans f1, fun f hf => ?_⟩
        obtain ⟨g, hg, hgi⟩ := r1 f hf
        obtain ⟨g2, hg2, hgi2⟩ := r2 g hg
        exact ⟨g2, hg2, hgi2.trans hgi⟩

end

end Nested
end TptWasm
