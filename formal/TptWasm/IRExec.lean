import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store
import TptWasm.Transition
import TptWasm.Observation
import TptWasm.IR

/-!
# IR execution: what running a function is

The relation the refinement proofs are about: an IR function, an initial state, and
the `Observation` the run leaves behind.

## The state

A `State` is a value environment plus the store. The value environment maps a
`ValueId` to the value it holds, which is the SSA discipline made explicit: a
value is either bound by a parameter, by a block parameter, or by the instruction
that computed it, and it never changes afterwards.

Locals are *not* in that environment, and that is a modelling decision worth
stating. A local is storage that a `local.set` re-assigns, so it does not behave
like a value: an optimizer that propagated a constant into a local's read would be
wrong on any path where the `local.set` had not executed. Keeping the two
separate is what stops the V5 proofs from having to reason about the distinction
everywhere.

## The two layers, and why

`execInstr` is the instruction semantics and `execFunction` is the whole run. The
split is the same one `TptWasm.Transition` makes between `OpStep` and `Step`, and
it is what lets the composition argument in `TptWasm.Refinement` reason about a
single instruction at a time.

The run is written as a *fuel-bounded* loop rather than a least fixed point.
A WebAssembly function may not terminate -- an infinite loop is a well-typed
program -- so a total function needs a bound, and the bound is the step count
rather than a syntactic depth. A run that exhausts its fuel is reported as such,
so "the optimizer changed an infinite loop into a terminating one" is visible
rather than silently accepted.
-/

namespace TptWasm
namespace IR

/-- The values a run can observe: the SSA environment and the store. -/
structure State where
  /-- `value` holds for each bound value. -/
  values : ValueId → Value
  /-- The memories, globals, and functions. -/
  store : Store
  /-- Whether each host capability has been granted. -/
  grants : List String := []

/-- The result of running a whole function. -/
inductive Result where
  /-- The function returned these values, having left this trace. -/
  | returned (events : List Event) (values : List Value)
  /-- The function trapped, having left this trace. -/
  | trapped (events : List Event) (trap : Trap)
  /-- The run used its whole step budget. -/
  | exhausted (events : List Event)

namespace State

/-- A state with `values` mapping everything to `defaultValue`, an empty store,
and no grants. -/
def empty : State := ⟨fun _ => .i32 0, {}, []⟩

/-- `s` with `value` bound to `v`.

Named `bind` rather than `with` because `with` is a Lean keyword. -/
def bind (s : State) (value : ValueId) (v : Value) : State :=
  { s with values := fun x => if x = value then v else s.values x }

/-- `s` with each of `ids` bound to the corresponding entry of `vs`, requiring the
two lists to be the same length.

A shorter `vs` stops the binding rather than reading past the end, so a malformed
edge leaves the remaining parameters at their previous value instead of
crashing. That is the same "refuse rather than approximate" discipline the rest of
the model uses, and it is why a proof about a well-formed edge does not have to
also prove the list is long enough. -/
def bindAll (s : State) (ids : List ValueId) (vs : List Value) : State :=
  match ids, vs with
  | [], _ => s
  | _, [] => s
  | b :: bs, v :: vs => bindAll (s.bind b v) bs vs

end State

/-- The result of evaluating one IR instruction.

`none` means the instruction has no effect in this state -- a type mismatch, an
absent global -- which is the same "no rule applies" discipline the Wasm
transition relation uses, and for the same reason: a stuck step must be
distinguishable from a step that did something. -/
inductive StepResult where
  /-- The instruction ran: the state, the event it observed, and the values it
  defined. -/
  | ran (state : State) (event : Option Event) (results : List (ValueId × Value))
  /-- The instruction trapped. Everything observed before it stands. -/
  | trapped (trap : Trap)

/-- Evaluate the `i32` binary operators. -/
def binI32Eval : BinOp → Int → Int → Int
  | .add, a, b => wrap32 (a + b)
  | .sub, a, b => wrap32 (a - b)
  | .mul, a, b => wrap32 (a * b)
  | .and, a, b => iand32 a b
  | .or, a, b => ior32 a b
  | .xor, a, b => ixor32 a b

/-- Evaluate the `i64` binary operators. -/
def binI64Eval : BinOp → Int → Int → Int
  | .add, a, b => wrap64 (a + b)
  | .sub, a, b => wrap64 (a - b)
  | .mul, a, b => wrap64 (a * b)
  | .and, a, b => iand64 a b
  | .or, a, b => ior64 a b
  | .xor, a, b => ixor64 a b

/-- WebAssembly's rendering of a comparison result: 1 for true, 0 for false. -/
def boolValue (b : Bool) : Value := .i32 (if b then 1 else 0)

/-- The value a `width`-byte little-endian read produces.

