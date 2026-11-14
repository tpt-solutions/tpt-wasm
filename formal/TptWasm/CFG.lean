import TptWasm.IR
import TptWasm.IRExec

/-!
# Whole-function execution and control-flow pruning

`TptWasm.IRExec` runs one block body. This file runs a whole IR function: it follows
terminators from the entry block, binds each target's parameters to the edge's
arguments, and stops at a return, a trap, or when its fuel is spent. It also defines
the pruning that CFG simplification's unreachable-block removal performs.

## Scope

The model covers the terminators `branch`, `condBranch`, `ret`, `trap`, and
`unreachable`, over the instructions `IRExec` already models. It does not model
the Rust pass's other three steps (empty-block threading, block merging, and
constant-condition folding); see `TptWasm.Proofs.CFG` for what is and is not proved.

## Choices

* A block is entered with the fuel of one step. The run is bounded, as `IRExec`'s is,
  so a loop that never exits is reported as `exhausted` rather than diverging.
* `unreachable` and `trap` both end in a trap, and the trap value is kept, as Wasm
  reports it. A reference to a block the function does not contain is refused as
  `hostFailure`. A verified function never has one, so the case is not one a real
  run reaches.
* A condition is true when its `i32` payload is non-zero, as in Wasm's `br_if`.
-/

namespace TptWasm
namespace IR

/-- Run a block body, keeping the trap that ends it.

`IRExec.runBody` returns `none` on a trap and discards which trap it was. This
version returns it, so the whole-function result can report the trap a run ends in.
The two agree on everything else. -/
def runBodyR : List Instr → State → List Event → Sum (List Event × Trap) (State × List Event)
  | [], s, trace => .inr (s, trace)
  | i :: rest, s, trace =>
    match execInstr i s with
    | .trapped t => .inl (trace, t)
    | .ran s' event results =>
      let s'' := results.foldl (fun st (r, v) => st.bind r v) s'
      let trace' := match event with
        | some e => trace ++ [e]
        | none => trace
      runBodyR rest s'' trace'

/-- The block with identifier `id`, if the list contains one. -/
def findBlock (blocks : List Block) (id : BlockId) : Option Block :=
  blocks.find? (fun b => b.id == id)

/-- Run blocks starting from `b`, whose parameters are already bound in `s`.

Each block entered costs one step of `fuel`. The result carries the trace the run
left behind, in order. -/
def runBlocks (blocks : List Block) : Nat → Block → State → List Event → Result
  | 0, _, _, trace => .exhausted trace
  | n + 1, b, s, trace =>
    match runBodyR b.instrs s trace with
    | .inl (trace', t) => .trapped trace' t
    | .inr (s', trace') =>
      -- Enter `target`, binding its parameters to the edge's arguments, which are
      -- read from the state after this block's body.
      let enter : BlockId → List ValueId → Result := fun target args =>
        match findBlock blocks target with
        | none => .trapped trace' .hostFailure
        | some tb => runBlocks blocks n tb (s'.bindAll tb.params (args.map s'.values)) trace'
      match b.terminator with
      | .ret values => .returned trace' (values.map s'.values)
      | .trap t => .trapped trace' t
      | .unreachable => .trapped trace' .unreachable
      | .branch target args => enter target args
      | .condBranch c (thenTarget, thenArgs) (elseTarget, elseArgs) =>
        if intPayload (s'.values c) = 0 then enter elseTarget elseArgs
        else enter thenTarget thenArgs

/-- Run a function from its entry block, with `s` holding the parameters' values. -/
def runFunction (f : Function) (fuel : Nat) (s : State) : Result :=
  match findBlock f.blocks f.entry with
  | none => .trapped [] .hostFailure
  | some b => runBlocks f.blocks fuel b s []

/-- The function with only the blocks `keep` selects.

Blocks are kept or dropped by identifier, and the order of the rest is unchanged. -/
def prune (f : Function) (keep : BlockId → Bool) : Function :=
  { f with blocks := f.blocks.filter fun b => keep b.id }

/-- The blocks a run from the entry can enter.

This is the least set containing the entry and closed under the edges of the
function's own blocks. An edge from a block that is itself not reachable adds
nothing, which is why it is stated through membership in `blocks`. -/
inductive Reachable (f : Function) : BlockId → Prop
  | entry : Reachable f f.entry
  | edge {b : Block} {target : BlockId × List ValueId} :
      Reachable f b.id → b ∈ f.blocks → target ∈ b.terminator.targets →
      Reachable f target.1


/-- The run that enters block `target` with the values `args`, as an edge into it does.

A missing target is refused as `hostFailure`, as `runBlocks` refuses it. -/
def runAt (blocks : List Block) (n : Nat) (target : BlockId) (args : List ValueId)
    (s : State) (trace : List Event) : Result :=
  match findBlock blocks target with
  | none => .trapped trace .hostFailure
  | some tb => runBlocks blocks n tb (s.bindAll tb.params (args.map s.values)) trace

/-- A result that is not "ran out of fuel". Only such results describe what a function
does: an exhausted run says nothing about the function beyond its budget. -/
def NotExhausted : Result → Prop
  | .exhausted _ => False
  | _ => True

/-- `target` leads to `last` through `k` empty blocks.

Each link is a block with no instructions and no parameters, whose only action is to
branch to the next block with no arguments. Such a block changes neither the state nor
the trace, so a run that passes through the chain is the run that starts at `last`,
with `k` more steps of fuel spent. -/
inductive Chain (blocks : List Block) : BlockId → BlockId → Nat → Prop
  | done (target : BlockId) : Chain blocks target target 0
  | link {target next last : BlockId} {k : Nat} {c : Block} :
      findBlock blocks target = some c → c.instrs = [] → c.params = [] →
      c.terminator = .branch next [] → Chain blocks next last k →
      Chain blocks target last (k + 1)

/-- A block with its terminator redirected to `last` with no arguments, and nothing else
changed. The identifier is kept, so the block is still found under the same name. -/
def retarget (from_ last : BlockId) (b : Block) : Block :=
  if b.id = from_ then { b with terminator := .branch last [] } else b

/-- Every block whose identifier is `from_` now branches straight to `last`. -/
def threadEdge (f : Function) (from_ last : BlockId) : Function :=
  { f with blocks := f.blocks.map (retarget from_ last) }

end IR
end TptWasm
