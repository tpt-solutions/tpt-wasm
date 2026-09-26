import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store
import TptWasm.Config
import TptWasm.Transition

/-!
# V2: memory safety

The property named in `spec.txt` section 28:

```
Valid(Module) => NoWasmMemoryOutOfBoundsAccess
```

Stated for the modeled subset: the only way a memory access can be *performed*
is if every byte it touches is inside the memory, and an access that is not in
bounds is a trap, which produces no configuration and therefore changes nothing.

The bounds check lives in `MemoryState.readLE?` and `MemoryState.writeLE?`, and
the `load` and `store` rules take the success or failure of those as a premise.
So the first two theorems below are the whole safety argument: the premise a
`load` or `store` rule needs can only hold inside the memory. The following
theorems show the other side, that an out-of-bounds access has a trap rule
available rather than being silently ignored.

The model does not formalize module validation, so the `Valid(Module)` premise
is not available here. Lifting the result to `Valid(Module) => ...` needs the
validation theory and remains M9 work.
-/

namespace TptWasm

namespace MemoryState

/-- A little-endian read only succeeds inside the memory.

This is the safety argument for loads: the guard in `readLE?` *is* the bounds
check, so no successful read can have left the memory. -/
theorem readLE?_inBounds (m : MemoryState) (address width : Nat) (raw : Nat)
    (h : m.readLE? address width = some raw) : address + width <= m.bytes.length := by
  unfold readLE? at h
  split at h
  · assumption
  · contradiction

/-- A little-endian write only succeeds inside the memory.

The same argument for stores. A write that would cross the end is rejected
whole, so it cannot write the part that was in bounds. -/
theorem writeLE?_inBounds (m : MemoryState) (address width value : Nat)
    (updated : MemoryState) (h : m.writeLE? address width value = some updated) :
    address + width <= m.bytes.length := by
  unfold writeLE? writeBytesLE? at h
  split at h
  · assumption
  · contradiction

end MemoryState

/-- V2: the premise a `load` rule needs implies the access is in bounds. -/
theorem load_performed_is_bounded (memory : MemoryState) (op : LoadOp) (address : Int)
    (raw : Nat) (h : memory.readLE? address.natAbs (op.width) = some raw) :
    address.natAbs + op.width <= memory.bytes.length :=
  MemoryState.readLE?_inBounds memory address.natAbs (op.width) raw h

/-- V2: the premise a `store` rule needs implies the access is in bounds. -/
theorem store_performed_is_bounded (memory : MemoryState) (op : StoreOp) (address : Int)
    (operand : Value) (updated : MemoryState)
    (h : memory.writeLE? address.natAbs (op.width) (storedBits operand) = some updated) :
    address.natAbs + op.width <= memory.bytes.length :=
  MemoryState.writeLE?_inBounds memory address.natAbs (op.width) (storedBits operand) updated h

/-- V2: an out-of-bounds load is a trap, not a silent no-op. -/
theorem oob_load_traps {c : Configuration} (op : LoadOp) (index : Nat) (arg : MemArg)
    (address : Int) (rest : List Value) (memory : MemoryState)
    (stack : c.operandStack = .i32 address :: rest)
    (mem : c.store.memory? index = some memory)
    (oob : memory.readLE? address.natAbs (op.width) = none) :
    OpStep (.load op index arg) c (.trap .memoryOutOfBounds) :=
  OpStep.loadTrap c op index arg address rest memory stack mem oob

/-- V2: an out-of-bounds store is a trap, not a silent no-op. -/
theorem oob_store_traps {c : Configuration} (op : StoreOp) (index : Nat) (arg : MemArg)
    (address : Int) (operand : Value) (rest : List Value) (memory : MemoryState)
    (stack : c.operandStack = .i32 address :: operand :: rest)
    (mem : c.store.memory? index = some memory)
    (typed : operand.type = op.operandType)
    (oob : memory.writeLE? address.natAbs (op.width) (storedBits operand) = none) :
    OpStep (.store op index arg) c (.trap .memoryOutOfBounds) :=
  OpStep.storeTrap c op index arg address operand rest memory stack mem typed oob


end TptWasm
