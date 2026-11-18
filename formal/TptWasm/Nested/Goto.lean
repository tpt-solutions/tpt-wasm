import TptWasm.Nested.Lower
import TptWasm.Proofs.V4Diamond

/-!
# The block graph reaches a continuation in a known number of steps

`GotoN G n b s tr K s' tr'` says: starting at block `b` in state `s` with trace `tr`, the block
graph `G` runs `n` blocks and enters block `K` in state `s'` with trace `tr'`. Each step runs a
block's body and follows its terminator, as `IR.runBlocks` does, so `runBlocks` with fuel `n + m`
from `b` equals `runBlocks` with fuel `m` from `K` (`GotoN_runBlocks`). Reaching is compositional
(`GotoN_trans`), which is what lets a statement list be run one statement after another.

Block lookups are by id, so the blocks must have distinct ids: `findBlock_of_mem` says a block
listed in `G`, with distinct ids, is the one lookup finds.
-/

namespace TptWasm
namespace Nested

open V4Diamond

/-- A block lookup finds the listed block, when the ids of the blocks are distinct. -/
theorem findBlock_of_mem : ∀ (G : List IR.Block) (blk : IR.Block), blk ∈ G →
    (G.map IR.Block.id).Nodup → IR.findBlock G blk.id = some blk
  | [], _, h, _ => by simp at h
  | x :: xs, blk, h, hnd => by
    by_cases hx : x = blk
    · subst hx
      simp [IR.findBlock]
    · have hmem : blk ∈ xs := (List.mem_cons.mp h).resolve_left (fun e => hx e.symm)
      have hne : x.id ≠ blk.id := by
        intro heq
        have hin : blk.id ∈ xs.map IR.Block.id := List.mem_map.mpr ⟨blk, hmem, rfl⟩
        rw [List.map_cons, List.nodup_cons] at hnd
        exact hnd.1 (heq ▸ hin)
      have hnd' : (xs.map IR.Block.id).Nodup := by
        rw [List.map_cons, List.nodup_cons] at hnd
        exact hnd.2
      have ih := findBlock_of_mem xs blk hmem hnd'
      unfold IR.findBlock at ih ⊢
      rw [List.find?_cons]
      have hbeq : (x.id == blk.id) = false := by
        simp only [beq_eq_false_iff_ne]
        exact hne
      simp only [hbeq]
      exact ih

/-- The block graph reaches block `K` from block `b` in `n` steps, leaving state `s'` and trace
`tr'`. -/
inductive GotoN (G : List IR.Block) :
    Nat → IR.BlockId → IR.State → List Event → IR.BlockId → IR.State → List Event → Prop
  | here (K : IR.BlockId) (s : IR.State) (tr : List Event) : GotoN G 0 K s tr K s tr
  | branch {n : Nat} {b tgt K : IR.BlockId} {s s1 s' : IR.State} {tr tr1 tr' : List Event}
      {blk tb : IR.Block} {args : List IR.ValueId} :
      IR.findBlock G b = some blk → blk.terminator = .branch tgt args →
      IR.runBodyR blk.instrs s tr = .inr (s1, tr1) → IR.findBlock G tgt = some tb →
      GotoN G n tgt (s1.bindAll tb.params (args.map s1.values)) tr1 K s' tr' →
      GotoN G (n + 1) b s tr K s' tr'
  | condThen {n : Nat} {b tt et K c : IR.BlockId} {s s1 s' : IR.State} {tr tr1 tr' : List Event}
      {blk tb : IR.Block} {ta ea : List IR.ValueId} :
      IR.findBlock G b = some blk → blk.terminator = .condBranch c (tt, ta) (et, ea) →
      IR.runBodyR blk.instrs s tr = .inr (s1, tr1) → IR.intPayload (s1.values c) ≠ 0 →
      IR.findBlock G tt = some tb →
      GotoN G n tt (s1.bindAll tb.params (ta.map s1.values)) tr1 K s' tr' →
      GotoN G (n + 1) b s tr K s' tr'
  | condElse {n : Nat} {b tt et K c : IR.BlockId} {s s1 s' : IR.State} {tr tr1 tr' : List Event}
      {blk tb : IR.Block} {ta ea : List IR.ValueId} :
      IR.findBlock G b = some blk → blk.terminator = .condBranch c (tt, ta) (et, ea) →
      IR.runBodyR blk.instrs s tr = .inr (s1, tr1) → IR.intPayload (s1.values c) = 0 →
      IR.findBlock G et = some tb →
      GotoN G n et (s1.bindAll tb.params (ea.map s1.values)) tr1 K s' tr' →
      GotoN G (n + 1) b s tr K s' tr'

