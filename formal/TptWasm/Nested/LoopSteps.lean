import TptWasm.Proofs.MachineDiamond

/-!
# The machine steps of a loop

A loop `whileNZ cond body` is laid out as `cond ; if body ; body ; loopBack d`, with `d` the
distance from the back-edge to the loop's condition. The `if` is `ifInstr |body|`: a non-zero
condition falls into the body, and a zero condition lands just past the back-edge, which is the
loop's end. The back-edge jumps to the condition.

The three lemmas are the machine's steps for these instructions: the zero test, the non-zero test,
and the back-edge. Each is one instruction of the interpreter, with no events.
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond IfControl

/-- The instruction at `pre.length` of a body laid out as `pre ++ (i :: rest) ++ post` is `i`. -/
theorem get_at_split (pre : List Instr) (i : Instr) (rest post : List Instr) :
    (pre ++ (i :: rest) ++ post)[pre.length]? = some i := by
  simp

/-- The instruction at `pre.length` of a body laid out as `pre ++ (i :: rest)` is `i`. -/
theorem get_at_split' (pre : List Instr) (i : Instr) (rest : List Instr) :
    (pre ++ (i :: rest))[pre.length]? = some i := by
  simp

/-- A zero condition of a loop's `if` lands just past the back-edge: the loop has ended. -/
theorem loop_zero_step (c : Configuration) (f : Frame) (fn : FunctionState)
    (pre body post : List Instr) (d : Nat) (rest : List Value)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = pre ++ (Instr.ifInstr body.length :: body) ++ (Instr.loopBack d :: post))
    (hpc : f.pc = pre.length) (hstack : c.operandStack = .i32 0 :: rest) :
    MReach c { c.advance.setActivePc (f.pc + 1 + body.length + 1) with operandStack := rest } [] := by
  have hA := advance_activeFrame c f hf
  have hfetch : c.currentInstr? = some (.ifInstr body.length) := by
    rw [currentInstr_of c f fn _ hf hfn hbody, hpc]
    exact get_at_split pre _ _ _
  have hst : c.advance.operandStack = .i32 0 :: rest := by
    rw [advance_operandStack]
    exact hstack
  have hrule := OpStep.ifElse c.advance body.length { f with pc := f.pc + 1 } rest hA hst
  have hev : ∀ i, c.currentInstr? = some i → stepEvents i c = [] := fun i hi => by
    rw [hfetch] at hi
    obtain rfl := (Option.some.inj hi).symm
    rfl
  exact mreach_of_step (Step.run hfetch hrule) hev (MReach.refl _)

/-- A non-zero condition of a loop's `if` falls into the body: the next instruction is the body's
first. -/
theorem loop_nonzero_step (c : Configuration) (f : Frame) (fn : FunctionState)
    (pre body post : List Instr) (d : Nat) (v : Int) (rest : List Value)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = pre ++ (Instr.ifInstr body.length :: body) ++ (Instr.loopBack d :: post))
    (hpc : f.pc = pre.length) (hstack : c.operandStack = .i32 v :: rest) (hv : v ≠ 0) :
    MReach c { c.advance with operandStack := rest } [] := by
  have hA := advance_activeFrame c f hf
  have hfetch : c.currentInstr? = some (.ifInstr body.length) := by
    rw [currentInstr_of c f fn _ hf hfn hbody, hpc]
    exact get_at_split pre _ _ _
  have hst : c.advance.operandStack = .i32 v :: rest := by
    rw [advance_operandStack]
    exact hstack
  have hrule := OpStep.ifThen c.advance body.length { f with pc := f.pc + 1 } v rest hA hst hv
  have hev : ∀ i, c.currentInstr? = some i → stepEvents i c = [] := fun i hi => by
    rw [hfetch] at hi
    obtain rfl := (Option.some.inj hi).symm
    rfl
  exact mreach_of_step (Step.run hfetch hrule) hev (MReach.refl _)

/-- The back-edge jumps to the loop's condition, which starts at `start`: `d` is the distance from
the back-edge back to `start`. -/
theorem loop_back_step (c : Configuration) (f : Frame) (fn : FunctionState)
    (pre post : List Instr) (d start : Nat)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = pre ++ (Instr.loopBack d :: post))
    (hpc : f.pc = pre.length) (hd : d + start = pre.length) :
    MReach c (c.setActivePc start) [] := by
  have hA := advance_activeFrame c f hf
  have hfetch : c.currentInstr? = some (.loopBack d) := by
    rw [currentInstr_of c f fn _ hf hfn hbody, hpc]
    exact get_at_split' pre _ _
  have hrule := OpStep.loopBack c.advance d { f with pc := f.pc + 1 } hA
  have hev : ∀ i, c.currentInstr? = some i → stepEvents i c = [] := fun i hi => by
    rw [hfetch] at hi
    obtain rfl := (Option.some.inj hi).symm
    rfl
  have htarget : f.pc + 1 - 1 - d = start := by omega
  have hsame : c.advance.setActivePc (f.pc + 1 - 1 - d) = c.setActivePc start := by
    rw [htarget]
    cases hr : c.frames.reverse with
    | nil => simp [Configuration.advance, Configuration.setActivePc, hr]
    | cons g rest => simp [Configuration.advance, Configuration.setActivePc, hr]
  have hstep : Step c (.step (c.setActivePc start)) := by
    have h0 : Step c (.step (c.advance.setActivePc (f.pc + 1 - 1 - d))) := Step.run hfetch hrule
    rw [hsame] at h0
    exact h0
  exact mreach_of_step hstep hev (MReach.refl _)

/-- Leaving the loop: after the zero test, the machine's configuration is the statement's end, with
the operand stack cleared. -/
theorem exit_eq (c1 : Configuration) (a b : Nat) :
    ({ (c1.setActivePc a).advance.setActivePc b with operandStack := [] } : Configuration) =
      ({ c1 with operandStack := [] } : Configuration).setActivePc b := by
  rw [advance_setActivePc, setActivePc_setActivePc, setActivePc_with_stack]

/-- Entering the body: after the non-zero test, the machine's configuration is the body's start,
with the operand stack cleared. -/
theorem nonzero_eq (c1 : Configuration) (a : Nat) :
    ({ (c1.setActivePc a).advance with operandStack := [] } : Configuration) =
      ({ c1 with operandStack := [] } : Configuration).setActivePc (a + 1) := by
  rw [advance_setActivePc, setActivePc_with_stack]

end Nested
end TptWasm
