import TptWasm.Basic
import TptWasm.Instruction
import TptWasm.Store
import TptWasm.Config
import TptWasm.Transition

/-!
# V0: invariants over configurations

The structural properties the Rust model checks at run time, proved here to be
preserved by the transition relation.

Two properties matter most for the rest of the ladder.

*Stack discipline*: no instruction pushes more than one value. This is what
makes the operand stack a stack rather than an unbounded accumulator.

*Authority discipline*: a step never changes the set of granted capabilities.
Execution cannot widen its own authority, which is the machine-level half of
V3.
-/

namespace TptWasm

/-- A step pushes at most one value onto the operand stack. -/
def PushesAtMostOne (c c' : Configuration) : Prop :=
  c'.operandStack.length <= c.operandStack.length + 1

/-- A step leaves the granted capabilities exactly as it found them. -/
def GrantsUnchanged (c c' : Configuration) : Prop := c'.grants = c.grants

/-- Advancing the program counter leaves the operand stack alone. -/
theorem advance_operandStack (c : Configuration) : (c.advance).operandStack = c.operandStack := by
  cases h : c.frames.reverse with
  | nil => simp [Configuration.advance, h]
  | cons frame rest => simp [Configuration.advance, h]

/-- Advancing the program counter leaves the grants alone. -/
theorem advance_grants (c : Configuration) : (c.advance).grants = c.grants := by
  cases h : c.frames.reverse with
  | nil => simp [Configuration.advance, h]
  | cons frame rest => simp [Configuration.advance, h]

/-- Every successful step of the instruction semantics respects both. -/
theorem opStep_disciplined {instr : Instr} {c c' : Configuration}
    (h : OpStep instr c (.step c')) : PushesAtMostOne c c' ∧ GrantsUnchanged c c' := by
  cases h <;>
    simp_all [PushesAtMostOne, GrantsUnchanged, Configuration.push, Configuration.pushCall] <;>
    omega

/-- The same, for the machine transition, which only adds a program counter. -/
theorem step_disciplined {c c' : Configuration} (h : Step c (.step c')) :
    PushesAtMostOne c c' ∧ GrantsUnchanged c c' := by
  cases h with
  | run fetched rule =>
      have h1 := opStep_disciplined rule
      simpa only [PushesAtMostOne, GrantsUnchanged, advance_operandStack c, advance_grants c]
        using h1

end TptWasm