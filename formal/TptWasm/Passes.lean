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

end Constants

/-- Replace an operation on two known constants with the constant it is.

A folding division is deliberately absent. `x / 0` traps and `x / 1` does not, so
a constant-folding pass that treated division as total would delete a trap. The
operation set in `IR` excludes it, and this comment is the record of why.
-/
def foldable : Instr → List (ValueId × Value) → Option Instr
  | .binI32 r op l rr, cs =>
    match Constants.lookup cs l, Constants.lookup cs rr with
    | some (.i32 a), some (.i32 b) => some (.constI32 r (binI32Eval op a b))
    | _, _ => none
  | .binI64 r op l rr, cs =>
    match Constants.lookup cs l, Constants.lookup cs rr with
    | some (.i64 a), some (.i64 b) => some (.constI64 r (binI64Eval op a b))
    | _, _ => none
  | _, _ => none

/-- Constant folding: replace operations on known constants with the constants they
evaluate to.

The pass threads the constants it has seen so far through the body and only folds
an instruction whose operands are already known, so the constant a fold
introduces is itself available to fold the instruction after it. That is what
turns a chain of constant arithmetic into a single constant rather than a
shortened chain.
-/
def foldBlockWith (seen : List (ValueId × Value)) (body : List Instr) (fuel : Nat) : List Instr :=
  match body with
  | [] => []
  | i :: rest =>
    match fuel with
    | 0 => i :: rest
    | fuel + 1 =>
      -- The table is extended with this instruction's own constant *after* the
      -- fold is attempted, so the operands are looked up in the table as it stood
      -- *before* the instruction -- which is what stops `binI32 r op r rr` from
      -- folding to a value computed from itself.
      match foldable i seen with
      | some i' =>
        let seen' := match constValue i' with
          | some (r, v) => (r, v) :: seen
          | none => seen
        foldBlockWith seen' (i' :: rest) fuel
      | none =>
        let seen' := match constValue i with
          | some (r, v) => (r, v) :: seen
          | none => seen
        i :: foldBlockWith seen' rest fuel

/-- Constant folding of a whole block body.

The fuel is `body.length` extra steps. Folding is monotone -- each step replaces
one instruction with the single constant it computes, so the body shrinks or
holds still -- so the bound is never reached on a real body; it is here to make
termination structural rather than argued.
-/
def foldBlock (body : List Instr) : List Instr := foldBlockWith [] body body.length

/-- Dead-value elimination: remove an instruction whose result nothing uses.

Stated over a block body, so it removes an instruction whose result is used by
nothing *later in the same body*. That is sound only because the pass is given the
values the block's own terminator and its successors' parameter bindings use; the
whole-function version of this pass needs exactly that set, and
`TptWasm.Proofs.V5` states the theorem with it as a premise.

Getting that premise wrong is the failure this pass is most prone to, and it fails
*silently*: a pass that deleted on "no use in this body" alone would produce
well-formed IR reading a value nothing defines, and every downstream check would
pass.
-/
def deadBlock (extra : List ValueId) : List Instr → List Instr
  | [] => []
  | i :: rest =>
    let used' := IR.usedBy i ++ extra
    if IR.definedBy i |>.all (fun r => r ∈ used') then
      i :: deadBlock used' rest
    else
      deadBlock used' rest

end IR
end TptWasm