/-- Reaching `K` in `n` steps is running `n` more blocks than the continuation's own run. -/
theorem GotoN_runBlocks {G : List IR.Block} {n : Nat} {b K : IR.BlockId} {s s' : IR.State}
    {tr tr' : List Event} (h : GotoN G n b s tr K s' tr') :
    ∀ (blk kblk : IR.Block) (m : Nat), IR.findBlock G b = some blk → IR.findBlock G K = some kblk →
      IR.runBlocks G (n + m) blk s tr = IR.runBlocks G m kblk s' tr' := by
  induction h with
  | here K s tr =>
    intro blk kblk m hb hk
    have : blk = kblk := Option.some.inj (hb.symm.trans hk)
    subst this
    simp
  | @branch n b tgt K s s1 s' tr tr1 tr' blk tb args hb hT hB hf _ ih =>
    intro blk' kblk m hb' hk
    have hblk : blk' = blk := Option.some.inj (hb'.symm.trans hb)
    subst hblk
    rw [show n + 1 + m = (n + m) + 1 by omega]
    rw [V4Diamond.runBlocks_branch G (n + m) blk' tb s s1 tr tr1 tgt args hB hT hf]
    exact ih tb kblk m hf hk
  | @condThen n b tt et K c s s1 s' tr tr1 tr' blk tb ta ea hb hT hB hv hf _ ih =>
    intro blk' kblk m hb' hk
    have hblk : blk' = blk := Option.some.inj (hb'.symm.trans hb)
    subst hblk
    rw [show n + 1 + m = (n + m) + 1 by omega]
    rw [V4Diamond.runBlocks_cond_then G (n + m) blk' tb s s1 tr tr1 c tt et ta ea hB hT hv hf]
    exact ih tb kblk m hf hk
  | @condElse n b tt et K c s s1 s' tr tr1 tr' blk tb ta ea hb hT hB hv hf _ ih =>
    intro blk' kblk m hb' hk
    have hblk : blk' = blk := Option.some.inj (hb'.symm.trans hb)
    subst hblk
    rw [show n + 1 + m = (n + m) + 1 by omega]
    rw [V4Diamond.runBlocks_cond_else G (n + m) blk' tb s s1 tr tr1 c tt et ta ea hB hT hv hf]
    exact ih tb kblk m hf hk

/-- Reaching is compositional: a run to `b` followed by a run from `b` to `c` is a run to `c`. -/
theorem GotoN_trans {G : List IR.Block} {n1 n2 : Nat} {a b c : IR.BlockId}
    {s s1 s2 : IR.State} {tr tr1 tr2 : List Event}
    (h1 : GotoN G n1 a s tr b s1 tr1) (h2 : GotoN G n2 b s1 tr1 c s2 tr2) :
    GotoN G (n1 + n2) a s tr c s2 tr2 := by
  induction h1 with
  | here K s tr =>
    simpa using h2
  | @branch n b tgt K s s1 s' tr tr1 tr' blk tb args hb hT hB hf _ ih =>
    have := GotoN.branch (n := n + n2) hb hT hB hf (ih h2)
    rw [show n + 1 + n2 = n + n2 + 1 by omega]
    exact this
  | @condThen n b tt et K c s s1 s' tr tr1 tr' blk tb ta ea hb hT hB hv hf _ ih =>
    have := GotoN.condThen (n := n + n2) hb hT hB hv hf (ih h2)
    rw [show n + 1 + n2 = n + n2 + 1 by omega]
    exact this
  | @condElse n b tt et K c s s1 s' tr tr1 tr' blk tb ta ea hb hT hB hv hf _ ih =>
    have := GotoN.condElse (n := n + n2) hb hT hB hv hf (ih h2)
    rw [show n + 1 + n2 = n + n2 + 1 by omega]
    exact this

end Nested
end TptWasm
