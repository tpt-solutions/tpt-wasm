import TptWasm.Proofs.MachineDiamond

/-!
# Nested diamonds: the source language

A statement is straight-line code, or an `if`/`else` whose condition is straight-line code and
whose arms are statement lists. A statement list runs one statement after another. Statements are
stack-neutral: a statement starts and ends with an empty operand stack, and a condition leaves
exactly one `i32` on an otherwise empty stack, which the `if` consumes.

The list semantics `runStmt` / `runSeq` is the Wasm meaning, built on the same `runWasm` as the
diamond. `flatStmt` / `flatSeq` is the flat layout the machine runs: the markers `ifInstr` and
`elseInstr` carry the flat lengths of the arms, so a nested `if` is a flat `if` inside an arm.
-/

namespace TptWasm
namespace Nested

open V4Straight

/-- A statement: straight-line code, or an `if`/`else`. -/
inductive Stmt where
  | straight (ops : List Instr)
  | ifElse (cond : List Instr) (thenB elseB : List Stmt)

mutual

/-- The Wasm meaning of a statement from configuration `c`, with its events. -/
def runStmt : Stmt → Configuration → Option (Configuration × List Event)
  | .straight ops, c =>
    match runWasm ops c with
    | some (c', e) => if c.operandStack = [] ∧ c'.operandStack = [] then some (c', e) else none
    | none => none
  | .ifElse cond t e, c =>
    if c.operandStack = [] then
      match runWasm cond c with
      | some (c1, e1) =>
        match c1.operandStack with
        | [.i32 v] =>
          let c2 : Configuration := { c1 with operandStack := [] }
          match (if v = 0 then runSeq e c2 else runSeq t c2) with
          | some (c3, e2) => if c3.operandStack = [] then some (c3, e1 ++ e2) else none
          | none => none
        | _ => none
      | none => none
    else none

/-- The Wasm meaning of a statement list: each statement in turn. -/
def runSeq : List Stmt → Configuration → Option (Configuration × List Event)
  | [], c => some (c, [])
  | s :: rest, c =>
    match runStmt s c with
    | some (c1, e1) =>
      match runSeq rest c1 with
      | some (c2, e2) => some (c2, e1 ++ e2)
      | none => none
    | none => none

end

mutual

/-- The flat layout of a statement: its instructions in the order the machine runs them. -/
def flatStmt : Stmt → List Instr
  | .straight ops => ops
  | .ifElse cond t e =>
    cond ++ [.ifInstr (flatSeq t).length] ++ flatSeq t ++ [.elseInstr (flatSeq e).length] ++ flatSeq e

/-- The flat layout of a statement list. -/
def flatSeq : List Stmt → List Instr
  | [] => []
  | s :: rest => flatStmt s ++ flatSeq rest

end

mutual

/-- Every instruction of a statement is one the machine and the lowering support. -/
def SupStmt : Stmt → Prop
  | .straight ops => ∀ i ∈ ops, V4Diamond.Supported i
  | .ifElse cond t e => (∀ i ∈ cond, V4Diamond.Supported i) ∧ SupSeq t ∧ SupSeq e

/-- Every statement of a list is supported. -/
def SupSeq : List Stmt → Prop
  | [] => True
  | s :: ss => SupStmt s ∧ SupSeq ss

end

end Nested
end TptWasm