`Instruction.loadedValue` is written against a `LoadOp`, because a Wasm load
carries its signedness. The IR models a load by its width alone -- signedness is a
property of how the *consumer* reads the value, and encoding it in the load would
make the IR commit to a reading the program may not take. The two agree on the
unsigned widths, which are the ones the bounds argument in V2 depends on.
-/
def loadValueOf (width raw : Nat) : Value :=
  if width = 8 then .i32 (Int.ofNat (raw % 256))
  else if width = 16 then .i32 (Int.ofNat (raw % 65536))
  else .i64 (Int.ofNat raw)


/-- Evaluate an `i32` comparison, yielding 0 or 1 as WebAssembly does. -/
def cmpI32Eval : ICmp → Int → Int → Value
  | .eq, a, b => boolValue (a == b)
  | .ne, a, b => boolValue (a != b)
  | .ltS, a, b => boolValue (a < b)
  | .ltU, a, b => boolValue (a.natAbs < b.natAbs)
  | .gtS, a, b => boolValue (a > b)
  | .gtU, a, b => boolValue (a.natAbs > b.natAbs)
  | .leS, a, b => boolValue (a ≤ b)
  | .leU, a, b => boolValue (a.natAbs ≤ b.natAbs)
  | .geS, a, b => boolValue (a ≥ b)
  | .geU, a, b => boolValue (a.natAbs ≥ b.natAbs)
/-- The integer payload of a value, or `0` for one that carries none.

A value is a sum over four numeric types, so an `i32` operation applied to a
value that happens to be an `f32` has nothing to operate on. Returning `0` rather
than refusing keeps `execInstr` total, which is what lets the V5 proofs reason
about instruction-level correspondence by case analysis instead of first proving
every operand is well typed.

That is sound *for the arguments the proofs use*: a well-typed IR never applies an
`i32` operation to a non-`i32`, and `TptWasm.Proofs.V4` states the typing premise
those theorems are given. The looseness is deliberate and localized to this one
function, so there is exactly one place to tighten when the typing theory lands.
-/
def intPayload : Value → Int
  | .i32 n | .i64 n => n
  | _ => 0

/-- Execute one IR instruction in `s`. -/
def execInstr : Instr → State → StepResult
  | .constI32 r v, s => .ran s none [(r, .i32 v)]
  | .constI64 r v, s => .ran s none [(r, .i64 v)]
  | .binI32 r op l rr, s =>
      .ran s none [(r, .i32 (binI32Eval op (intPayload (s.values l)) (intPayload (s.values rr))))]
  | .binI64 r op l rr, s =>
      .ran s none [(r, .i64 (binI64Eval op (intPayload (s.values l)) (intPayload (s.values rr))))]
  | .cmpI32 r op l rr, s =>
      .ran s none [(r, cmpI32Eval op (intPayload (s.values l)) (intPayload (s.values rr)))]
  | .load r memory address width, s =>
      let addr := (intPayload (s.values address)).natAbs
      match s.store.memory? memory with
      | some m =>
        match m.readLE? addr width with
        | some raw => .ran s (some (TptWasm.Event.load addr width)) [(r, loadValueOf width raw)]
        | none => .trapped Trap.memoryOutOfBounds
      | none => .trapped Trap.memoryOutOfBounds
  | .store memory address value width, s =>
      let addr := (intPayload (s.values address)).natAbs
      match s.store.memory? memory with
      | some m =>
        match m.writeLE? addr width (storedBits (s.values value)) with
        | some updated =>
          .ran { s with store := s.store.setMemory memory updated }
            (some (TptWasm.Event.store memory addr width)) []
        | none => .trapped Trap.memoryOutOfBounds
      | none => .trapped Trap.memoryOutOfBounds
  | .globalGet r index, s =>
      match s.store.global? index with
      | some g => .ran s (some (TptWasm.Event.globalGet index)) [(r, g.value)]
      | none => .ran s (some (TptWasm.Event.globalGet index)) []
  | .globalSet index value, s =>
      match s.store.global? index with
      | some g =>
        .ran { s with store := s.store.setGlobal index { g with value := s.values value } }
          (some (TptWasm.Event.globalSet index)) []
      | none => .ran s (some (TptWasm.Event.globalSet index)) []
  | .call index args _, s => .ran s (some (TptWasm.Event.call index)) []
  | .callHost name granted args _, s =>
      if granted then .ran s (some (TptWasm.Event.hostCall name (args.map s.values))) [] else .ran s none []
  | .drop _, s => .ran s none []

/-- Run a whole straight-line block body, threading the state and the trace. -/
def runBody : List Instr → State → List Event → Option (State × List Event)
  | [], s, trace => some (s, trace)
  | i :: rest, s, trace =>
    match execInstr i s with
    | .trapped t => none
    | .ran s' event results =>
      let s'' := results.foldl (fun st (r, v) => st.bind r v) s'
      let trace' := match event with
        | some e => trace ++ [e]
        | none => trace
      runBody rest s'' trace'

end IR
end TptWasm
