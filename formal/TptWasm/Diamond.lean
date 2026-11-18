import TptWasm.Proofs.V4Straight
import TptWasm.CFG

/-!
# A control-flow shape for V4: one `if`/`else` diamond

`TptWasm.Proofs.V4Straight` relates a straight-line Wasm body to a single IR block. This file adds
the smallest control-flow shape that needs more than one block: a function body of the form

```text
pre ; if (then thenArm) (else elseArm) end ; post
```

where `pre` leaves exactly one `i32` condition on the operand stack, the two arms are stack-neutral
(they leave the operand stack empty, so they change only the locals and the store), and `post`
runs after the join and leaves the function's results on the stack.

## What is defined here

* `Shape` is the source program: the four straight-line pieces.
* `runShape` is its Wasm meaning. It is built from the same `runWasm` the straight-line
  simulation proves against, so the only new definition is the rule for the `if` itself: pop an
  `i32`, run the arm the condition selects from an empty operand stack, and require the arm to
  leave it empty. A zero condition selects the else arm, as Wasm's `if` does.
* `lowerShape` is the lowering to a four-block IR function:
  block 0 runs `pre` and branches on the condition, blocks 1 and 2 run the arms, and block 3 runs
  `post` and returns. Every local is carried across an edge as a block parameter, so the join
  receives the locals of whichever arm ran. Each piece is lowered by `V4Straight.lower`.

## What is not modelled

Nesting, `loop`, `br`, `br_if`, `br_table`, calls, and arms that change the operand stack. Traps
are outside the statement proved in `TptWasm.Proofs.V4Diamond`, as they are for the straight-line
simulation. The lowered function's `declared` list is empty, because the IR verifier is not part of
this model; only the blocks and the entry are used by `IR.runFunction`.
-/

namespace TptWasm
namespace Diamond

open V4Straight

/-- A diamond: the straight-line pieces around one `if`. -/
structure Shape where
  /-- Runs first, and leaves exactly one `i32` condition on the operand stack. -/
  pre : List Instr
  /-- The arm taken when the condition is non-zero. -/
  thenArm : List Instr
  /-- The arm taken when the condition is zero. -/
  elseArm : List Instr
  /-- Runs after the join. -/
  post : List Instr

/-- The Wasm meaning of a diamond, from configuration `c`.

The `if` pops the condition, runs the selected arm from an empty operand stack, and requires the
arm to leave the operand stack empty. Each step is the same `runWasm` the straight-line simulation
is stated for. -/
def runShape (d : Shape) (c : Configuration) : Option (Configuration × List Event) :=
  match runWasm d.pre c with
  | some (c1, e1) =>
    match c1.operandStack with
    | [.i32 v] =>
      let c2 := { c1 with operandStack := [] }
      match runWasm (if v = 0 then d.elseArm else d.thenArm) c2 with
      | some (c3, e2) =>
        if c3.operandStack = [] then
          (runWasm d.post c3).map fun (c4, e3) => (c4, e1 ++ e2 ++ e3)
        else none
      | none => none
    | _ => none
  | none => none

/-- Block identifiers of the lowered diamond. -/
abbrev entryBlock : IR.BlockId := 0
abbrev thenBlock : IR.BlockId := 1
abbrev elseBlock : IR.BlockId := 2
abbrev joinBlock : IR.BlockId := 3

/-- The entry block: runs `pre` and branches on its condition. -/
def blkEntry (irPre : List IR.Instr) (cond : IR.ValueId) (locsPre : List IR.ValueId) : IR.Block :=
  { id := entryBlock, params := [], instrs := irPre,
    terminator := .condBranch cond (thenBlock, locsPre) (elseBlock, locsPre) }

/-- The then block: runs the then arm and joins. -/
def blkThen (params : List IR.ValueId) (irT : List IR.Instr) (locsT : List IR.ValueId) :
    IR.Block :=
  { id := thenBlock, params := params, instrs := irT, terminator := .branch joinBlock locsT }

/-- The else block: runs the else arm and joins. -/
def blkElse (params : List IR.ValueId) (irE : List IR.Instr) (locsE : List IR.ValueId) :
    IR.Block :=
  { id := elseBlock, params := params, instrs := irE, terminator := .branch joinBlock locsE }

/-- The join block: runs `post` and returns the function's results. -/
def blkJoin (params : List IR.ValueId) (irM : List IR.Instr) (final : List IR.ValueId) :
    IR.Block :=
  { id := joinBlock, params := params, instrs := irM, terminator := .ret final }

/-- Lower a diamond, given the value id each local holds (`locs`) and the next fresh id (`n`).

The parameters of the three blocks after the entry are fresh ids, one per local, allocated in the
order then, else, join. -/
def lowerShape (d : Shape) (locs : List IR.ValueId) (n : Nat) : Option IR.Function :=
  match lower d.pre [] locs n with
  | some (irPre, [cond], locsPre, n1) =>
    let L := locs.length
    let tp := List.range' n1 L
    let ep := List.range' (n1 + L) L
    let mp := List.range' (n1 + 2 * L) L
    match lower d.thenArm [] tp (n1 + 3 * L) with
    | some (irT, [], locsT, nT) =>
      match lower d.elseArm [] ep nT with
      | some (irE, [], locsE, nE) =>
        match lower d.post [] mp nE with
        | some (irM, final, _, _) =>
          some {
            params := locs
            locals := []
            declared := []
            blocks := [blkEntry irPre cond locsPre, blkThen tp irT locsT,
              blkElse ep irE locsE, blkJoin mp irM final]
            entry := entryBlock }
        | none => none
      | _ => none
    | _ => none
  | _ => none

end Diamond
end TptWasm
