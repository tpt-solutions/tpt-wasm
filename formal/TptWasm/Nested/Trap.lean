import TptWasm.Nested.Simulate
import TptWasm.Proofs.V4Trap

/-!
# Traps in nested statements: the relations and the block graph

A trap in a nested program is an ordered trace of the events before it, and the trap itself. The
Wasm side is the relations `TrapS` (a statement traps) and `TrapSeq` (a statement list traps). A
trap happens in one of the straight-line instructions of the program, after the events of the runs
before it, so each relation is a run that succeeds up to a point and then traps there.

The IR side is `GotoT`: the block graph, from a block, runs some blocks and then a block traps,
with the trace before the trap. `GotoT_runBlocks` says the block runner then returns `trapped` with
that trace and trap.
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond V4Diamond

/-- The straight-line code `ops` traps from `c`: a prefix runs to `ck` with the events `evs`, and
the next instruction traps with `t`. -/
def OpsTrap (ops : List Instr) (c : Configuration) (evs : List Event) (t : Trap) : Prop :=
  ∃ (pre : List Instr) (i : Instr) (post : List Instr) (ck : Configuration),
    ops = pre ++ i :: post ∧ runWasm pre c = some (ck, evs) ∧ execOp i ck = some (.trap t)

mutual

/-- A statement traps from `c` with the events `evs` and the trap `t`, with at most `k` loop
iterations before the trap. -/
inductive TrapS : Nat → Stmt → Configuration → List Event → Trap → Prop
  | straight (k : Nat) (ops : List Instr) (c : Configuration) (evs : List Event) (t : Trap) :
      OpsTrap ops c evs t → TrapS k (.straight ops) c evs t
  | condTrap (k : Nat) (cond : List Instr) (th el : List Stmt) (c : Configuration)
      (evs : List Event) (t : Trap) :
      c.operandStack = [] → OpsTrap cond c evs t → TrapS k (.ifElse cond th el) c evs t
  | armThen (k : Nat) (cond : List Instr) (th el : List Stmt) (c c1 : Configuration)
      (e1 evs : List Event) (v : Int) (t : Trap) :
      c.operandStack = [] → runWasm cond c = some (c1, e1) → c1.operandStack = [.i32 v] → v ≠ 0 →
      TrapSeq k th { c1 with operandStack := [] } evs t → TrapS k (.ifElse cond th el) c (e1 ++ evs) t
  | armElse (k : Nat) (cond : List Instr) (th el : List Stmt) (c c1 : Configuration)
      (e1 evs : List Event) (t : Trap) :
      c.operandStack = [] → runWasm cond c = some (c1, e1) → c1.operandStack = [.i32 0] →
      TrapSeq k el { c1 with operandStack := [] } evs t → TrapS k (.ifElse cond th el) c (e1 ++ evs) t
  | loopCond (k : Nat) (cond : List Instr) (body : List Stmt) (c : Configuration)
      (evs : List Event) (t : Trap) :
      c.operandStack = [] → OpsTrap cond c evs t → TrapS (k + 1) (.whileNZ cond body) c evs t
  | loopBody (k : Nat) (cond : List Instr) (body : List Stmt) (c c1 : Configuration)
      (e1 evs : List Event) (v : Int) (t : Trap) :
      c.operandStack = [] → runWasm cond c = some (c1, e1) → c1.operandStack = [.i32 v] → v ≠ 0 →
      TrapSeq k body { c1 with operandStack := [] } evs t →
      TrapS (k + 1) (.whileNZ cond body) c (e1 ++ evs) t
  | loopNext (k : Nat) (cond : List Instr) (body : List Stmt) (c c1 c3 : Configuration)
      (e1 e2 evs : List Event) (v : Int) (t : Trap) :
      c.operandStack = [] → runWasm cond c = some (c1, e1) → c1.operandStack = [.i32 v] → v ≠ 0 →
      runSeq k body { c1 with operandStack := [] } = some (c3, e2) →
      TrapS k (.whileNZ cond body) c3 evs t →
      TrapS (k + 1) (.whileNZ cond body) c (e1 ++ e2 ++ evs) t

/-- A statement list traps from `c`: its first statement traps, or a statement runs and the rest
traps. -/
inductive TrapSeq : Nat → List Stmt → Configuration → List Event → Trap → Prop
  | head (k : Nat) (s : Stmt) (rest : List Stmt) (c : Configuration) (evs : List Event) (t : Trap) :
      TrapS k s c evs t → TrapSeq k (s :: rest) c evs t
  | tail (k : Nat) (s : Stmt) (rest : List Stmt) (c c1 : Configuration) (e1 evs : List Event)
      (t : Trap) :
      runStmt k s c = some (c1, e1) → TrapSeq k rest c1 evs t →
      TrapSeq k (s :: rest) c (e1 ++ evs) t

end

