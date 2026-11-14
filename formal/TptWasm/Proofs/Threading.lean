import TptWasm.Proofs.CFG

/-!
# CFG simplification: empty-block threading is sound

The second of the pass's four steps. An edge into a chain of empty blocks is
redirected past the chain to the block the chain ends at (`tpt-wasm-opt::graph`,
`thread_empty_blocks`).

## What is proved

`thread_behaviour`: when every block named `from_` branches to the head `t` of a chain
that ends at `last`, redirecting `from_` past the chain changes no function's behaviour.
A non-exhausted result, with its trace and its trap, is reached by the original
function if and only if the threaded function reaches it.

The statement is about behaviour, not fuel. Threading removes steps, so the threaded
function may finish inside a budget the original cannot. What it must not do is finish
with a different answer, or fail to finish when the original does.

## Hypotheses, and why they are the pass's conditions

* `hfrom`: every block named `from_` branches to `t` with no arguments. The pass only
  threads an edge that carries no values into a parameterless chain head. The
  verifier's arity rule forces that: a chain link has no parameters, so the edge into
  it carries none.
* `hchain`: the chain from `t` to `last` is made of empty, parameterless blocks that
  branch on with no arguments. This is the condition `chain_end` checks.

Nothing is admitted. The arity rule is not an axiom here: it is why `hfrom` holds on
verified IR, and the Rust pass checks it.
-/

namespace TptWasm
namespace IR

/-! ## Basic facts about the run -/

/-- Binding no values changes no state, whichever parameters are named. -/
theorem State.bindAll_nil_vals (s : State) (ids : List ValueId) : s.bindAll ids [] = s := by
  cases ids <;> rfl

/-- What a block's terminator does once its body has run to `s` with trace `trace`.

It is the terminator half of `runBlocks`, pulled out so that the proofs can treat the
terminator as a value. -/
def termResult (blocks : List Block) (n : Nat) (term : Terminator) (s : State)
    (trace : List Event) : Result :=
  match term with
  | .ret values => .returned trace (values.map s.values)
  | .trap t => .trapped trace t
  | .unreachable => .trapped trace .unreachable
  | .branch target args => runAt blocks n target args s trace
  | .condBranch c (thenTarget, thenArgs) (elseTarget, elseArgs) =>
    if intPayload (s.values c) = 0 then runAt blocks n elseTarget elseArgs s trace
    else runAt blocks n thenTarget thenArgs s trace

