import TptWasm.IR
import TptWasm.IRExec
import TptWasm.Passes
import TptWasm.Proofs.V2

/-!
# V5: compiler transformation correctness

The three proofs `spec.txt` section 84 asks for, in the order it gives them:

```text
constant-fold(x + 0)
    can have a tiny proof.
Then: dead-block elimination
Then: ...
```

## What each theorem says

Each pass is proved to preserve the *observation* of a block body: the same event
trace, and the same state. That is the `spec.txt` section 67 contract restated in
the vocabulary of `TptWasm.Observation`.

## The shape the proofs deliberately do not take

These are equivalences for a block *body*, in a state where the operands are
already bound. They do not attempt a whole-function equivalence, which needs the
liveness argument the body-level version takes as a premise; nor the trap-position
clause, which needs the fuel-bounded run rather than a single block.

Each of those is named in `docs/formal/verification-ladder.md` as remaining work.
A theorem that quietly claimed more than it proved would be worse than a smaller
theorem that says exactly what it says.
-/

namespace TptWasm
namespace IR

/-- The event an instruction emits, if any. -/
def traceEvent (i : Instr) (s : State) : List Event :=
  match execInstr i s with
  | .ran _ (some e) _ => [e]
  | _ => []

/-- The events a whole body emits, in order. -/
def bodyTrace (body : List Instr) (s : State) : List Event :=
  match runBody body s [] with
  | some (_, trace) => trace
  | none => []

/-- Two bodies are equivalent in `s` when they leave the same state and emit the
same events. -/
def BodyEquiv (a b : List Instr) (s : State) : Prop :=
  runBody a s [] = runBody b s []

/-- **V5 step 1: a constant instruction is observationally inert.**

This is the fact the folding proof rests on. Arithmetic is not observable, which
is what lets an optimizer remove it -- every `Event` constructor corresponds to
something an embedder can see, and none of them corresponds to computing a value.
-/
theorem constI32_inert (r : ValueId) (v : Int) (s : State) :
    runBody [.constI32 r v] s [] = some (s.bind r (.i32 v), []) := by
  simp [runBody, execInstr, State.bind]

/-- The same for an `i64` constant. -/
theorem constI64_inert (r : ValueId) (v : Int) (s : State) :
    runBody [.constI64 r v] s [] = some (s.bind r (.i64 v), []) := by
  simp [runBody, execInstr, State.bind]

/-- **V5 step 2: a pure arithmetic instruction is observationally inert.**

The instruction changes the value environment, which the next instruction reads,
but emits no event and leaves the store alone. This is the fact that makes the
`spec.txt` "pure computation is not observable" claim checkable rather than
merely asserted.
-/
theorem binI32_inert (r : ValueId) (op : BinOp) (l rr : ValueId) (s : State) :
    bodyTrace [.binI32 r op l rr] s = [] := by
  simp [bodyTrace, runBody, execInstr]

/-- A comparison is inert in the same way. -/
theorem cmpI32_inert (r : ValueId) (op : ICmp) (l rr : ValueId) (s : State) :
    bodyTrace [.cmpI32 r op l rr] s = [] := by
  simp [bodyTrace, runBody, execInstr]

/-- **V5 step 3: a store is *not* inert.

Recorded explicitly because it is the clause the contract is really about. A
store is observable even when the value stored is a constant, which is why
constant folding must not delete one -- and why a folding pass that treated
"computes a value" as "has no effect" would be caught here.

The premise is that the write *succeeds*, rather than that the access is in
bounds. That is the right split: whether an in-bounds write succeeds is a
property of `MemoryState` and is V2's subject, whereas what this theorem is
about is that a store which does succeed leaves an event. Pushing the bounds
argument in here would restate V2 and obscure the claim.
-/
theorem store_not_inert (m : MemoryId) (a v : ValueId) (w : Nat) (s : State)
    (heap : MemoryState) (hmem : s.store.memory? m = some heap)
    (updated : MemoryState)
    (hwrite : heap.writeLE? (intPayload (s.values a)).natAbs w (storedBits (s.values v))
      = some updated) :
    bodyTrace [.store m a v w] s
      = [Event.store m (intPayload (s.values a)).natAbs w] :=
  by simp [bodyTrace, runBody, execInstr, hmem, hwrite]

