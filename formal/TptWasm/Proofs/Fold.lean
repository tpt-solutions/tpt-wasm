import TptWasm.Proofs.Threading

/-!
# CFG simplification: folding a branch on a known constant

The last of the pass's four steps the model covers at the block level. A conditional
branch whose condition is a known constant `k` becomes the branch that `k` selects
(`tpt-wasm-opt::graph`, `fold_constant_conditions`): the then-arm when `k` is non-zero,
the else-arm otherwise.

## What is proved

* `termResult_foldCond`: a terminator, folded on a condition that is bound to `k` in the
  state, gives the same result as the terminator itself, at every fuel.
* `runBlocks_foldCond`: the same for a whole block's run, given that the condition is
  bound to `k` after the block's body has run. The body may define the constant itself.

## What is not proved

The whole-function statement. It needs the condition to be bound to `k` every time the
folded block's terminator is reached from the entry. That holds in compiled code because
the constant's definition dominates the branch and an SSA value is bound once: the
verifier checks both. The Lean model has no dominance or run-level binding invariant yet,
so the invariant is the explicit premise of any whole-function claim, not something this
file derives.
-/

namespace TptWasm
namespace IR

/-- The terminator the fold produces: a condition branch on `c` is replaced by the arm the
constant `k` selects; anything else is unchanged. -/
def foldCond (c : ValueId) (k : Int) (T : Terminator) : Terminator :=
  match T with
  | .condBranch c' (t1, a1) (t2, a2) =>
    if c' = c then
      if k = 0 then .branch t2 a2 else .branch t1 a1
    else T
  | _ => T

/-- A folded terminator gives the same result as the original, when the condition is bound
to the constant the fold uses. -/
theorem termResult_foldCond (blocks : List Block) (n : Nat) (c : ValueId) (k : Int)
    (T : Terminator) (s : State) (tr : List Event) (hc : s.values c = .i32 k) :
    termResult blocks n (foldCond c k T) s tr = termResult blocks n T s tr := by
  cases T with
  | ret values => rfl
  | trap t => rfl
  | unreachable => rfl
  | branch t a => rfl
  | condBranch c' a b =>
    obtain ⟨t1, a1⟩ := a
    obtain ⟨t2, a2⟩ := b
    by_cases hcc : c' = c
    · subst hcc
      simp only [foldCond, termResult, hc, intPayload, ↓reduceIte]
      by_cases hk : k = 0
      · simp [hk]
      · simp [hk]
    · simp only [foldCond, hcc, ↓reduceIte]

/-- The block-level statement: running a block whose branch is folded gives the same result
as running it unfolded, given that the condition is bound to the constant after the body. -/
theorem runBlocks_foldCond (blocks : List Block) (n : Nat) (b : Block) (c : ValueId) (k : Int)
    (s : State) (tr : List Event)
    (hpost : ∀ (s' : State) (tr' : List Event), runBodyR b.instrs s tr = .inr (s', tr') →
      s'.values c = .i32 k) :
    runBlocks blocks (n + 1) { b with terminator := foldCond c k b.terminator } s tr =
      runBlocks blocks (n + 1) b s tr := by
  rw [runBlocks_succ, runBlocks_succ]
  dsimp only
  generalize hbody : runBodyR b.instrs s tr = res
  rcases res with ⟨t1, x1⟩ | ⟨s', tr'⟩
  · rfl
  · exact termResult_foldCond blocks n c k b.terminator s' tr' (hpost s' tr' hbody)

end IR
end TptWasm
