import TptWasm.Basic
import TptWasm.Store
import TptWasm.Observation

/-!
# The TPT-Wasm IR

The IR of `tpt-wasm-ir`, in the shape the Rust crate gives it: single-assignment
values, straight-line instructions inside basic blocks, block parameters binding
the values an incoming edge carries, and a terminator choosing the next block.

This is the level the verification ladder needs to exist between. `spec.txt`
section 37 places it here:

```text
Core Wasm ≈ TPT-Wasm IR ≈ Optimized IR ≈ Machine IR ≈ Machine Code
```

and V4's job is the first `≈`. The point of having a separate level is the one
the specification makes: "the compiler never becomes the definition of Wasm
semantics". A proof obligation is only meaningful against something the compiler
did not define, and that is the Wasm instruction semantics already modelled in
`TptWasm.Transition`.

## What is modelled, and what is not

The modelled subset is the instructions that can *observe* something -- memory
access, globals, calls, and the host boundary -- plus enough arithmetic
(`add`, `sub`, `mul`, and the comparisons) to have values to move around.

Everything else is deliberately absent. This is the same discipline the earlier
levels follow, and for the same reason: a larger model proves less, because each
additional constructor is a case the proofs must handle. The absent cases
(`f32`/`f64` arithmetic, `select`, conversions, `br_table`) are listed in
`docs/formal/verification-ladder.md` as remaining work rather than quietly
omitted, so that the gap is visible rather than assumed closed.
-/

namespace TptWasm
namespace IR

/-- A single-assignment value. -/
abbrev ValueId := Nat

/-- A block. -/
abbrev BlockId := Nat

/-- A global. -/
abbrev GlobalId := Nat

/-- A memory. -/
abbrev MemoryId := Nat

/-- The arithmetic the model evaluates, chosen so the folding proofs have
something to fold. Division is absent because it is the instruction that
*traps* on some operands and not others, which is precisely the distinction the
folding proof must respect; `TptWasm.Proofs.V5` adds it back as a partial
operation with a trap case. -/
inductive BinOp where
  | add | sub | mul | and | or | xor
  deriving DecidableEq, Repr

/-- The `i32` comparison, whose result is a value rather than a branch. -/
inductive ICmp where
  | eq | ne | ltS | ltU | gtS | gtU | leS | leU | geS | geU
  deriving DecidableEq, Repr

/-- A single IR instruction.

Each constructor names the value it defines. There is no "no result" form: the
instructions that produce nothing (`store`, `global.set`, `drop`) either are
effects, recorded in the trace, or name the value they discard. -/
inductive Instr where
  /-- The 32-bit constant `value`. -/
  | constI32 (result : ValueId) (value : Int)
  /-- The 64-bit constant `value`. -/
  | constI64 (result : ValueId) (value : Int)
  /-- `left op right`, as `i32`. -/
  | binI32 (result : ValueId) (op : BinOp) (left right : ValueId)
  /-- `left op right`, as `i64`. -/
  | binI64 (result : ValueId) (op : BinOp) (left right : ValueId)
  /-- `left <op> right`, yielding an `i32` 0 or 1. -/
  | cmpI32 (result : ValueId) (op : ICmp) (left right : ValueId)
  /-- Read `width` bytes at `address` from memory `memory`. -/
  | load (result : ValueId) (memory : MemoryId) (address : ValueId) (width : Nat)
  /-- Write `width` bytes of `value` at `address` to memory `memory`. -/
  | store (memory : MemoryId) (address value : ValueId) (width : Nat)
  /-- Read global `index`. -/
  | globalGet (result : ValueId) (index : GlobalId)
  /-- Write global `index`. -/
  | globalSet (index : GlobalId) (value : ValueId)
  /-- Call function `index` with `args`, producing `results`. -/
  | call (index : Nat) (args : List ValueId) (results : List ValueId)
  /-- Invoke the host function `name` with `args`, producing `results`.

  The `granted` premise is the whole host-safety argument, and it is carried
  here for the same reason `TptWasm.Proofs.V3` carries it: an ungranted host
  call has no rule, so it cannot be executed rather than being executed and
  refused afterwards. -/
  | callHost (name : String) (granted : Bool) (args : List ValueId) (results : List ValueId)
  /-- Discard `value`. -/
  | drop (value : ValueId)
  deriving DecidableEq, Repr

