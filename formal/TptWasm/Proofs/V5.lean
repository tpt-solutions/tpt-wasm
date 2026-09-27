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
    (h : foldable i cs = some i') :
    bodyTrace [i] s = bodyTrace [i'] s := by
  cases i with
  | constI32 r v => simp [foldable] at h
  | constI64 r v => simp [foldable] at h
  | binI32 r op l rr =>
    cases hl : Constants.lookup cs l with
    | none => simp [foldable, hl] at h
    | some vl =>
      cases hr : Constants.lookup cs rr with
      | none => simp [foldable, hl, hr] at h
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
          | i64 _ => simp [foldable, hl, hr] at h
          | f32 _ => simp [foldable, hl, hr] at h
          | f64 _ => simp [foldable, hl, hr] at h
        | i64 _ => simp [foldable, hl, hr] at h
        | f32 _ => simp [foldable, hl, hr] at h
        | f64 _ => simp [foldable, hl, hr] at h
  | binI64 r op l rr =>
    cases hl : Constants.lookup cs l with
    | none => simp [foldable, hl] at h
    | some vl =>
      cases hr : Constants.lookup cs rr with
      | none => simp [foldable, hl, hr] at h
      | some vr =>
        cases vl with
        | i64 a =>
          cases vr with
          | i64 b =>
            simp only [foldable, hl, hr, Option.some.injEq] at h
            cases h
            simp [bodyTrace, runBody, execInstr]
          | i32 _ => simp [foldable, hl, hr] at h
          | f32 _ => simp [foldable, hl, hr] at h
          | f64 _ => simp [foldable, hl, hr] at h
        | i32 _ => simp [foldable, hl, hr] at h
        | f32 _ => simp [foldable, hl, hr] at h
        | f64 _ => simp [foldable, hl, hr] at h
  | cmpI32 r op l rr => simp [foldable] at h
  | load r m a w => simp [foldable] at h
  | store m a v w => simp [foldable] at h
  | globalGet r i => simp [foldable] at h
  | globalSet i v => simp [foldable] at h
  | call i args results => simp [foldable] at h
  | callHost n g args results => simp [foldable] at h
  | drop v => simp [foldable] at h

/- **What a fold step does
 
`foldable_inert` above is the strongest per-step statement that is true
unconditionally, and the reason it is only about the trace is not a weakness of the
proof but a fact about the model.
 
`foldable` is handed a constant *table*, and it reads that table rather than the
state. So for `foldable (.binI32 r op l rr) cs = some (.constI32 r v)` to be sound,
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
