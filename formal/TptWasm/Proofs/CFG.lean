import TptWasm.CFG

/-!
# CFG simplification: unreachable-block removal is sound

The first of the four steps `tpt-wasm-opt::graph` performs. Removing every block no
run from the entry can reach does not change what any run does: the same result,
the same trace, and the same trap, for every fuel and every starting state.

## What is proved

* `prune_sound`: for any set of kept blocks that contains the entry and is closed
  under the edges between kept blocks, pruning to that set leaves every run
  unchanged.
* `prune_reachable_sound`: the instance with the set of blocks `Reachable` from the
  entry, which is what the Rust pass removes the complement of.

## What is not proved here

Empty-block threading is proved in `TptWasm.Proofs.Threading`. Block merging and
constant-condition folding are not proved yet: merging substitutes block parameters
along edges, and folding rewrites a branch from a known condition. Both need their
own edge argument.

Nothing here is admitted or assumed: every statement is a theorem with a proof.
-/

namespace TptWasm
namespace IR

/-- Looking up a kept block finds the same block in the pruned list. -/
theorem findBlock_prune (blocks : List Block) (keep : BlockId → Bool) (id : BlockId)
    (h : keep id = true) :
    findBlock (blocks.filter fun b => keep b.id) id = findBlock blocks id := by
  induction blocks with
  | nil => rfl
  | cons b rest ih =>
    unfold findBlock at ih ⊢
    by_cases hk : keep b.id = true
    · have hfilter : (b :: rest).filter (fun c => keep c.id) =
          b :: rest.filter (fun c => keep c.id) := by
        simp [List.filter, hk]
      rw [hfilter, List.find?_cons, List.find?_cons, ih]
    · have hdrop : keep b.id = false := by simpa using hk
      have hne : (b.id == id) = false := by
        cases hm : (b.id == id)
        · rfl
        · exfalso
          have heq : b.id = id := by simpa using hm
          rw [heq] at hdrop
          rw [h] at hdrop
          exact Bool.noConfusion hdrop
      have hfilter : (b :: rest).filter (fun c => keep c.id) =
          rest.filter (fun c => keep c.id) := by
        simp [List.filter, hdrop]
      rw [hfilter, List.find?_cons, hne]
      simpa using ih

/-- A block that `findBlock` returns has the identifier it was asked for. -/
theorem findBlock_id {blocks : List Block} {id : BlockId} {b : Block}
    (h : findBlock blocks id = some b) : b.id = id := by
  unfold findBlock at h
  have hp := List.find?_some h
  simpa using hp

/-- A block that `findBlock` returns is one of the blocks searched. -/
theorem findBlock_mem {blocks : List Block} {id : BlockId} {b : Block}
    (h : findBlock blocks id = some b) : b ∈ blocks := by
  unfold findBlock at h
  exact List.mem_of_find?_eq_some h

/-- A run from a kept block is the same in the pruned function as in the original.

