import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store

/-!
# The abstract machine configuration

The configuration of WebAssembly Core section 4, mirroring
`tpt_wasm_semantics::Configuration`:

```
Configuration = (store, frames, operandStack, controlStack)
```

A frame records which function is running, its locals, and how far it has got.
`operandBase` and `controlBase` delimit the part of each stack that belongs to
the frame, which is what makes a return hand the callee's results back to the
caller rather than discarding them.
-/

namespace TptWasm

/-- An activation record. -/
structure Frame where
  functionIndex : Nat
  locals : List Value
  pc : Nat
  returnArity : Nat
  operandBase : Nat
  controlBase : Nat
  deriving Repr, DecidableEq

/-- The kind of control label on the control stack. -/
inductive ControlKind where
  | block | loop | ifThenElse | function
  deriving DecidableEq, Repr

/-- A control label. `stackHeight` is the operand height when the label opened. -/
structure ControlFrame where
  kind : ControlKind
  stackHeight : Nat
  resultTypes : List ValType
  deriving Repr, DecidableEq

/-- The abstract machine configuration.

`grants` is the set of capabilities the embedder has granted to this
configuration. It is part of the configuration rather than a global, so the
capability boundary of V3 is stated over the machine state itself.
-/
structure Configuration where
  store : Store
  frames : List Frame
  operandStack : List Value
  controlStack : List ControlFrame
  grants : List String := []
  deriving Repr

/-- The proposition that `c` is structurally well formed.

The properties are the ones the Rust model checks in
`Configuration::check_invariants`: every frame names a known function, every
program counter is inside its body, frame bases are ordered and within their
stacks, control labels do not claim more stack than exists, and every global
holds a value of its declared type.
-/
def WellFormed (c : Configuration) : Prop :=
  (∀ f ∈ c.frames, ∃ fn, c.store.function? f.functionIndex = some fn ∧ f.pc ≤ fn.body.length) ∧
  (∀ f ∈ c.frames, f.operandBase ≤ c.operandStack.length) ∧
  (∀ f ∈ c.frames, f.controlBase ≤ c.controlStack.length) ∧
  (∀ g ∈ c.store.globals, g.value.type = g.ty)

/-- The maximum call depth the model admits before trapping. -/
def maxCallDepth : Nat := 1000

/-- The outcome of one transition `C → C'`. -/
inductive Outcome where
  | step (config : Configuration)
  | returning (values : List Value)
  | trap (trap : Trap)
  deriving Repr

namespace Configuration

/-- The last element of a list, or `none` when it is empty. -/
def last? : List α → Option α
  | [] => none
  | [x] => some x
  | _ :: xs => last? xs

/-- A list with its last element removed. -/
def withoutLast : List α → List α
  | [] => []
  | [_] => []
  | x :: xs => x :: withoutLast xs

/-- `xs` with its `index`-th entry replaced by `x`, or `none` when out of range. -/
def replaceAt? {α : Type} : List α → Nat → α → Option (List α)
  | [], _, _ => none
  | _, 0, x => some [x]
  | head :: rest, n + 1, x => (replaceAt? rest n x).map (head :: ·)

/-- A frame with its `index`-th local replaced, or `none` when out of range. -/
def setLocal? (frame : Frame) (index : Nat) (value : Value) : Option Frame :=
  (replaceAt? frame.locals index value).map fun locals => { frame with locals := locals }

/-- The active frame, when there is one. -/
def activeFrame? (c : Configuration) : Option Frame := last? c.frames

/-- The function the active frame is running, when the store has one. -/
def activeFunction? (c : Configuration) : Option FunctionState :=
  match c.activeFrame? with
  | some frame => c.store.function? frame.functionIndex
  | none => none

/-- The instruction the active frame is about to run. -/
def currentInstr? (c : Configuration) : Option Instr :=
  match c.activeFunction? with
  | some function =>
    match c.activeFrame? with
    | some frame => Store.get? function.body frame.pc
    | none => none
  | none => none

/-- The configuration with the active frame's program counter advanced. -/
def advance (c : Configuration) : Configuration :=
  match c.frames.reverse with
  | [] => c
  | frame :: rest => { c with frames := rest.reverse ++ [{ frame with pc := frame.pc + 1 }] }

/-- The configuration with a value pushed on the operand stack.

The top of the operand stack is the head of the list, so pushing prepends and
popping matches on the head. This is the orientation `execOp` uses.
-/
def push (c : Configuration) (value : Value) : Configuration :=
  { c with operandStack := value :: c.operandStack }

/-- The configuration with a new frame running `index`.

`args` are the arguments the callee receives, `rest` is the caller's operand
stack left below the call, and the new frame's base is the height of `rest`.
-/
def pushCall (c : Configuration) (index : Nat) (args : List Value) (rest : List Value)
    (results : Nat) (locals : List Value) : Configuration :=
  { c with
    frames :=
      c.frames ++
        [⟨index, args ++ locals, 0, results, rest.length, c.controlStack.length⟩]
    operandStack := rest
    controlStack := [] }

/-- The local types of `function`: its parameters followed by its declared locals. -/
def localTypesOf (function : FunctionState) : List ValType :=
  function.params ++ function.localTypes

end Configuration

end TptWasm