/-- The block graph, from block `b`, runs some blocks and then a block traps. -/
inductive GotoT (G : List IR.Block) :
    IR.BlockId → IR.State → List Event → List Event → Trap → Prop
  | blk (b : IR.BlockId) (s : IR.State) (tr tr' : List Event) (t : Trap) (blk : IR.Block) :
      IR.findBlock G b = some blk → IR.runBodyR blk.instrs s tr = .inl (tr', t) →
      GotoT G b s tr tr' t
  | branch {b tgt : IR.BlockId} {s s1 : IR.State} {tr tr1 tr' : List Event} {t : Trap}
      {blk tb : IR.Block} {args : List IR.ValueId} :
      IR.findBlock G b = some blk → blk.terminator = .branch tgt args →
      IR.runBodyR blk.instrs s tr = .inr (s1, tr1) → IR.findBlock G tgt = some tb →
      GotoT G tgt (s1.bindAll tb.params (args.map s1.values)) tr1 tr' t → GotoT G b s tr tr' t
  | condThen {b tt et c : IR.BlockId} {s s1 : IR.State} {tr tr1 tr' : List Event} {t : Trap}
      {blk tb : IR.Block} {ta ea : List IR.ValueId} :
      IR.findBlock G b = some blk → blk.terminator = .condBranch c (tt, ta) (et, ea) →
      IR.runBodyR blk.instrs s tr = .inr (s1, tr1) → IR.intPayload (s1.values c) ≠ 0 →
      IR.findBlock G tt = some tb →
      GotoT G tt (s1.bindAll tb.params (ta.map s1.values)) tr1 tr' t → GotoT G b s tr tr' t
  | condElse {b tt et c : IR.BlockId} {s s1 : IR.State} {tr tr1 tr' : List Event} {t : Trap}
      {blk tb : IR.Block} {ta ea : List IR.ValueId} :
      IR.findBlock G b = some blk → blk.terminator = .condBranch c (tt, ta) (et, ea) →
      IR.runBodyR blk.instrs s tr = .inr (s1, tr1) → IR.intPayload (s1.values c) = 0 →
      IR.findBlock G et = some tb →
      GotoT G et (s1.bindAll tb.params (ea.map s1.values)) tr1 tr' t → GotoT G b s tr tr' t

/-- Reaching a trapping block is compositional: a run to `b`, then a trap from `b`, is a trap. -/
theorem GotoN_transT {G : List IR.Block} {n : Nat} {a b : IR.BlockId}
    {s s1 : IR.State} {tr tr1 tr' : List Event} {t : Trap}
    (h1 : GotoN G n a s tr b s1 tr1) (h2 : GotoT G b s1 tr1 tr' t) : GotoT G a s tr tr' t := by
  induction h1 with
  | here K s tr => exact h2
  | @branch n b tgt K s s1 s' tr tr1 tr' blk tb args hb hT hB hf _ ih =>
    exact GotoT.branch hb hT hB hf (ih h2)
  | @condThen n b tt et K c s s1 s' tr tr1 tr' blk tb ta ea hb hT hB hv hf _ ih =>
    exact GotoT.condThen hb hT hB hv hf (ih h2)
  | @condElse n b tt et K c s s1 s' tr tr1 tr' blk tb ta ea hb hT hB hv hf _ ih =>
    exact GotoT.condElse hb hT hB hv hf (ih h2)

/-- A trap in the block graph is a trap of the block runner, with some fuel. -/
theorem GotoT_runBlocks {G : List IR.Block} {b : IR.BlockId} {s : IR.State}
    {tr tr' : List Event} {t : Trap} (h : GotoT G b s tr tr' t) :
    ∀ (blk : IR.Block), IR.findBlock G b = some blk →
      ∃ n, IR.runBlocks G n blk s tr = .trapped tr' t := by
  induction h with
  | blk b s tr tr' t blk0 hb hr =>
    intro blk hfb
    have : blk = blk0 := Option.some.inj (hfb.symm.trans hb)
    subst this
    exact ⟨1, by simp [IR.runBlocks, hr]⟩
  | @branch b tgt s s1 tr tr1 tr' t blk tb args hb hT hB hf _ ih =>
    intro blk0 hfb0
    have : blk0 = blk := Option.some.inj (hfb0.symm.trans hb)
    subst this
    obtain ⟨m, hm⟩ := ih tb hf
    exact ⟨m + 1, by simp [IR.runBlocks, hB, hT, hf, hm]⟩
  | @condThen b tt et c s s1 tr tr1 tr' t blk tb ta ea hb hT hB hv hf _ ih =>
    intro blk0 hfb0
    have : blk0 = blk := Option.some.inj (hfb0.symm.trans hb)
    subst this
    obtain ⟨m, hm⟩ := ih tb hf
    exact ⟨m + 1, by simp [IR.runBlocks, hB, hT, hv, hf, hm]⟩
  | @condElse b tt et c s s1 tr tr1 tr' t blk tb ta ea hb hT hB hv hf _ ih =>
    intro blk0 hfb0
    have : blk0 = blk := Option.some.inj (hfb0.symm.trans hb)
    subst this
    obtain ⟨m, hm⟩ := ih tb hf
    exact ⟨m + 1, by simp [IR.runBlocks, hB, hT, hv, hf, hm]⟩

end Nested
end TptWasm