Each entered block is found by `findBlock`, and a kept block's target is kept by the
closure hypothesis, so every lookup a run makes lands on the same block in both
functions. The induction is on fuel. -/
theorem runBlocks_prune (blocks : List Block) (keep : BlockId → Bool)
    (hclosed : ∀ b ∈ blocks, keep b.id = true →
      ∀ p ∈ b.terminator.targets, keep p.1 = true) :
    ∀ (n : Nat) (b : Block) (s : State) (trace : List Event),
      b ∈ blocks → keep b.id = true →
      runBlocks (blocks.filter fun c => keep c.id) n b s trace =
        runBlocks blocks n b s trace := by
  intro n
  induction n with
  | zero =>
    intro b s trace _ _
    rfl
  | succ n ih =>
    intro b s trace hb hk
    -- Enter a target, given that the target is kept, as both runs would.
    have enter_eq : ∀ (target : BlockId) (args : List ValueId) (s' : State) (trace' : List Event),
        (target, args) ∈ b.terminator.targets →
        (match findBlock (blocks.filter fun c => keep c.id) target with
          | none => Result.trapped trace' .hostFailure
          | some tb => runBlocks (blocks.filter fun c => keep c.id) n tb
              (s'.bindAll tb.params (args.map s'.values)) trace') =
        (match findBlock blocks target with
          | none => Result.trapped trace' .hostFailure
          | some tb => runBlocks blocks n tb
              (s'.bindAll tb.params (args.map s'.values)) trace') := by
      intro target args s' trace' hmem
      have hkt : keep target = true := hclosed b hb hk (target, args) hmem
      rw [findBlock_prune blocks keep target hkt]
      cases hf : findBlock blocks target with
      | none => rfl
      | some tb =>
        have hbmem : tb ∈ blocks := findBlock_mem hf
        have hidt : tb.id = target := findBlock_id hf
        have hktb : keep tb.id = true := by rw [hidt]; exact hkt
        exact ih tb _ trace' hbmem hktb
    unfold runBlocks
    cases hbody : runBodyR b.instrs s trace with
    | inl pair => rfl
    | inr pair =>
      obtain ⟨s', trace'⟩ := pair
      dsimp only
      cases hterm : b.terminator with
      | ret values => rfl
      | trap t => rfl
      | unreachable => rfl
      | branch target args =>
        exact enter_eq target args s' trace' (by simp [hterm, Terminator.targets])
      | condBranch c thenArm elseArm =>
        obtain ⟨thenTarget, thenArgs⟩ := thenArm
        obtain ⟨elseTarget, elseArgs⟩ := elseArm
        have hthen : (thenTarget, thenArgs) ∈ b.terminator.targets := by
          simp [hterm, Terminator.targets]
        have helse : (elseTarget, elseArgs) ∈ b.terminator.targets := by
          simp [hterm, Terminator.targets]
        by_cases hc : intPayload (s'.values c) = 0
        · simp only [hc, ↓reduceIte]
          exact enter_eq elseTarget elseArgs s' trace' helse
        · simp only [hc, ↓reduceIte]
          exact enter_eq thenTarget thenArgs s' trace' hthen

/-- Pruning to a closed set of blocks that contains the entry leaves every run of the
function unchanged, for every fuel and every starting state. -/
theorem prune_sound (f : Function) (keep : BlockId → Bool)
    (hentry : keep f.entry = true)
    (hclosed : ∀ b ∈ f.blocks, keep b.id = true →
      ∀ p ∈ b.terminator.targets, keep p.1 = true)
    (fuel : Nat) (s : State) :
    runFunction (prune f keep) fuel s = runFunction f fuel s := by
  unfold runFunction prune
  have hfind := findBlock_prune f.blocks keep f.entry hentry
  simp only
  rw [hfind]
  cases hb : findBlock f.blocks f.entry with
  | none => rfl
  | some b =>
    have hmem : b ∈ f.blocks := findBlock_mem hb
    have hidb : b.id = f.entry := findBlock_id hb
    have hkb : keep b.id = true := by rw [hidb]; exact hentry
    exact runBlocks_prune f.blocks keep hclosed fuel b s [] hmem hkb

/-- The set of blocks reachable from the entry is closed under the edges, so pruning
to it is sound. This is the statement that the blocks a run can reach are the only
ones unreachable-block removal may keep, and that keeping them is enough. -/
theorem reachable_closed (f : Function) :
    (∀ b ∈ f.blocks, Reachable f b.id →
      ∀ p ∈ b.terminator.targets, Reachable f p.1) := by
  intro b hb hreach p hp
  exact Reachable.edge hreach hb hp

open Classical in
/-- Removing every block that no run from the entry can reach does not change any
run of the function. -/
theorem prune_reachable_sound (f : Function) (fuel : Nat) (s : State) :
    runFunction (prune f (fun id => decide (Reachable f id))) fuel s =
      runFunction f fuel s := by
  apply prune_sound f (fun id => decide (Reachable f id))
  · exact decide_eq_true Reachable.entry
  · intro b hb hk p hp
    have hreach : Reachable f b.id := of_decide_eq_true hk
    exact decide_eq_true (reachable_closed f b hb hreach p hp)

end IR
end TptWasm
