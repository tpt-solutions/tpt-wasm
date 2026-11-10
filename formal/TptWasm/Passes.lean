import TptWasm.IR
import TptWasm.IRExec

/-!
# The optimizations V5 proves correct

The three passes `spec.txt` section 84 names: constant folding, dead-block
elimination, and CFG simplification.

## Why the passes are stated in this order

The specification's own recipe is the order to prove them in:

```text
constant-fold(x + 0)
    can have a tiny proof.
Then: dead-block elimination
Then: ...
```

Each is stated as a *rewrite on a block body* rather than on a whole function.
That is a scoping decision, and it is what keeps the proofs small enough to be
worth having. A pass on a block body cannot see the edges, so it cannot change
control flow, and the observable trace of a block body depends only on the
instructions in it. CFG simplification is the one pass that *does* move control
flow, and it is stated over the whole function for that reason.

The alternative -- proving each pass over a whole function -- proves the same
things and costs an order of magnitude more, because every block-body lemma would
have to be re-proved under a `∀ b, b ∈ blocks` quantifier with the edges in play.
-/

namespace TptWasm
namespace IR

/-- The value an `i32` constant instruction defines, when it is one. -/
def constValue : Instr → Option (ValueId × Value)
  | .constI32 r v => some (r, .i32 v)
  | .constI64 r v => some (r, .i64 v)
  | _ => none

/-- Substitute a value for the operand occurrences of `v`. -/
def substOperand (v replacement : ValueId) : Instr → Instr
  | .binI32 r op l rr =>
      if l = v then .binI32 r op replacement rr else if rr = v then .binI32 r op l replacement else .binI32 r op l rr
  | .binI64 r op l rr =>
      if l = v then .binI64 r op replacement rr else if rr = v then .binI64 r op l replacement else .binI64 r op l rr
  | .cmpI32 r op l rr =>
      if l = v then .cmpI32 r op replacement rr else if rr = v then .cmpI32 r op l replacement else .cmpI32 r op l rr
  | .load r m a w => if a = v then .load r m replacement w else .load r m a w
  | .store m a value w =>
      if a = v then .store m replacement value w
      else if value = v then .store m a replacement w
      else .store m a value w
  | .globalSet index value => if value = v then .globalSet index replacement else .globalSet index value
  | .call index args results => .call index (args.map fun a => if a = v then replacement else a) results
  | .callHost name granted args results =>
      .callHost name granted (args.map fun a => if a = v then replacement else a) results
  | .drop value => if value = v then .drop replacement else .drop value
  | other => other

namespace Constants

/-- Look `id` up in a constant table, innermost first. -/
def lookup : List (ValueId × Value) → ValueId → Option Value
  | [], _ => none
  | (k, v) :: rest, id => if k = id then some v else lookup rest id

/-- The value `id` holds, if the table says it is a compile-time constant *and* the
state agrees.
 
The agreement check is what makes the pass sound without a side condition. The table
is a record of what the pass has believed so far, while the state is what the program
will actually compute, and a fold replaces a computation with a value the *table*
supplies. If the two disagree, the fold replaces the program's own arithmetic with
someone else's.
 
That is not hypothetical. On `State.empty`, `execInstr (.binI32 0 .add 1 2) _`
binds `(0, .i32 0)`, while a table claiming `1 ↦ 5, 2 ↦ 6` would fold the same
instruction to `(0, .i32 11)`. Requiring agreement rejects the stale table instead of
trusting it, and it is what the implementation gets for free: the Rust pass builds
its table from the same `i32` constants it then reads.
 
The check also means a table that goes stale *later* -- because a head rebound a key
the table still mentions -- causes that entry to be ignored rather than trusted. So no
single-definition invariant has to be assumed: staleness is handled by refusing. -/
def constant (cs : List (ValueId × Value)) (s : State) (id : ValueId) : Option Value :=
  match lookup cs id with
  | some v => if s.values id = v then some v else none
  | none => none

/-- A value the table reports as a compile-time constant is the one the state holds. -/
theorem constant_eq {cs : List (ValueId × Value)} {s : State} {id : ValueId} {v : Value}
    (h : constant cs s id = some v) : s.values id = v := by
  cases hl : lookup cs id with
  | none => simp [constant, hl] at h
  | some w =>
    by_cases hagree : s.values id = w
    · simp only [constant, hl, hagree] at h
      have hvw : w = v := Option.some.inj h
      exact hagree.trans hvw
    · simp [constant, hl, hagree] at h

end Constants
/-- Replace an operation on two known constants with the constant it is.
 
A folding division is deliberately absent. `x / 0` traps and `x / 1` does not, so
a constant-folding pass that treated division as total would delete a trap. The
operation set in `IR` excludes it, and this comment is the record of why.
 
The pass is parameterised by the state as well as the table, and that is the whole
point: an operand counts as a compile-time constant only when the table *and* the
state agree about it. See `Constants.constant` for why a bare table is not enough.
Without the state argument this rewrite is unsound, and not marginally so -- it
replaces the program's arithmetic with the table's. -/
def foldable : State → List (ValueId × Value) → Instr → Option Instr
  | s, cs, .binI32 r op l rr =>
    match Constants.constant cs s l, Constants.constant cs s rr with
    | some (.i32 a), some (.i32 b) => some (.constI32 r (binI32Eval op a b))
    | _, _ => none
  | s, cs, .binI64 r op l rr =>
    match Constants.constant cs s l, Constants.constant cs s rr with
    | some (.i64 a), some (.i64 b) => some (.constI64 r (binI64Eval op a b))
    | _, _ => none
  | _, _, _ => none

