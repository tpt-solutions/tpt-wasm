import TptWasm.IR

/-!
# Dominance over the block graph

The model's notion of control flow between blocks, and dominance on it. This is the
property the verifier checks (`tpt-wasm-ir`'s `verify_dominance`): every use of a value
is dominated by its definition. Constant-condition folding relies on it.

* `Edge f X Y`: some block of `f` named `X` branches to `Y`.
* `RA f D Y`: `Y` can be reached from the entry without passing through `D`.
* `Dom f D Y`: `D` dominates `Y`, that is, `Y` cannot be reached without `D`. It is the
  negation of `RA f D Y`.
-/

namespace TptWasm
namespace IR

/-- Some block of `f` named `X` has a branch to `Y`. -/
def Edge (f : Function) (X Y : BlockId) : Prop :=
  ∃ B ∈ f.blocks, B.id = X ∧ ∃ p ∈ B.terminator.targets, p.1 = Y

/-- The blocks reachable from the entry without passing through `D`. -/
inductive RA (f : Function) (D : BlockId) : BlockId → Prop
  | entry : f.entry ≠ D → RA f D f.entry
  | step {X Y : BlockId} : RA f D X → Edge f X Y → Y ≠ D → RA f D Y

/-- `D` dominates `Y`: every path from the entry to `Y` passes `D`. -/
def Dom (f : Function) (D Y : BlockId) : Prop :=
  ¬ RA f D Y

/-- If `D` dominates `Y`, `Y` is not `D`, and `X` branches to `Y`, then `D` dominates `X`.
A path to `X` that avoided `D` would extend to a path to `Y` that avoids it too. -/
theorem dom_pred {f : Function} {D X Y : BlockId} (hY : Dom f D Y) (hXY : Edge f X Y)
    (hYD : Y ≠ D) : Dom f D X :=
  fun hRA => hY (RA.step hRA hXY hYD)

end IR
end TptWasm