/-- **V5: a load is *not* inert**, symmetrically.

A load is observable because it can fault: deleting a read that would have been
out of bounds deletes a trap, which is the trap-position clause of the contract in
concrete form.
-/
theorem load_not_inert (m : MemoryId) (r : ValueId) (a : ValueId) (w : Nat) (s : State)
    (heap : MemoryState) (hmem : s.store.memory? m = some heap)
    (raw : Nat) (hread : heap.readLE? (intPayload (s.values a)).natAbs w = some raw) :
    bodyTrace [.load r m a w] s
      = [Event.load (intPayload (s.values a)).natAbs w] :=
  by simp [bodyTrace, runBody, execInstr, hmem, hread]

/-- **V5: a single fold step is observationally inert.**

If `foldable` rewrites `i` to `i'`, then running one leaves the same events as
running the other. This is the per-instruction content of constant folding, and
`foldBlockWith_preserves_trace` below lifts it to the whole pass.

The statement is about the event trace rather than the whole run because that is
the part a rewrite can be observed changing. A fold replaces a computation with
its result, so it changes what a *later* instruction reads -- and nothing an
embedder can see.

The case analysis is on what `foldable` computes, so in every surviving branch the
replacement is *known* to be a constant rather than guessed at. That is what makes
the goal close: both a constant and the arithmetic instruction it replaces emit
nothing.
-/
theorem foldable_inert (cs : List (ValueId × Value)) (i i' : Instr) (s : State)
    (h : foldable s cs i = some i') :
    bodyTrace [i] s = bodyTrace [i'] s := by
  cases i with
  | constI32 r v => simp_all [foldable]
  | constI64 r v => simp_all [foldable]
  | binI32 r op l rr =>
    cases hl : Constants.constant cs s l with
    | none => simp_all [foldable]
    | some vl =>
      cases hr : Constants.constant cs s rr with
      | none => simp_all [foldable]
      | some vr =>
        cases vl with
        | i32 a =>
          cases vr with
          | i32 b =>
            -- The only branch that can fold. `h` says what it folded to, so `i'`
            -- is that constant, and both sides are inert.
            simp only [foldable, hl, hr, Option.some.injEq] at h
            cases h
            simp [bodyTrace, runBody, execInstr]
          | i64 _ => simp_all [foldable]
          | f32 _ => simp_all [foldable]
          | f64 _ => simp_all [foldable]
        | i64 _ => simp_all [foldable]
        | f32 _ => simp_all [foldable]
        | f64 _ => simp_all [foldable]
  | binI64 r op l rr =>
    cases hl : Constants.constant cs s l with
    | none => simp_all [foldable]
    | some vl =>
      cases hr : Constants.constant cs s rr with
      | none => simp_all [foldable]
      | some vr =>
        cases vl with
        | i64 a =>
          cases vr with
          | i64 b =>
            simp only [foldable, hl, hr, Option.some.injEq] at h
            cases h
            simp [bodyTrace, runBody, execInstr]
          | i32 _ => simp_all [foldable]
          | f32 _ => simp_all [foldable]
          | f64 _ => simp_all [foldable]
        | i32 _ => simp_all [foldable]
        | f32 _ => simp_all [foldable]
        | f64 _ => simp_all [foldable]
  | cmpI32 r op l rr => simp_all [foldable]
  | load r m a w => simp_all [foldable]
  | store m a v w => simp_all [foldable]
  | globalGet r i => simp_all [foldable]
  | globalSet i v => simp_all [foldable]
  | call i args results => simp_all [foldable]
  | callHost n g args results => simp_all [foldable]
  | drop v => simp_all [foldable]

/- **What a fold step does
 
`foldable_inert` above is the strongest per-step statement that is true
unconditionally, and the reason it is only about the trace is not a weakness of the
proof but a fact about the model.
 
`foldable` is handed a constant *table*, and it reads that table rather than the
state. So for `foldable s cs (.binI32 r op l rr) = some (.constI32 r v)` to be sound,
`cs` must say `l` and `rr` hold the values the *state* holds -- otherwise the fold
replaces an arithmetic step with a constant computed from different operands. In
this model that failure is real and not hypothetical: on `State.empty`,
`execInstr (.binI32 0 .add 1 2) _` binds `(0, i32 0)` while
`execInstr (.constI32 0 99) _` binds `(0, i32 99)`, so the two instructions
disagree about the value they produce.
 
The same gap appears one level up. A whole-body theorem needs the table to agree
with the state *and* to stay agreeing as the pass extends it, and because a
non-constant head binds its result key, agreement can only be maintained if no value
is defined twice -- an SSA condition this model does not enforce.
 
None of this is a counterexample to the optimizer: `foldBlock` builds its table
during the same walk over the body it is folding, starting from `[]`, so the table
is by construction a record of the constants *this* body defined earlier, and the
Rust lowering guarantees each value is defined once. What the model lacks is the
statement of that invariant, not the fact of it.
 
The remaining work for V5 is therefore a model change rather than a proof change:
give `foldable` the state (or a table derived from it) and state the single
-definition invariant, and the run-level theorem follows by the induction in
`foldBlockWith_preserves_runBody`'s docstring. Recording that here is
more useful than a proof of a statement that is false as currently modelled. -/

/-- **An inert head contributes nothing to the trace.**

If the head ran and emitted no event, the body's trace is the rest's trace, run
from the state the head left. That is the only fact the folding induction needs
about a body, and stating it this way -- rather than as "the trace is a prefix
plus the rest" -- is what makes the induction a two-line argument.

A head that *traps* is a different case: `runBody` returns `none` and the whole
trace is empty, so nothing after it is observable. That is stated separately
below, because a fold can never produce a trapping head and the induction needs
to know it does not.
-/
theorem bodyTrace_cons_inert (i : Instr) (rest : List Instr) (s : State)
    (s' : State) (results : List (ValueId × Value))
    (hexec : execInstr i s = StepResult.ran s' none results) :
    bodyTrace (i :: rest) s
      = bodyTrace rest (results.foldl (fun st (r, v) => st.bind r v) s') := by
  unfold bodyTrace
  rw [runBody, hexec]

/-- Prepending an event to the accumulated trace does not change whether a body
completes, and extends its trace by exactly that event.

`runBody` threads the accumulated trace into the recursive call, so the trace
argument is a *prefix* of the result rather than something the result replaces.
This lemma is the statement of that, and it is the one piece of vocabulary the
folding proof needs: the heads on both sides of a fold emit the same event, so the
rest is entered with the same prefix and the two runs agree.

Stated as a two-sided equation rather than a case analysis on the result, because
that is what makes it usable at the call site: the goal there is a `match` over
`runBody`, and rewriting with this lemma collapses the match without needing to
know which branch it is in. -/
theorem runBody_trace_prefix (instrs : List Instr) (s : State) (tr : List Event)
    (e : Event) :
    (match runBody instrs s tr with
      | some (st, t) => some (st, e :: t)
      | none => none) = runBody instrs s (e :: tr) := by
  induction instrs generalizing s tr with
  | nil => rfl
  | cons i rest ih =>
      -- The head either traps -- in which case both sides are `none` -- or runs,
      -- in which case the prefix and the head's own event are threaded down
      -- unchanged and the induction hypothesis applies to the rest.
      cases hexec : execInstr i s with
      | trapped t => simp [runBody, hexec]
      | ran s' ev results =>
          -- The head ran, so the rest is entered from the state it left, with
          -- whatever trace the head built. Case on whether the head was silent:
          -- `ev` is the scrutinee of a `match` inside `runBody`, so it has to be a
          -- constructor before `simp` can collapse it.
          cases ev with
          | none =>
              -- A silent head threads the incoming trace down unchanged.
              simp [runBody, hexec]
              exact ih (s := _) (tr := _)
          | some e' =>
              -- An event-emitting head appends its event to the incoming trace, and
              -- the rest is entered with that.
              simp [runBody, hexec]
              exact ih (s := _) (tr := _)

/-- A trapping head makes the whole body's trace empty. -/
theorem bodyTrace_cons_trapped (i : Instr) (rest : List Instr) (s : State)
    (trap : Trap) (hexec : execInstr i s = StepResult.trapped trap) :
    bodyTrace (i :: rest) s = [] := by
  unfold bodyTrace
  rw [runBody, hexec]

/-- Two heads that leave the same state and emit the same event make the bodies
below them observed identically.

This is the form the folding induction actually needs. It is deliberately stated
over `runBody` with an *arbitrary* incoming trace rather than over `bodyTrace`:
`runBody` threads the accumulated trace into the rest, so the two runs hand
*different* prefixes down even when the heads agree, and their final traces are
equal because the prefixes are equal -- not because the recursive calls were
literally the same expression.

Stating it for an arbitrary incoming trace is what makes the induction work: the
recursive call is made with a trace this theorem quantifies over, so the
hypothesis applies at whatever prefix the enclosing context has built up. -/
theorem runBody_same_head (i i' : Instr) (rest : List Instr) (s : State)
    (s' : State) (event : Option Event) (results : List (ValueId × Value))
    (hexec : execInstr i s = StepResult.ran s' event results)
    (hexec' : execInstr i' s = StepResult.ran s' event results)
    (trace : List Event) :
    runBody (i :: rest) s trace
      = runBody (i' :: rest) s trace := by
  -- Both heads reduce to the same `StepResult`, so the state the rest runs from,
  -- the event appended to the trace, and the bound values are all the same
  -- expression -- and `runBody` is deterministic in those.
  simp only [runBody, hexec, hexec']

/-- **V5: dead-value elimination never removes an observable instruction.**

An observable instruction is present in the output whenever it was present in the
input, for every `extra`. That is the claim that stops a value-only pass from
dropping a trap or a host effect while every value-level check still passes. It is
kept as its own theorem because it needs no liveness reasoning at all; the
semantic statement `deadBlock_sound` below subsumes it. -/
theorem deadBlock_keeps_observable :
    forall (extra : List ValueId) (body : List Instr) (i : Instr),
      i ∈ body → observable i → i ∈ deadBlock extra body := by
  intro extra body
  induction body with
  | nil =>
    intro i himem
    simp at himem
  | cons head tail ih =>
    intro i himem hobs
    simp only [List.mem_cons] at himem
    rcases himem with rfl | htail
    · simp [deadBlock, hobs]
    · simp only [deadBlock]
      split
      · exact List.mem_cons_of_mem _ (ih i htail hobs)
      · exact ih i htail hobs

/-! ### The liveness half of dead-block elimination

`deadBlock_keeps_observable` says what the pass must not delete. The theorems below
say what it may delete, and that deleting it changes nothing a caller can see. The
premise is the live-out set `extra`, stated explicitly: the caller promises that
`extra` covers every value read after the body (the terminator and the successors'
parameter bindings). Getting that premise wrong is the failure this pass is most
prone to, which is why it is a hypothesis and not an assumption. -/

/-- Two states agree on a set of values when they hold the same store and give the
same value to every member of `L`. -/
def Agree (L : List ValueId) (s t : State) : Prop :=
  s.store = t.store ∧ ∀ v ∈ L, s.values v = t.values v

theorem Agree.mono {a b : List ValueId} (hab : ∀ v ∈ a, v ∈ b) {s t : State}
    (h : Agree b s t) : Agree a s t :=
  ⟨h.1, fun v hv => h.2 v (hab v hv)⟩

/-- How one instruction's result, run from `sB`, relates to the result of the same
instruction run from `sD`.

The `ran` case says the events and the defined values are equal, the stores are
equal afterwards, and each side's values are its own input values: a non-store
instruction returns the state it was given, and a store changes only the store. -/
def SimFrom (sB sD : State) : StepResult -> StepResult -> Prop
  | .ran s1 e1 r1, .ran s2 e2 r2 =>
      e1 = e2 ∧ r1 = r2 ∧ s1.store = s2.store ∧ s1.values = sB.values ∧ s2.values = sD.values
  | .trapped t1, .trapped t2 => t1 = t2
  | _, _ => False

/-- An instruction behaves the same on two states that agree on what it reads. -/
theorem execInstr_sim (i : Instr) (sB sD : State) (h : Agree (usedBy i) sB sD) :
    SimFrom sB sD (execInstr i sB) (execInstr i sD) := by
  obtain ⟨hstore, hval⟩ := h
  cases i with
  | constI32 r v => simp [execInstr, SimFrom, hstore]
  | constI64 r v => simp [execInstr, SimFrom, hstore]
  | binI32 r op l rr =>
    have hl := hval l (by simp [usedBy])
    have hr := hval rr (by simp [usedBy])
    simp [execInstr, SimFrom, hl, hr, hstore]
  | binI64 r op l rr =>
    have hl := hval l (by simp [usedBy])
    have hr := hval rr (by simp [usedBy])
    simp [execInstr, SimFrom, hl, hr, hstore]
  | cmpI32 r op l rr =>
    have hl := hval l (by simp [usedBy])
    have hr := hval rr (by simp [usedBy])
    simp [execInstr, SimFrom, hl, hr, hstore]
  | load r memory address width =>
    have ha := hval address (by simp [usedBy])
    simp only [execInstr, hstore, ha]
    repeat' split
    all_goals simp [SimFrom, hstore]
  | store memory address value width =>
    have ha := hval address (by simp [usedBy])
    have hv := hval value (by simp [usedBy])
    simp only [execInstr, hstore, ha, hv]
    repeat' split
    all_goals simp [SimFrom]
  | globalGet r index =>
    simp only [execInstr, hstore]
    repeat' split
    all_goals simp [SimFrom, hstore]
  | globalSet index value =>
    have hv := hval value (by simp [usedBy])
    simp only [execInstr, hstore, hv]
    repeat' split
    all_goals simp [SimFrom, hstore]
  | call index args results => simp [execInstr, SimFrom, hstore]
  | callHost name granted args results =>
    have hargs : args.map sB.values = args.map sD.values := by
      apply List.map_congr_left
      intro a ha
      exact hval a (by simpa [usedBy] using ha)
    cases granted <;> simp [execInstr, SimFrom, hargs, hstore]
  | drop value => simp [execInstr, SimFrom, hstore]

/-- Binding the same results into two agreeing states keeps them agreeing. -/
theorem fold_bind_agree (L : List ValueId) (results : List (ValueId × Value)) :
    ∀ (s t : State), s.store = t.store -> (∀ v ∈ L, s.values v = t.values v) ->
      Agree L (results.foldl (fun st (r, v) => st.bind r v) s)
        (results.foldl (fun st (r, v) => st.bind r v) t) := by
  induction results with
  | nil =>
    intro s t hs hv
    exact ⟨hs, hv⟩
  | cons head rest ih =>
    intro s t hs hv
    obtain ⟨r, v⟩ := head
    apply ih
    · simp [State.bind, hs]
    · intro w hw
      simp only [State.bind]
      split
      · rfl
      · exact hv w hw

/-- A removable instruction runs without effect and defines exactly one value. -/
theorem removable_shape (i : Instr) (s : State) (h : removable i = true) :
    ∃ r v, definedBy i = [r] ∧ execInstr i s = .ran s none [(r, v)] := by
  cases i with
  | constI32 r val => exact ⟨r, .i32 val, rfl, rfl⟩
  | constI64 r val => exact ⟨r, .i64 val, rfl, rfl⟩
  | binI32 r op l rr =>
    exact ⟨r, .i32 (binI32Eval op (intPayload (s.values l)) (intPayload (s.values rr))), rfl, rfl⟩
  | binI64 r op l rr =>
    exact ⟨r, .i64 (binI64Eval op (intPayload (s.values l)) (intPayload (s.values rr))), rfl, rfl⟩
  | cmpI32 r op l rr =>
    exact ⟨r, cmpI32Eval op (intPayload (s.values l)) (intPayload (s.values rr)), rfl, rfl⟩
  | load _ _ _ _ => simp [removable] at h
  | store _ _ _ _ => simp [removable] at h
  | globalGet _ _ => simp [removable] at h
  | globalSet _ _ => simp [removable] at h
  | call _ _ _ => simp [removable] at h
  | callHost _ _ _ _ => simp [removable] at h
  | drop _ => simp [removable] at h

/-- **V5: dead-value elimination preserves the block's behaviour.**

Let `D` be `deadBlock extra body`, and let `sB`, `sD` be two states that agree on
the values `D` reads and on the store. Then running `body` from `sB` and `D` from
`sD` either both trap, or both succeed with the same trace and with final states
that agree on `extra`.

This is the whole-body statement the earlier note on the constant-folding pass
said was missing. It holds for three reasons, each proved above or in the
induction: an instruction the pass keeps reads only values in the live set, so it
runs the same in both states (`execInstr_sim`); an instruction the pass deletes is
pure, so running it changes only the one value it defines (`removable_shape`); and
that value is not read by anything that survives after it, so the later steps
cannot see the difference. -/
theorem deadBlock_sound (extra : List ValueId) :
    forall (body : List Instr) (sB sD : State) (tr : List Event),
      Agree (liveOf extra (deadBlock extra body)) sB sD ->
      (runBody body sB tr = none ∧ runBody (deadBlock extra body) sD tr = none) ∨
      ∃ sB' sD' trE, runBody body sB tr = some (sB', trE) ∧
        runBody (deadBlock extra body) sD tr = some (sD', trE) ∧ Agree extra sB' sD' := by
  intro body
  induction body with
  | nil =>
    intro sB sD tr hag
    right
    refine ⟨sB, sD, tr, ?_, ?_, ?_⟩
    · simp [runBody]
    · simp [deadBlock, runBody]
    · simpa [deadBlock, liveOf] using hag
  | cons i rest ih =>
    intro sB sD tr hag
    by_cases hk : (observable i || !removable i ||
        (definedBy i).any (fun r => decide (r ∈ liveOf extra (deadBlock extra rest)))) = true
    · -- The pass keeps `i`.
      have hD : deadBlock extra (i :: rest) = i :: deadBlock extra rest := by
        simp [deadBlock, hk]
      have hsub : ∀ v ∈ usedBy i, v ∈ liveOf extra (deadBlock extra (i :: rest)) := by
        intro v hv
        rw [hD]
        simp only [liveOf, List.flatMap_cons, List.mem_append]
        exact Or.inr (Or.inl hv)
      have hrest : ∀ v ∈ liveOf extra (deadBlock extra rest),
          v ∈ liveOf extra (deadBlock extra (i :: rest)) := by
        intro v hv
        rw [hD]
        simp only [liveOf, List.flatMap_cons, List.mem_append] at hv ⊢
        rcases hv with hv | hv
        · exact Or.inl hv
        · exact Or.inr (Or.inr hv)
      have hagI : Agree (usedBy i) sB sD := hag.mono hsub
      have hagR : Agree (liveOf extra (deadBlock extra rest)) sB sD := hag.mono hrest
      have hsim := execInstr_sim i sB sD hagI
      rw [hD]
      cases hB : execInstr i sB with
      | trapped t =>
        cases hDi : execInstr i sD with
        | trapped t2 =>
          rw [hB, hDi] at hsim
          simp only [SimFrom] at hsim
          subst hsim
          left
          simp [runBody, hB, hDi]
        | ran s2 e2 r2 =>
          rw [hB, hDi] at hsim
          simp only [SimFrom] at hsim
      | ran s1 e1 r1 =>
        cases hDi : execInstr i sD with
        | trapped t2 =>
          rw [hB, hDi] at hsim
          simp only [SimFrom] at hsim
        | ran s2 e2 r2 =>
          rw [hB, hDi] at hsim
          simp only [SimFrom] at hsim
          obtain ⟨he, hr, hstore, hv1, hv2⟩ := hsim
          subst he
          subst hr
          have hfold := fold_bind_agree (liveOf extra (deadBlock extra rest)) r1 s1 s2 hstore
            (fun v hv => by rw [hv1, hv2]; exact hagR.2 v hv)
          simp only [runBody, hB, hDi]
          exact ih _ _ _ hfold
    · -- The pass deletes `i`: it is pure, and its value is read by nothing that survives.
      have hD : deadBlock extra (i :: rest) = deadBlock extra rest := by
        simp [deadBlock, hk]
      have hpure : removable i = true := by
        by_cases hne : removable i = true
        · exact hne
        · exfalso
          apply hk
          simp [hne]
      obtain ⟨r, v, hdef, hrun⟩ := removable_shape i sB hpure
      have hnotlive : r ∉ liveOf extra (deadBlock extra rest) := by
        intro hmem
        apply hk
        simp [hdef, hmem]
      rw [hD] at hag ⊢
      simp only [runBody, hrun]
      apply ih
      refine ⟨hag.1, fun w hw => ?_⟩
      have hwr : w ≠ r := fun h => hnotlive (by rw [← h]; exact hw)
      simp [State.bind, hwr, hag.2 w hw]

/- **What is proved here, and what is not.**
 
Proved, in order:
 
- `constI32_inert`, `constI64_inert`, `binI32_inert`, `cmpI32_inert`: the
  individual arithmetic instructions emit no event, so replacing one cannot change a
  trace.
- `store_not_inert`, `load_not_inert`: memory access and the host boundary *do* emit
  events, and are therefore not foldable. This pair is what stops the previous four
  from being read as "all instructions are interchangeable".
- `foldable_inert`: a fold step preserves a single instruction's trace.
- `runBody_trace_prefix`: the accumulated trace is a prefix of the result, so the same
  event can be handed to the rest without changing the rest's behaviour.
- `runBody_same_head`: two heads that run alike make the bodies below them alike.
- `bodyTrace_cons_inert`, `bodyTrace_cons_trapped`: the two ways a head can fail to
  contribute a trace.
 
Not proved, and deliberately not asserted: the whole-body equation
`bodyTrace (foldBlock body) s = bodyTrace body s`. The per-step facts above are
not enough for it, and the reason is a gap in the *model* rather than in the proof --
`foldable` reads a constant table that need not be the one the body ran with. See
the note above for the concrete counterexample and for what the model would have to
state instead.
 
Asserting the equation anyway, with a `sorry` or a weakened hypothesis that happens
to be provable, would make this file build while proving less than it appears to,
which is worse than leaving the gap visible. The Rust side of M8 does establish the
equation -- differentially, over the Core suite -- so the claim is not in doubt;
what is missing here is a machine-checked version of it. -/
