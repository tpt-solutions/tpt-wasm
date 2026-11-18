import TptWasm.Nested.TrapProgram

/-!
# A concrete trapping program

The memory is four bytes. The condition is zero, so the else-arm runs: it stores `7` at address `0`,
which fits and emits one store event, then stores `7` at address `4`, which does not fit and traps.
The lowered program returns `trapped` with that one event and the out-of-bounds trap, which shows
the trap theorem is inhabited.
-/

namespace TptWasm
namespace Nested
namespace TrapExample

open V4Straight MachineDiamond

/-- The access argument: no offset. -/
def argT : MemArg := { offset := 0 }

/-- A four-byte memory with room for one `i32`. -/
def memT : MemoryState := ⟨List.replicate 4 0, 1⟩

/-- The store with one memory, holding `memT`. -/
def storeT : Store := { memories := [memT] }

/-- The start configuration: the frame's local 0 at `i32 0`, the stack empty. -/
def c0T : Configuration :=
  { store := storeT, frames := [⟨0, [.i32 0], 0, 0, 0, 0⟩], operandStack := [], controlStack := [] }

/-- The IR state the lowering starts from. -/
def sT : IR.State := { values := fun _ => .i32 0, store := storeT }

/-- The store at address `0`: it fits, and emits one store event. -/
def okStoreT : List Instr := [.i32Const 0, .i32Const 7, .store StoreOp.i32 0 argT]

/-- The store past the end of the memory: it traps. -/
def badStoreT : Instr := .store StoreOp.i32 0 argT

/-- The else-arm: the good store, then the one that traps. -/
def elseArmT : List Instr := okStoreT ++ [.i32Const 4, .i32Const 7, badStoreT]

/-- The program: the condition `0` selects the else-arm. -/
def bodyT : List Stmt :=
  [.ifElse [.i32Const 0] [.straight [.i32Const 5, .localSet 0]] [.straight elseArmT]]

/-- The return expression. -/
def retT : List Instr := [.localGet 0]

/-- The configuration after the condition, with the stack empty again. -/
def c1T : Configuration := { c0T with operandStack := [.i32 0] }

/-- The configuration after the good store and before the bad one: the stack holds the bad
store's operands, and the store has run. -/
def ckT : Configuration :=
  ((runWasm (okStoreT ++ [.i32Const 4, .i32Const 7]) c0T).map (·.1)).getD c0T

/-- The events of the run before the trap: the good store's one event. -/
def evT : List Event := [Event.store 0 0 4]

theorem supT : SupSeqL bodyT := by
  simp [bodyT, elseArmT, okStoreT, badStoreT, SupSeqL, SupStmtL, V4Diamond.Supported]

theorem bcT : BoundCorr c0T sT (List.range' 0 1) := by
  refine ⟨rfl, rfl, ⟨⟨0, [.i32 0], 0, 0, 0, 0⟩, rfl, rfl⟩, ?_⟩
  intro id hid
  simp at hid
  subst hid
  exact ⟨0, rfl⟩

theorem globalsT : V4Diamond.GlobalsI32 c0T.store := by
  intro g hg
  simp [c0T, storeT] at hg

/-- The else-arm traps: its good store emits `evT`, and the bad store traps with the out-of-bounds
trap. -/
theorem trapT : TrapSeq 0 bodyT c0T evT Trap.memoryOutOfBounds := by
  refine TrapSeq.head 0 _ _ c0T evT _
    (TrapS.armElse 0 [.i32Const 0] [.straight [.i32Const 5, .localSet 0]] [.straight elseArmT]
      c0T c1T [] evT Trap.memoryOutOfBounds rfl rfl rfl ?_)
  refine TrapSeq.head 0 _ _ _ evT _
    (TrapS.straight 0 elseArmT _ evT _ ⟨okStoreT ++ [.i32Const 4, .i32Const 7], badStoreT, [], ckT,
      rfl, rfl, rfl⟩)

/-- The example: the program lowers, and the lowered function returns `trapped` with the good
store's event and the out-of-bounds trap. -/
theorem trap_example :
    ∃ lf, lowerProg bodyT retT 1 = some lf ∧
      ∃ n0, IR.runFunction lf (n0 + 2) sT = .trapped evT Trap.memoryOutOfBounds := by
  obtain ⟨lf, hl⟩ : ∃ lf, lowerProg bodyT retT 1 = some lf := ⟨_, rfl⟩
  obtain ⟨n0, hn⟩ := nested_trap 0 bodyT retT 1 lf hl c0T evT Trap.memoryOutOfBounds sT bcT
    globalsT supT trapT
  exact ⟨lf, hl, n0, hn⟩

end TrapExample
end Nested
end TptWasm