/-- One step of fuel: the body runs, then the terminator decides the result. -/
theorem runBlocks_succ (blocks : List Block) (n : Nat) (b : Block) (s : State)
    (trace : List Event) :
    runBlocks blocks (n + 1) b s trace =
      match runBodyR b.instrs s trace with
      | .inl (trace', t) => .trapped trace' t
      | .inr (s', trace') => termResult blocks n b.terminator s' trace' := by
  rfl

theorem runBlocks_zero (blocks : List Block) (b : Block) (s : State) (trace : List Event) :
    runBlocks blocks 0 b s trace = .exhausted trace := rfl

/-- A lookup that finds a block runs that block, entered with the edge's values. -/
theorem runAt_some {blocks : List Block} {n : Nat} {id : BlockId} {args : List ValueId}
    {s : State} {trace : List Event} {c : Block} (h : findBlock blocks id = some c) :
    runAt blocks n id args s trace =
      runBlocks blocks n c (s.bindAll c.params (args.map s.values)) trace := by
  unfold runAt
  rw [h]

theorem runAt_none {blocks : List Block} {n : Nat} {id : BlockId} {args : List ValueId}
    {s : State} {trace : List Event} (h : findBlock blocks id = none) :
    runAt blocks n id args s trace = .trapped trace .hostFailure := by
  unfold runAt
  rw [h]

/-- A run from a function's entry is the run that enters the entry with no arguments. -/
theorem runFunction_eq_runAt (f : Function) (n : Nat) (s : State) :
    runFunction f n s = runAt f.blocks n f.entry [] s [] := by
  unfold runFunction
  cases hf : findBlock f.blocks f.entry with
  | none => rw [runAt_none hf]
  | some b => rw [runAt_some hf, List.map_nil, State.bindAll_nil_vals]

/-! ## Fuel is monotone

A run that finishes with fuel `n` finishes with the same result with any more fuel. -/

theorem termResult_mono (blocks : List Block) (n : Nat) (term : Terminator) (s : State)
    (trace : List Event) (r : Result)
    (H : ∀ (id : BlockId) (args : List ValueId) (s : State) (tr : List Event) (r' : Result),
      NotExhausted r' → runAt blocks n id args s tr = r' → ∀ d, runAt blocks (n + d) id args s tr = r')
    (hne : NotExhausted r) (h : termResult blocks n term s trace = r) (d : Nat) :
    termResult blocks (n + d) term s trace = r := by
  cases term with
  | ret values => exact h
  | trap t => exact h
  | unreachable => exact h
  | branch target args => exact H target args s trace r hne h d
  | condBranch c thenArm elseArm =>
    obtain ⟨thenTarget, thenArgs⟩ := thenArm
    obtain ⟨elseTarget, elseArgs⟩ := elseArm
    unfold termResult at h ⊢
    by_cases hc : intPayload (s.values c) = 0
    · simp only [hc, ↓reduceIte] at h ⊢
      exact H elseTarget elseArgs s trace r hne h d
    · simp only [hc, ↓reduceIte] at h ⊢
      exact H thenTarget thenArgs s trace r hne h d

theorem runBlocks_mono (blocks : List Block) :
    ∀ (n : Nat) (b : Block) (s : State) (trace : List Event) (r : Result),
      NotExhausted r → runBlocks blocks n b s trace = r →
        ∀ d, runBlocks blocks (n + d) b s trace = r := by
  intro n
  induction n with
  | zero =>
    intro b s trace r hne h d
    rw [runBlocks_zero] at h
    subst h
    simp [NotExhausted] at hne
  | succ n ih =>
    intro b s trace r hne h d
    have hnd : n + 1 + d = (n + d) + 1 := by omega
    rw [hnd, runBlocks_succ]
    rw [runBlocks_succ] at h
    generalize hbody : runBodyR b.instrs s trace = res at h ⊢
    rcases res with ⟨tr0, tt⟩ | ⟨s', tr'⟩
    · exact h
    · have arm : ∀ (id : BlockId) (args : List ValueId) (s : State) (tr : List Event) (r' : Result),
          NotExhausted r' → runAt blocks n id args s tr = r' →
            ∀ d, runAt blocks (n + d) id args s tr = r' := by
        intro id args s tr r' hne' h' d'
        rcases Option.eq_none_or_eq_some (findBlock blocks id) with hf | ⟨tb, hf⟩
        · rw [runAt_none hf] at h' ⊢
          exact h'
        · rw [runAt_some hf] at h' ⊢
          exact ih tb _ tr r' hne' h' d'
      exact termResult_mono blocks n b.terminator s' tr' r arm hne h d

/-- Fuel monotonicity, for the runs the forward step uses. -/
theorem runAt_mono_of' (blocks : List Block) (n : Nat) (id : BlockId) (args : List ValueId)
    (s : State) (trace : List Event) (r : Result) (hne : NotExhausted r)
    (h : runAt blocks n id args s trace = r) (d : Nat) :
    runAt blocks (n + d) id args s trace = r := by
  rcases Option.eq_none_or_eq_some (findBlock blocks id) with hf | ⟨tb, hf⟩
  · rw [runAt_none hf] at h ⊢
    exact h
  · rw [runAt_some hf] at h ⊢
    exact runBlocks_mono blocks n tb _ trace r hne h d

/-! ## A chain costs exactly its length

Passing through `k` empty links takes `k` steps and changes nothing else. -/

theorem runAt_chain {blocks : List Block} {t last : BlockId} {k : Nat}
    (hc : Chain blocks t last k) :
    ∀ (j : Nat) (s : State) (trace : List Event),
      runAt blocks (j + k) t [] s trace = runAt blocks j last [] s trace := by
  induction hc with
  | done target =>
    intro j s trace
    simp
  | @link target next last' k' c hfind hinstrs hparams hterm hchain ih =>
    intro j s trace
    have hnd : j + (k' + 1) = (j + k') + 1 := by omega
    rw [hnd, runAt_some hfind, List.map_nil, State.bindAll_nil_vals,
      runBlocks_succ, hinstrs]
    simp only [runBodyR]
    rw [hterm]
    change runAt blocks (j + k') next [] s trace = runAt blocks j last' [] s trace
    exact ih j s trace

/-- A run that starts inside a chain with fewer steps than the chain's length runs out
of fuel there. -/
theorem runAt_chain_short {blocks : List Block} {t last : BlockId} {k : Nat}
    (hc : Chain blocks t last k) :
    ∀ (m : Nat), m < k → ∀ (s : State) (trace : List Event),
      ∃ trace2, runAt blocks m t [] s trace = .exhausted trace2 := by
  induction hc with
  | done target =>
    intro m hm
    exact absurd hm (Nat.not_lt_zero _)
  | @link target next last' k' c hfind hinstrs hparams hterm hchain ih =>
    intro m hm s trace
    cases m with
    | zero =>
      rw [runAt_some hfind]
      exact ⟨trace, rfl⟩
    | succ m' =>
      have hm' : m' < k' := by omega
      rw [runAt_some hfind, List.map_nil, State.bindAll_nil_vals,
        runBlocks_succ, hinstrs]
      simp only [runBodyR]
      rw [hterm]
      change ∃ trace2, runAt blocks m' next [] s trace = .exhausted trace2
      exact ih m' hm' s trace

/-! ## Block lookup and the threaded function -/

theorem retarget_id (from_ last : BlockId) (b : Block) :
    (retarget from_ last b).id = b.id := by
  unfold retarget
  split <;> rfl

theorem retarget_instrs (from_ last : BlockId) (b : Block) :
    (retarget from_ last b).instrs = b.instrs := by
  unfold retarget
  split <;> rfl

theorem retarget_params (from_ last : BlockId) (b : Block) :
    (retarget from_ last b).params = b.params := by
  unfold retarget
  split <;> rfl

theorem retarget_hit (from_ last : BlockId) (b : Block) (hid : b.id = from_) :
    (retarget from_ last b).terminator = .branch last [] := by
  simp [retarget, hid]

theorem retarget_miss (from_ last : BlockId) (b : Block) (hid : ¬ b.id = from_) :
    retarget from_ last b = b := by
  simp [retarget, hid]

theorem findBlock_threadEdge (blocks : List Block) (from_ last id : BlockId) :
    findBlock (blocks.map (retarget from_ last)) id =
      (findBlock blocks id).map (retarget from_ last) := by
  induction blocks with
  | nil => rfl
  | cons b rest ih =>
    unfold findBlock at ih ⊢
    simp only [List.map_cons, List.find?_cons, retarget_id]
    cases hb : (b.id == id) with
    | true => rfl
    | false => simpa using ih

/-- A lookup in the threaded function that finds the block gives the retargeted block,
with its parameters unchanged. -/
theorem runAt_map_some {blocks : List Block} {from_ last id : BlockId} {n : Nat}
    {args : List ValueId} {s : State} {trace : List Event} {bf : Block}
    (h : findBlock blocks id = some bf) :
    runAt (blocks.map (retarget from_ last)) n id args s trace =
      runBlocks (blocks.map (retarget from_ last)) n (retarget from_ last bf)
        (s.bindAll bf.params (args.map s.values)) trace := by
  rw [runAt_some (c := retarget from_ last bf) (by rw [findBlock_threadEdge, h]; rfl),
    retarget_params]

theorem runAt_map_none {blocks : List Block} {from_ last id : BlockId} {n : Nat}
    {args : List ValueId} {s : State} {trace : List Event} (h : findBlock blocks id = none) :
    runAt (blocks.map (retarget from_ last)) n id args s trace =
      .trapped trace .hostFailure :=
  runAt_none (by rw [findBlock_threadEdge, h]; rfl)

/-! ## Forward: the original's runs are the threaded function's runs -/

/-- The forward claim at fuel `j`: every block of the original, run at fuel `j`, gives
the same non-exhausted result when run in the threaded function. -/
def FwdP (f : Function) (from_ last : BlockId) (j : Nat) : Prop :=
  ∀ (b : Block), b ∈ f.blocks → ∀ (s : State) (trace : List Event) (r : Result),
    NotExhausted r → runBlocks f.blocks j b s trace = r →
      runBlocks (f.blocks.map (retarget from_ last)) j (retarget from_ last b) s trace = r

/-- The terminator of an unchanged block behaves the same in the threaded function,
given that its lookups do. -/
theorem termResult_fwd (blocks : List Block) (from_ last : BlockId) (m : Nat)
    (H : ∀ (id : BlockId) (args : List ValueId) (s : State) (tr : List Event) (r' : Result),
      NotExhausted r' → runAt blocks m id args s tr = r' →
        runAt (blocks.map (retarget from_ last)) m id args s tr = r')
    (term : Terminator) (s : State) (trace : List Event) (r : Result) (hne : NotExhausted r)
    (h : termResult blocks m term s trace = r) :
    termResult (blocks.map (retarget from_ last)) m term s trace = r := by
  cases term with
  | ret values => exact h
  | trap t => exact h
  | unreachable => exact h
  | branch target args => exact H target args s trace r hne h
  | condBranch c thenArm elseArm =>
    obtain ⟨thenTarget, thenArgs⟩ := thenArm
    obtain ⟨elseTarget, elseArgs⟩ := elseArm
    unfold termResult at h ⊢
    by_cases hc : intPayload (s.values c) = 0
    · simp only [hc, ↓reduceIte] at h ⊢
      exact H elseTarget elseArgs s trace r hne h
    · simp only [hc, ↓reduceIte] at h ⊢
      exact H thenTarget thenArgs s trace r hne h

/-- The forward step: `FwdP` at fuel `m + 1` follows from `FwdP` at every smaller fuel.

The block named `from_` is the only one whose terminator changes. Its run enters the
chain, which takes exactly `k` steps and changes nothing, and then enters `last`; the
threaded run enters `last` directly. -/
theorem forward_step (f : Function) (from_ t last : BlockId) (k : Nat)
    (hfrom : ∀ b ∈ f.blocks, b.id = from_ → b.terminator = .branch t [])
    (hchain : Chain f.blocks t last k) (m : Nat)
    (ih : ∀ j, j ≤ m → FwdP f from_ last j) :
    FwdP f from_ last (m + 1) := by
  -- A lookup at fuel `j ≤ m` in the original gives the same lookup in the threaded one.
  have runAt_fwd : ∀ (j : Nat), j ≤ m → ∀ (id : BlockId) (args : List ValueId) (s : State)
      (tr : List Event) (r' : Result), NotExhausted r' →
      runAt f.blocks j id args s tr = r' →
        runAt (f.blocks.map (retarget from_ last)) j id args s tr = r' := by
    intro j hj id args s tr r' hne' h'
    rcases Option.eq_none_or_eq_some (findBlock f.blocks id) with hf | ⟨bf, hf⟩
    · rw [runAt_none hf] at h'
      rw [runAt_map_none hf]
      exact h'
    · rw [runAt_some hf] at h'
      rw [runAt_map_some hf]
      exact ih j hj bf (findBlock_mem hf) (s.bindAll bf.params (args.map s.values)) tr r' hne' h'
  intro b hb s trace r hne h
  by_cases hid : b.id = from_
  · -- The block whose terminator was threaded.
    have hterm : b.terminator = .branch t [] := hfrom b hb hid
    rw [runBlocks_succ] at h
    rw [runBlocks_succ, retarget_instrs, retarget_hit from_ last b hid]
    rw [hterm] at h
    generalize hbody : runBodyR b.instrs s trace = res at h ⊢
    rcases res with ⟨tr0, tt⟩ | ⟨s', tr'⟩
    · exact h
    · change runAt f.blocks m t [] s' tr' = r at h
      change runAt (f.blocks.map (retarget from_ last)) m last [] s' tr' = r
      rcases Nat.lt_or_ge m k with hlt | hge
      · -- The run stops inside the chain, so it has no result.
        obtain ⟨trace2, h2⟩ := runAt_chain_short hchain m hlt s' tr'
        rw [h2] at h
        cases h
        simp [NotExhausted] at hne
      · -- The run reaches `last` with `m - k` fuel, which the threaded run has.
        obtain ⟨j, rfl⟩ : ∃ j, m = j + k := ⟨m - k, by omega⟩
        rw [runAt_chain hchain j s' tr'] at h
        have hfwd := runAt_fwd j (by omega) last [] s' tr' r hne h
        exact runAt_mono_of' (f.blocks.map (retarget from_ last)) j last [] s' tr' r hne hfwd k
  · -- Any other block keeps its terminator, so its targets are entered the same way.
    rw [retarget_miss from_ last b hid]
    rw [runBlocks_succ] at h ⊢
    generalize hbody : runBodyR b.instrs s trace = res at h ⊢
    rcases res with ⟨tr0, tt⟩ | ⟨s', tr'⟩
    · exact h
    · exact termResult_fwd f.blocks from_ last m
        (fun id args s'' tr'' r'' hne'' h'' => runAt_fwd m (Nat.le_refl m) id args s'' tr'' r'' hne'' h'')
        b.terminator s' tr' r hne h

theorem forward_all (f : Function) (from_ t last : BlockId) (k : Nat)
    (hfrom : ∀ b ∈ f.blocks, b.id = from_ → b.terminator = .branch t [])
    (hchain : Chain f.blocks t last k) :
    ∀ (m j : Nat), j ≤ m → FwdP f from_ last j := by
  intro m
  induction m with
  | zero =>
    intro j hj
    have hj0 : j = 0 := by omega
    subst hj0
    intro b _ s trace r hne h
    rw [runBlocks_zero] at h
    subst h
    simp [NotExhausted] at hne
  | succ m ih =>
    intro j hj
    rcases Nat.lt_or_ge j (m + 1) with hlt | hge
    · exact ih j (by omega)
    · have hj' : j = m + 1 := by omega
      subst hj'
      exact forward_step f from_ t last k hfrom hchain m ih

/-- The threaded function reaches the same lookups as the original, for every fuel. -/
theorem forward_runAt (f : Function) (from_ t last : BlockId) (k : Nat)
    (hfrom : ∀ b ∈ f.blocks, b.id = from_ → b.terminator = .branch t [])
    (hchain : Chain f.blocks t last k) (n : Nat) (id : BlockId) (args : List ValueId)
    (s : State) (trace : List Event) (r : Result) (hne : NotExhausted r)
    (h : runAt f.blocks n id args s trace = r) :
    runAt (f.blocks.map (retarget from_ last)) n id args s trace = r := by
  rcases Option.eq_none_or_eq_some (findBlock f.blocks id) with hf | ⟨bf, hf⟩
  · rw [runAt_none hf] at h
    rw [runAt_map_none hf]
    exact h
  · rw [runAt_some hf] at h
    rw [runAt_map_some hf]
    exact forward_all f from_ t last k hfrom hchain n n (Nat.le_refl n) bf (findBlock_mem hf) _ trace r hne h

/-! ## Backward: the threaded function's runs are the original's runs -/

/-- The backward claim at fuel `j`: a run of a block in the threaded function at fuel
`j` is matched by a run of the original block with some fuel. -/
def BackP (f : Function) (from_ last : BlockId) (j : Nat) : Prop :=
  ∀ (b : Block), b ∈ f.blocks → ∀ (s : State) (trace : List Event) (r : Result),
    NotExhausted r → runBlocks (f.blocks.map (retarget from_ last)) j (retarget from_ last b) s trace = r →
      ∃ m, runBlocks f.blocks m b s trace = r

/-- A lookup in the threaded function, at fuel `m`, is matched by a lookup in the original. -/
theorem termResult_bwd (blocks : List Block) (from_ last : BlockId) (m : Nat)
    (H : ∀ (id : BlockId) (args : List ValueId) (s : State) (tr : List Event) (r' : Result),
      NotExhausted r' → runAt (blocks.map (retarget from_ last)) m id args s tr = r' →
        ∃ m', runAt blocks m' id args s tr = r')
    (term : Terminator) (s : State) (trace : List Event) (r : Result) (hne : NotExhausted r)
    (h : termResult (blocks.map (retarget from_ last)) m term s trace = r) :
    ∃ m', termResult blocks m' term s trace = r := by
  cases term with
  | ret values => exact ⟨0, h⟩
  | trap t => exact ⟨0, h⟩
  | unreachable => exact ⟨0, h⟩
  | branch target args =>
    obtain ⟨m', hm'⟩ := H target args s trace r hne h
    exact ⟨m', hm'⟩
  | condBranch c thenArm elseArm =>
    obtain ⟨thenTarget, thenArgs⟩ := thenArm
    obtain ⟨elseTarget, elseArgs⟩ := elseArm
    unfold termResult at h
    by_cases hc : intPayload (s.values c) = 0
    · simp only [hc, ↓reduceIte] at h
      obtain ⟨m', hm'⟩ := H elseTarget elseArgs s trace r hne h
      refine ⟨m', ?_⟩
      unfold termResult
      simp only [hc, ↓reduceIte]
      exact hm'
    · simp only [hc, ↓reduceIte] at h
      obtain ⟨m', hm'⟩ := H thenTarget thenArgs s trace r hne h
      refine ⟨m', ?_⟩
      unfold termResult
      simp only [hc, ↓reduceIte]
      exact hm'

theorem backward_step (f : Function) (from_ t last : BlockId) (k : Nat)
    (hfrom : ∀ b ∈ f.blocks, b.id = from_ → b.terminator = .branch t [])
    (hchain : Chain f.blocks t last k) (m : Nat) (ih : BackP f from_ last m) :
    BackP f from_ last (m + 1) := by
  -- A threaded lookup at fuel `m` is matched by an original lookup.
  have runAt_bwd : ∀ (id : BlockId) (args : List ValueId) (s : State) (tr : List Event)
      (r' : Result), NotExhausted r' →
      runAt (f.blocks.map (retarget from_ last)) m id args s tr = r' →
        ∃ m', runAt f.blocks m' id args s tr = r' := by
    intro id args s tr r' hne' h'
    rcases Option.eq_none_or_eq_some (findBlock f.blocks id) with hf | ⟨bf, hf⟩
    · rw [runAt_map_none hf] at h'
      exact ⟨0, by rw [runAt_none hf]; exact h'⟩
    · rw [runAt_map_some hf] at h'
      obtain ⟨m', hm'⟩ :=
        ih bf (findBlock_mem hf) (s.bindAll bf.params (args.map s.values)) tr r' hne' h'
      exact ⟨m', by rw [runAt_some hf]; exact hm'⟩
  intro b hb s trace r hne h
  by_cases hid : b.id = from_
  · -- The threaded block branches straight to `last`.
    have hterm : b.terminator = .branch t [] := hfrom b hb hid
    rw [runBlocks_succ, retarget_instrs, retarget_hit from_ last b hid] at h
    generalize hbody : runBodyR b.instrs s trace = res at h
    rcases res with ⟨tr0, tt⟩ | ⟨s', tr'⟩
    · exact ⟨m + 1, by rw [runBlocks_succ, hbody]; exact h⟩
    · change runAt (f.blocks.map (retarget from_ last)) m last [] s' tr' = r at h
      obtain ⟨m', hm'⟩ := runAt_bwd last [] s' tr' r hne h
      refine ⟨(m' + k) + 1, ?_⟩
      rw [runBlocks_succ, hbody, hterm]
      change runAt f.blocks (m' + k) t [] s' tr' = r
      rw [runAt_chain hchain m' s' tr']
      exact hm'
  · -- Any other block is unchanged, and so is its terminator.
    rw [retarget_miss from_ last b hid] at h
    rw [runBlocks_succ] at h
    generalize hbody : runBodyR b.instrs s trace = res at h
    rcases res with ⟨tr0, tt⟩ | ⟨s', tr'⟩
    · exact ⟨m + 1, by rw [runBlocks_succ, hbody]; exact h⟩
    · obtain ⟨m', hm'⟩ := termResult_bwd f.blocks from_ last m
        (fun id args s'' tr'' r'' hne'' h'' => runAt_bwd id args s'' tr'' r'' hne'' h'')
        b.terminator s' tr' r hne h
      exact ⟨m' + 1, by rw [runBlocks_succ, hbody]; exact hm'⟩

theorem backward_all (f : Function) (from_ t last : BlockId) (k : Nat)
    (hfrom : ∀ b ∈ f.blocks, b.id = from_ → b.terminator = .branch t [])
    (hchain : Chain f.blocks t last k) :
    ∀ (m : Nat), BackP f from_ last m := by
  intro m
  induction m with
  | zero =>
    intro b _ s trace r hne h
    rw [runBlocks_zero] at h
    subst h
    simp [NotExhausted] at hne
  | succ m ih => exact backward_step f from_ t last k hfrom hchain m ih

theorem backward_runAt (f : Function) (from_ t last : BlockId) (k : Nat)
    (hfrom : ∀ b ∈ f.blocks, b.id = from_ → b.terminator = .branch t [])
    (hchain : Chain f.blocks t last k) (n : Nat) (id : BlockId) (args : List ValueId)
    (s : State) (trace : List Event) (r : Result) (hne : NotExhausted r)
    (h : runAt (f.blocks.map (retarget from_ last)) n id args s trace = r) :
    ∃ m, runAt f.blocks m id args s trace = r := by
  rcases Option.eq_none_or_eq_some (findBlock f.blocks id) with hf | ⟨bf, hf⟩
  · rw [runAt_map_none hf] at h
    exact ⟨0, by rw [runAt_none hf]; exact h⟩
  · rw [runAt_map_some hf] at h
    obtain ⟨m, hm⟩ :=
      backward_all f from_ t last k hfrom hchain n bf (findBlock_mem hf)
        (s.bindAll bf.params (args.map s.values)) trace r hne h
    exact ⟨m, by rw [runAt_some hf]; exact hm⟩

/-! ## The theorem -/

/-- Empty-block threading preserves behaviour: for every non-exhausted result, the
original function reaches it with some fuel exactly when the threaded function does. -/
theorem thread_behaviour (f : Function) (from_ t last : BlockId) (k : Nat)
    (hfrom : ∀ b ∈ f.blocks, b.id = from_ → b.terminator = .branch t [])
    (hchain : Chain f.blocks t last k) (s : State) (r : Result) (hne : NotExhausted r) :
    (∃ n, runFunction f n s = r) ↔ (∃ n, runFunction (threadEdge f from_ last) n s = r) := by
  have threaded : ∀ n, runFunction (threadEdge f from_ last) n s =
      runAt (f.blocks.map (retarget from_ last)) n f.entry [] s [] := by
    intro n
    rw [runFunction_eq_runAt]
    rfl
  constructor
  · rintro ⟨n, h⟩
    rw [runFunction_eq_runAt] at h
    refine ⟨n, ?_⟩
    rw [threaded n]
    exact forward_runAt f from_ t last k hfrom hchain n f.entry [] s [] r hne h
  · rintro ⟨n, h⟩
    rw [threaded n] at h
    obtain ⟨m, hm⟩ := backward_runAt f from_ t last k hfrom hchain n f.entry [] s [] r hne h
    exact ⟨m, by rw [runFunction_eq_runAt]; exact hm⟩

end IR
end TptWasm
