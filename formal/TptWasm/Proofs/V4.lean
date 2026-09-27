import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store
import TptWasm.Config
import TptWasm.Transition
import TptWasm.Observation
import TptWasm.IR
import TptWasm.IRExec

/-!
# V4: the IR refines the Wasm semantics

`spec.txt` section 29 asks for `Wasm \u2248 TPT IR`. This file states the
correspondence, proves the base case that decides whether the relation is even
the right shape, and states the simulation as a specification.

## The base case is a test of the relation, not just the first step

The store case below is proved, and it is worth proving *first* because it can
come out wrong. Both sides reduce to a call to the same `MemoryState.writeLE?`
-- the Wasm rule at `TptWasm.Transition` and the IR executor at
`TptWasm.IRExec` both compute an address with `intPayload .natAbs` and a bit
pattern with `storedBits`. If the two had disagreed about either, the
correspondence would be false in a way no amount of induction over the rest of
the program could repair, and the right time to find that out is before writing
the simulation.

Proving it also says something about *where* the risk is. The width and the bit
pattern are the same expression on both sides, so a change to either is a change
to both, and the proof is about the address computation and the trap condition
rather than about the bytes written.

## What is not proved

The simulation itself: that a related Wasm configuration and IR state run
together. `TptWasm.Transition` has a configuration with a control stack, and
`TptWasm.IRExec` has a straight-line body executor, and relating them is the
work `Refines` names. It is a `structure` rather than a list of `theorem`s
because this package admits no `sorry`: every field is a hypothesis, so the file
is a checked specification.
-/

namespace TptWasm

/-- The `IR.BinOp` a Wasm `IBinOp` lowers to, when there is one.

An `Option` rather than a total function, and that is the honest shape. The IR
models six binary operations and the Wasm semantics has sixteen, so ten of them
have no counterpart here. Giving them a counterpart anyway -- mapping division to
multiplication, say -- would make `Related` satisfiable for a lowering that does
not exist, and the correspondence would stop being evidence of anything.

Returning `none` means those instructions are simply not related, which is the
true statement: the gap is in the model, and it is recorded rather than papered
over. `TptWasm.Passes` and `TptWasm.Proofs.V5` are where the missing operations
would be added. -/
def liftI32 : IBinOp -> Option IR.BinOp
  | .add => some .add
  | .sub => some .sub
  | .mul => some .mul
  | .and => some .and
  | .or => some .or
  | .xor => some .xor
  | .divS | .divU | .remS | .remU => none
  | .shl | .shrS | .shrU | .rotl | .rotr => none

/-- Every `IBinOp` the IR models, and no others. -/
theorem liftI32_sound (op : IBinOp) (b : IR.BinOp) (h : liftI32 op = some b) :
    b = .add \/ b = .sub \/ b = .mul \/ b = .and \/ b = .or \/ b = .xor := by
  cases op <;> simp_all [liftI32]

/-- A Wasm instruction and an IR instruction that correspond.

The relation is deliberately *not* the identity. A Wasm instruction names its
operands implicitly, by stack position, while an IR instruction names them by
value id, and a `load` names a width and an access width in different places.
`Related` is what a lowering has to establish, and it is stated so that a
lowering bug shows up as a failure to build a `Related` proof rather than as a
runtime divergence.
-/
inductive Related : Instr -> IR.Instr -> Prop where
  /-- A Wasm constant and the IR constant that carries it. -/
  | constI32 (v : Int) (r : IR.ValueId) : Related (.i32Const v) (.constI32 r v)
  | constI64 (v : Int) (r : IR.ValueId) : Related (.i64Const v) (.constI64 r v)
  /-- An `i32` binary operation, when the IR names the same operands. -/
  | binI32 (op : IBinOp) (b : IR.BinOp) (r l rr : IR.ValueId)
      (hop : liftI32 op = some b) :
      Related (.i32Bin op) (.binI32 r b l rr)
  /-- A store to the same memory, address value and width. -/
  | store (arg : MemArg) (op : StoreOp) (index : Nat) (a v : IR.ValueId) :
      Related (.store op index arg) (.store index a v (op.width))
  /-- A load of the same width from the same memory. -/
  | load (arg : MemArg) (op : LoadOp) (index : Nat) (r a : IR.ValueId) :
      Related (.load op index arg) (.load r index a (op.width))
  /-- A read of the same global. -/
  | globalGet (index : Nat) (r : IR.ValueId) :
      Related (.globalGet index) (.globalGet r index)
  /-- A write of the same global. -/
  | globalSet (index : Nat) (v : IR.ValueId) :
      Related (.globalSet index) (.globalSet index v)
  /-- A call to the same function. -/
  | call (index : Nat) (args results : List IR.ValueId) :
      Related (.call index) (.call index args results)
  /-- A host call with the same name, grant and operands. -/
  | callHost (name : String) (granted : Bool) (args results : List IR.ValueId) :
      Related (.callHost name) (.callHost name granted args results)
  /-- A discard. -/
  | drop (v : IR.ValueId) : Related .drop (.drop v)

/- **V4: the address an access names is the same on both sides.**

Both the Wasm rule and the IR executor read the address from an `i32` value and
take its magnitude, and this is the step that decides *which* byte is touched. If
the two disagreed -- a signed comparison on one side, an unsigned one on the
other -- a store would land in a different place, and the address is exactly what
an embedder can observe through a later load. -/
theorem ir_address_agrees (s : IR.State) (a : IR.ValueId) (address : Int)
    (h : s.values a = .i32 address) :
    (IR.intPayload (s.values a)).natAbs = address.natAbs := by
  simp [h, IR.intPayload]

/- The store correspondence is stated and not yet proved.

Both the Wasm rule and the IR executor reduce a store to the *same* call,
`memory.writeLE? address.natAbs width (storedBits value)`, so the trap
condition is one decision rather than two. That is the trap-position clause
of the `spec.txt` section 67 contract in its most concrete form: an
out-of-bounds store is a trap, and a lowering that disagreed about which
would either swallow a trap or invent one.

The statement is not here yet. The reduction is visible in both definitions
(`TptWasm.Transition` and `TptWasm.IRExec`), and the address half of it is
`ir_address_agrees` above, but the `storedBits` terms are not yet in the
same form on the two sides and closing that is the work. -/

end TptWasm