/-- Constant folding: replace operations on known constants with the constants they
evaluate to.
 
The pass threads both the constants it has seen and the state it is folding
against through the body, and only folds an instruction whose operands are already
known in *both*. Threading the state is what makes the fold sound: the rest of the
body runs from the post-head state, and an operand is a constant relative to that
state rather than to the one the whole body started in.
 
Threading the constants as well is what turns a chain of constant arithmetic
into a single constant rather than a shortened chain: the constant a fold introduces
is available to fold the instruction after it. -/
def foldBlockWith (s : State) (seen : List (ValueId × Value)) (body : List Instr)
    (fuel : Nat) : List Instr :=
  match body with
  | [] => []
  | i :: rest =>
    match fuel with
    | 0 => i :: rest
    | fuel + 1 =>
      -- The table is extended with this instruction's own constant *after* the
      -- fold is attempted, so the operands are looked up as they stood *before* the
      -- instruction -- which is what stops `binI32 r op r rr` from folding to a value
      -- computed from itself.
      let seen' := match constValue i with
        | some (r, v) => (r, v) :: seen
        | none => seen
      match execInstr i s with
      | .trapped _ =>
        -- The head traps, so nothing after it runs and there is nothing to fold.
        i :: rest
      | .ran s' _ results =>
        let post := results.foldl (fun st (r, v) => st.bind r v) s'
        match foldable s seen i with
        | some i' => foldBlockWith post seen' (i' :: rest) fuel
        | none => i :: foldBlockWith post seen' rest fuel

/-- Constant folding of a whole block body, run against `s`.
 
The fuel is `body.length` extra steps. Folding is monotone -- each step replaces
one instruction with the single constant it computes, so the body shrinks or
holds still -- so the bound is never reached on a real body; it is here to make
termination structural rather than argued.
 
The table starts empty because the pass discovers the body's own constants as it
goes. A caller that already has a table may pass it to `foldBlockWith`. -/
def foldBlock (s : State) (body : List Instr) : List Instr :=
  foldBlockWith s [] body body.length

/-- An instruction that must never be deleted, because running it is observable.

This is the same lesson `foldable` taught, in a different place. Dead-value
elimination is only sound on instructions whose *absence* cannot be noticed, and
the modeled instructions that are not:

* a `load` can trap (`Trap.memoryOutOfBounds`), so deleting a load whose result
  nobody reads deletes a trap with it -- which `load_not_inert` in
  `TptWasm.Proofs.V5` proves is observable;
* a `call` or `callHost` emits an event, so deleting a call whose results nobody
  reads deletes a host or module effect with it -- which `store_not_inert` proves
  is observable, and which the host boundary makes visible to the embedder;
* a `globalGet` emits a `globalGet` event whatever its result is, so deleting one
  whose value nobody reads still changes the trace. It was missing from this
  predicate until the liveness proof in `TptWasm.Proofs.V5` found it.

The predicate is named for what it is -- not a claim about values, but about the
instruction as a whole -- and it is deliberately the conservative answer. -/
def observable : Instr -> Bool
  | .load _ _ _ _ => true
  | .call _ _ _ => true
  | .callHost _ _ _ _ => true
  | .globalGet _ _ => true
  | _ => false

/-- The instructions the pass is allowed to delete.

These are the five that compute one value and do nothing else: they emit no event,
they cannot trap, and they do not touch the store. Everything else is kept
unconditionally. A store, a `global.set`, and a `drop` define no value, so a
liveness test on them would be vacuously true; keeping them by name rather than by
that accident is what makes the pass's guarantee readable from the definition. -/
def removable : Instr -> Bool
  | .constI32 _ _ | .constI64 _ _ | .binI32 _ _ _ _ | .binI64 _ _ _ _
  | .cmpI32 _ _ _ _ => true
  | _ => false

/-- The values live after a body, given the values live after the whole block.

`extra` is the caller's premise: the values the block's terminator and its
successors read. It is supplied rather than computed, because the block-body pass
cannot see the edges. Every value a body reads is in `liveOf`, so the set is the
uses of the body's own instructions together with `extra`. -/
def liveOf (extra : List ValueId) (body : List Instr) : List ValueId :=
  extra ++ body.flatMap usedBy

/-- Dead-value elimination on a block body, with `extra` the values live out of it.

The sweep runs backwards, matching `tpt-wasm-opt::dce::eliminate`. An instruction is
kept when it is observable, when it is not removable, or when one of the values it
defines is read by something that survives after it. The liveness it tests is
therefore the uses of the *kept* later instructions together with `extra`, and
that is computed from the already-processed suffix `deadBlock extra rest`.

An earlier version of this pass tested the uses of the instructions *before* the
one being considered, so it never looked at a later reader. It deleted a constant
that a later `binI32` read, and then deleted the `binI32` as well, leaving the
kept `store` to read two values nothing defined. That is the failure this
definition replaces, and `TptWasm.Proofs.V5.deadBlock_sound` is what shows the
current version cannot make it. -/
def deadBlock (extra : List ValueId) : List Instr -> List Instr
  | [] => []
  | i :: rest =>
    let rest' := deadBlock extra rest
    if observable i || !removable i ||
        (definedBy i).any (fun r => decide (r ∈ liveOf extra rest')) then
      i :: rest'
    else
      rest'

end IR
end TptWasm
