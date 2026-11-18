import TptWasm.Proofs.MachineDiamond

/-!
# Nested diamonds: the source language

A statement is straight-line code, an `if`/`else` whose condition is straight-line code and whose
arms are statement lists, or a `while` loop: `whileNZ cond body` runs `cond`, and while it leaves a
non-zero `i32` it runs `body` and repeats. Statements are stack-neutral: a statement starts and ends
with an empty operand stack, and a condition leaves exactly one `i32` on an otherwise empty stack,
which the `if` or the loop consumes.

The Wasm meaning is fuel-bounded. `runStmt k` and `runSeq k` run a statement with at most `k` loop
iterations in total; a run that needs more has no result. That is how a diverging loop is modelled
without a result: a Wasm run that exists is one that terminates within some fuel, and the theorems
are stated for some fuel. Statements without loops do not depend on the fuel.

`flatStmt` / `flatSeq` is the flat layout the machine runs. The machine layer does not cover loops
yet: `SupStmt` and `SupSeq` are the loop-free fragment the machine theorems take, and `SupStmtL` and
`SupSeqL` are the fragment the IR simulation takes, which allows loops. A loop's flat layout is `[]`
here and is never used, because the machine theorems require the loop-free fragment.
-/

namespace TptWasm
namespace Nested

open V4Straight

/-- A statement: straight-line code, an `if`/`else`, or a loop on a non-zero condition. -/
inductive Stmt where
  | straight (ops : List Instr)
  | ifElse (cond : List Instr) (thenB elseB : List Stmt)
  | whileNZ (cond : List Instr) (body : List Stmt)

mutual

/-- The Wasm meaning of a statement from configuration `c`, with at most `k` loop iterations. -/
def runStmt : Nat → Stmt → Configuration → Option (Configuration × List Event)
  | _, .straight ops, c =>
    match runWasm ops c with
    | some (c', e) => if c.operandStack = [] ∧ c'.operandStack = [] then some (c', e) else none
    | none => none
  | k, .ifElse cond t e, c =>
    if c.operandStack = [] then
      match runWasm cond c with
      | some (c1, e1) =>
        match c1.operandStack with
        | [.i32 v] =>
          let c2 : Configuration := { c1 with operandStack := [] }
          match (if v = 0 then runSeq k e c2 else runSeq k t c2) with
          | some (c3, e2) => if c3.operandStack = [] then some (c3, e1 ++ e2) else none
          | none => none
        | _ => none
      | none => none
    else none
  | 0, .whileNZ _ _, _ => none
  | k + 1, .whileNZ cond body, c =>
    if c.operandStack = [] then
      match runWasm cond c with
      | some (c1, e1) =>
        match c1.operandStack with
        | [.i32 v] =>
          let c2 : Configuration := { c1 with operandStack := [] }
          if v = 0 then some (c2, e1)
          else
            match runSeq k body c2 with
            | some (c3, e2) =>
              match runStmt k (.whileNZ cond body) c3 with
              | some (c4, e3) => some (c4, e1 ++ e2 ++ e3)
              | none => none
            | none => none
        | _ => none
      | none => none
    else none

/-- The Wasm meaning of a statement list: each statement in turn, with at most `k` loop iterations
in each. -/
def runSeq : Nat → List Stmt → Configuration → Option (Configuration × List Event)
  | _, [], c => some (c, [])
  | k, s :: rest, c =>
    match runStmt k s c with
    | some (c1, e1) =>
      match runSeq k rest c1 with
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
  | .whileNZ cond body =>
    cond ++ [.ifInstr (flatSeq body).length] ++ flatSeq body ++
      [.loopBack (cond.length + 1 + (flatSeq body).length)]

/-- The flat layout of a statement list. -/
def flatSeq : List Stmt → List Instr
  | [] => []
  | s :: rest => flatStmt s ++ flatSeq rest

end

mutual

/-- Every instruction of a statement is one the machine and the lowering support, loop-free. -/
def SupStmt : Stmt → Prop
  | .straight ops => ∀ i ∈ ops, V4Diamond.Supported i
  | .ifElse cond t e => (∀ i ∈ cond, V4Diamond.Supported i) ∧ SupSeq t ∧ SupSeq e
  | .whileNZ cond body => (∀ i ∈ cond, V4Diamond.Supported i) ∧ SupSeq body

/-- Every statement of a list is supported, loop-free. -/
def SupSeq : List Stmt → Prop
  | [] => True
  | s :: ss => SupStmt s ∧ SupSeq ss

end

mutual

/-- Every instruction of a statement is supported; loops are allowed, with a supported condition
and a supported body. -/
def SupStmtL : Stmt → Prop
  | .straight ops => ∀ i ∈ ops, V4Diamond.Supported i
  | .ifElse cond t e => (∀ i ∈ cond, V4Diamond.Supported i) ∧ SupSeqL t ∧ SupSeqL e
  | .whileNZ cond body => (∀ i ∈ cond, V4Diamond.Supported i) ∧ SupSeqL body

/-- Every statement of a list is supported, loops allowed. -/
def SupSeqL : List Stmt → Prop
  | [] => True
  | s :: ss => SupStmtL s ∧ SupSeqL ss

end

mutual

/-- A loop-free supported statement is supported in the loop-allowing sense. -/
theorem supStmt_toL : ∀ (s : Stmt), SupStmt s → SupStmtL s
  | .straight _, h => h
  | .ifElse _ t e, ⟨hc, ht, he⟩ => ⟨hc, supSeq_toL t ht, supSeq_toL e he⟩
  | .whileNZ _ _, ⟨hc, hb⟩ => ⟨hc, supSeq_toL _ hb⟩

/-- A loop-free supported list is supported in the loop-allowing sense. -/
theorem supSeq_toL : ∀ (ss : List Stmt), SupSeq ss → SupSeqL ss
  | [], _ => trivial
  | s :: ss, ⟨hs, hss⟩ => ⟨supStmt_toL s hs, supSeq_toL ss hss⟩

end

end Nested
end TptWasm