/-- How a block ends. -/
inductive Terminator where
  /-- Enter `target`, binding its parameters to `args`. -/
  | branch (target : BlockId) (args : List ValueId)
  /-- Enter one of two blocks on a condition.

  Each arm bundles its target with the values that bind the target's parameters,
  because an edge is a *pair*: the target and the argument list belong together,
  and separating them let them get out of step. Every other consumer of an edge
  in this model takes the pair too. -/
  | condBranch (condition : ValueId) (thenArm : BlockId × List ValueId)
      (elseArm : BlockId × List ValueId)
  /-- Return `values`. -/
  | ret (values : List ValueId)
  /-- Execution ends in a trap. -/
  | trap (trap : Trap)
  /-- Execution cannot continue. -/
  | unreachable
  deriving DecidableEq, Repr

/-- A basic block: parameters bound by the edge that enters it, a straight-line
body, and a terminator. -/
structure Block where
  /-- The block this is. -/
  id : BlockId
  /-- Parameters bound by the incoming edge. -/
  params : List ValueId
  /-- The straight-line body. -/
  instrs : List Instr
  /-- How the block ends. -/
  terminator : Terminator
  deriving DecidableEq, Repr

/-- A whole IR function. -/
structure Function where
  /-- Parameters, bound by the caller. -/
  params : List ValueId
  /-- Locals, which unlike values may be re-assigned. -/
  locals : List ValueId
  /-- Every value the function declares, so an operand can be checked against
  the declaration list the way the Rust verifier checks it. -/
  declared : List ValueId
  /-- The blocks. -/
  blocks : List Block
  /-- The block execution starts in. -/
  entry : BlockId
  deriving DecidableEq, Repr

namespace Terminator

/-- The blocks this terminator can enter. -/
def targets : Terminator → List (BlockId × List ValueId)
  | .branch t args => [(t, args)]
  | .condBranch _ (t₁, a₁) (t₂, a₂) => [(t₁, a₁), (t₂, a₂)]
  | .ret _ | .trap _ | .unreachable => []

/-- The values this terminator reads. -/
def reads : Terminator → List ValueId
  | .branch _ args => args
  | .condBranch c (t₁, a₁) (t₂, a₂) => c :: a₁ ++ a₂
  | .ret values => values
  | .trap _ | .unreachable => []

end Terminator

/-- Every value an instruction defines, in order. A call may define several. -/
def definedBy : Instr → List ValueId
  | .constI32 r _ | .constI64 r _ | .binI32 r _ _ _ | .binI64 r _ _ _
  | .cmpI32 r _ _ _ | .load r _ _ _ | .globalGet r _ => [r]
  | .store _ _ _ _ | .globalSet _ _ | .drop _ => []
  | .call _ _ results | .callHost _ _ _ results => results

/-- Every value an instruction reads. -/
def usedBy : Instr → List ValueId
  | .constI32 _ _ | .constI64 _ _ => []
  | .binI32 _ _ l r | .binI64 _ _ l r | .cmpI32 _ _ l r => [l, r]
  | .load _ _ address _ => [address]
  | .store _ address value _ => [address, value]
  | .globalGet _ _ => []
  | .globalSet _ value => [value]
  | .call _ args _ | .callHost _ _ args _ => args
  | .drop value => [value]

/-- Every value the function defines: parameters, block parameters, and
instruction results. -/
def definedValues (f : Function) : List ValueId :=
  f.params ++ f.locals ++ f.blocks.flatMap fun b => b.params ++ b.instrs.flatMap definedBy

/-- Every value the function mentions. -/
def usedValues (f : Function) : List ValueId :=
  f.blocks.flatMap fun b => b.params ++ b.instrs.flatMap usedBy ++ b.terminator.reads

end IR
end TptWasm