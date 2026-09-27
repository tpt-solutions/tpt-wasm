import TptWasm.Basic
import TptWasm.Instruction

/-!
# Observable behaviour

`spec.txt` section 37 draws the compiler correctness argument as a chain of
boundaries:

```text
Core Wasm ≈ TPT-Wasm IR ≈ Optimized IR ≈ Machine IR ≈ Machine Code
```

and says each `≈` is a *refinement*. This file defines what "refinement" means, so
every later level states its obligation in one vocabulary instead of each inventing
its own.

## The definition used here

A refinement `A ≈ B` holds when the two observe the same thing: the same sequence
of observable events, and the same result or the same trap. Concretely, for every
execution the two produce equal `Observation`s.

This is deliberately *observational* and not up-to-equality. Two programs that
differ in their arithmetic, in which value is computed first, or in how many
blocks they have are the same program if the embedder cannot tell -- which is
exactly the property an optimizer needs, and exactly what the Rust contract
checker in `tpt_wasm_opt::contract` checks at run time. The formal claim and the
executable check are the same claim: the checker finds violations on real modules,
and this theory says what a violation would mean.

## Why events and not just a return value

Because a return value is not the whole of a WebAssembly program's meaning. A
program that returns the right number after performing one fewer `memory.store` is
a different program: the embedder observes the memory. The contract in `spec.txt`
section 67 says the same in its own words -- "an optimization that changes *when* a
trap occurs may be incorrect even if the eventual return value looks identical".

A trap is therefore an observation in its own right, ordered among the others rather
than reported separately, so that *when* it happens is part of what must be
preserved.
-/

namespace TptWasm

/-- How a whole-function run ended.

Deliberately *not* `TptWasm.Outcome`: that type carries a `Configuration`, so it
cannot be compared, and a refinement statement has to compare its two sides.
What a run ended with is the returned values or the trap, and nothing else about
the final configuration is observable to an embedder -- what an embedder observes
of the store is in the event trace.
-/
inductive Finish where
  /-- The function returned these values. -/
  | returned (values : List Value)
  /-- The function trapped. -/
  | trapped (trap : Trap)
  deriving DecidableEq, Repr

/-- One observable thing a program does.

The constructors are the clauses of the `spec.txt` section 67 contract: values,
control flow, traps, memory, global, table, host, and ordering. Pure computation
is absent by design -- an optimizer exists to remove it, and a semantic relation
that recorded arithmetic would say the optimizer may do nothing.
-/
inductive Event where
  /-- `width` bytes written at `address` of memory `memory`. -/
  | store (memory address width : Nat)
  /-- `width` bytes read from `address`. Reads are observable through a faulting
  one: deleting a read that would have been out of bounds deletes a trap. -/
  | load (address width : Nat)
  /-- The value of global `index` was read. -/
  | globalGet (index : Nat)
  /-- Global `index` was written. -/
  | globalSet (index : Nat)
  /-- The host function `name` was invoked with these arguments. -/
  | hostCall (name : String) (args : List Value)
  /-- A call to function `index`. -/
  | call (index : Nat)
  /-- Execution trapped. The constructor carries the trap's identity, so changing
  *which* trap occurs is a change of behaviour and not merely of success. -/
  | trap (trap : Trap)
  /-- The function returned these values. -/
  | ret (values : List Value)
  deriving DecidableEq, Repr

/-- What an execution of a whole function may be seen doing.

An `Observation` is the event trace together with the final outcome. A trap is
*also* recorded in the trace, because the position of a trap among the effects is
part of the program: a store that happened before a trap is observable, and one
that did not happen is not.
-/
structure Observation where
  /-- The events, in program order. -/
  events : List Event
  /-- The returned values, or the trap the function ended with. -/
  outcome : Finish
  deriving DecidableEq, Repr

namespace Observation

/-- The observation of a function that does nothing observable and returns
`values`. -/
def pure (values : List Value) : Observation := ⟨[], Finish.returned values⟩

/-- The event trace. -/
def trace (o : Observation) : List Event := o.events

/-- Whether the execution ended in a trap. -/
def trapped (o : Observation) : Bool :=
  match o.outcome with
  | Finish.trapped _ => true
  | _ => false

/-- Two executions are indistinguishable to an embedder.

The `=` is the whole point: refinement is *equality of observation*, so a proof that
two computations agree here has proven the compiler correct in the sense the
specification means.
-/
def Equiv (a b : Observation) : Prop := a = b

@[refl] theorem Equiv.refl (o : Observation) : o.Equiv o := rfl

theorem Equiv.symm {a b : Observation} (h : a.Equiv b) : b.Equiv a := Eq.symm h

theorem Equiv.trans {a b c : Observation} (h1 : a.Equiv b) (h2 : b.Equiv c) : a.Equiv c :=
  Eq.trans h1 h2

end Observation

end TptWasm