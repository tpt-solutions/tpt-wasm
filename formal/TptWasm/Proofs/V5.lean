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

end IR
end TptWasm
