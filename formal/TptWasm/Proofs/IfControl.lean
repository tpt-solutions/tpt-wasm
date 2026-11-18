import TptWasm.Transition
import TptWasm.Invariants
import TptWasm.Proofs.V4Straight

/-!
# Structured `if`: the program counter lands where the layout says

`TptWasm.Transition` gives `if` and its `else` marker their rules (`ifThen`, `ifElse`, `elseSkip`),
and each rule moves the frame's program counter. This file proves that, for a flat body laid out
as

```text
pre ; if (thenLen) ; then-body ; else (elseLen) ; else-body ; post
```

each machine transition lands where that layout says it should:

* a non-zero condition falls into the then-body, at the first of its instructions;
* a zero condition lands at the first instruction of the else-body (or of `post`, when the
  else-body is empty);
* control that reaches the marker skips the else-body and lands at the first instruction of `post`.

The lemmas are about `Step`, the machine transition, so they cover the rules as the interpreter
uses them, with the program counter advanced before the rule applies.

## What this does not say

It does not say that a run of the flat body computes what the diamond's `runShape` computes. That
link, from the machine to the block graph, is the next step of V4 control flow.
-/

namespace TptWasm
namespace IfControl

/-- A list whose last element is `x` is the list before `x`, followed by `x`. -/
theorem frames_of_last : ∀ (l : List Frame) (x : Frame),
    Configuration.last? l = some x → ∃ rest, l = rest ++ [x]
  | [], _, h => by simp [Configuration.last?] at h
  | [y], x, h => ⟨[], by simp [Configuration.last?] at h; simp [h]⟩
  | y :: z :: zs, x, h => by
    obtain ⟨rest, hr⟩ := frames_of_last (z :: zs) x (by simpa [Configuration.last?] using h)
    exact ⟨y :: rest, by simp [hr]⟩

/-- The last element of a list followed by one more element is that element. -/
theorem last_append_single : ∀ (l : List Frame) (x : Frame),
    Configuration.last? (l ++ [x]) = some x
  | [], _ => rfl
  | y :: ys, x => by
    cases ys with
    | nil => rfl
    | cons z zs => exact last_append_single (z :: zs) x

/-- The active frame is the last frame: the frames are the frames before it, followed by it. -/
theorem frames_of_active (c : Configuration) (f : Frame) (h : c.activeFrame? = some f) :
    ∃ rest, c.frames = rest ++ [f] :=
  frames_of_last c.frames f h

/-- Advancing the program counter moves the active frame's pc by one and keeps its identity. -/
theorem advance_activeFrame (c : Configuration) (f : Frame) (h : c.activeFrame? = some f) :
    (c.advance).activeFrame? = some { f with pc := f.pc + 1 } := by
  obtain ⟨rest, hc⟩ := frames_of_active c f h
  simp [Configuration.advance, Configuration.activeFrame?, hc, last_append_single]

/-- Setting the program counter moves the active frame's pc to the new value. -/
theorem setActivePc_activeFrame (c : Configuration) (f : Frame) (q : Nat)
    (h : c.activeFrame? = some f) :
    (c.setActivePc q).activeFrame? = some { f with pc := q } := by
  obtain ⟨rest, hc⟩ := frames_of_active c f h
  simp [Configuration.setActivePc, Configuration.activeFrame?, hc, last_append_single]

/-- Advancing the program counter leaves the store alone. -/
theorem advance_store (c : Configuration) : (c.advance).store = c.store := by
  cases h : c.frames.reverse with
  | nil => simp [Configuration.advance, h]
  | cons frame rest => simp [Configuration.advance, h]

/-- The instruction at the active frame's pc is the body's entry at that index. -/
theorem currentInstr_of (c : Configuration) (f : Frame) (fn : FunctionState) (body : List Instr)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = body) :
    c.currentInstr? = body[f.pc]? := by
  simp [Configuration.currentInstr?, Configuration.activeFunction?, hf, hfn, hbody,
    V4Straight.get?_eq_getElem?]

/-- The flat body: `pre`, then the `if` with its then-body, the marker, the else-body, and `post`. -/
abbrev layout (pre thenBody elseBody post : List Instr) (thenLen elseLen : Nat) : List Instr :=
  pre ++ (.ifInstr thenLen :: thenBody) ++ (.elseInstr elseLen :: elseBody) ++ post

/-- Indexing into an append past the left part looks into the right part. -/
theorem get_append_length {α : Type} : ∀ (xs ys : List α) (k : Nat),
    (xs ++ ys)[xs.length + k]? = ys[k]?
  | [], _, _ => by simp
  | x :: xs, ys, k => by
    rw [List.length_cons, show xs.length + 1 + k = (xs.length + k) + 1 by omega,
      List.cons_append, List.getElem?_cons_succ]
    exact get_append_length xs ys k

/-- The `if` sits at index `pre.length`. -/
theorem layout_if (pre thenBody elseBody post : List Instr) (thenLen elseLen : Nat) :
    (layout pre thenBody elseBody post thenLen elseLen)[pre.length]? = some (.ifInstr thenLen) := by
  have hL : layout pre thenBody elseBody post thenLen elseLen =
      pre ++ ((.ifInstr thenLen :: thenBody) ++ ((.elseInstr elseLen :: elseBody) ++ post)) := by
    simp [layout, List.append_assoc]
  rw [hL]
  have := get_append_length pre ((.ifInstr thenLen :: thenBody) ++ ((.elseInstr elseLen :: elseBody) ++ post)) 0
  simp

/-- The first instruction after `if` is the first then-body entry, or the marker. -/
theorem layout_then (pre thenBody elseBody post : List Instr) (thenLen elseLen : Nat) :
    (layout pre thenBody elseBody post thenLen elseLen)[pre.length + 1]? =
      (thenBody ++ .elseInstr elseLen :: elseBody ++ post)[0]? := by
  have hL : layout pre thenBody elseBody post thenLen elseLen =
      pre ++ ((.ifInstr thenLen :: thenBody) ++ ((.elseInstr elseLen :: elseBody) ++ post)) := by
    simp [layout, List.append_assoc]
  rw [hL]
  have := get_append_length pre ((.ifInstr thenLen :: thenBody) ++ ((.elseInstr elseLen :: elseBody) ++ post)) 1
  simpa using this

/-- The marker sits at index `pre.length + thenBody.length + 1`. -/
theorem layout_marker (pre thenBody elseBody post : List Instr) (thenLen elseLen : Nat)
    (_hthen : thenBody.length = thenLen) :
    (layout pre thenBody elseBody post thenLen elseLen)[pre.length + thenBody.length + 1]? =
      some (.elseInstr elseLen) := by
  have hL : layout pre thenBody elseBody post thenLen elseLen =
      (pre ++ (.ifInstr thenLen :: thenBody)) ++ ((.elseInstr elseLen :: elseBody) ++ post) := by
    simp [layout, List.append_assoc]
  rw [hL]
  have hlen : (pre ++ (.ifInstr thenLen :: thenBody)).length = pre.length + thenBody.length + 1 := by
    simp; omega
  rw [← hlen]
  have := get_append_length (pre ++ (.ifInstr thenLen :: thenBody)) ((.elseInstr elseLen :: elseBody) ++ post) 0
  simp

/-- The first instruction of `post` sits at index `pre.length + thenBody.length + 2 + elseBody.length`. -/
theorem layout_post (pre thenBody elseBody post : List Instr) (thenLen elseLen : Nat)
    (_hthen : thenBody.length = thenLen) (_helse : elseBody.length = elseLen) :
    (layout pre thenBody elseBody post thenLen elseLen)[pre.length + thenBody.length + 2 + elseBody.length]? =
      post[0]? := by
  have hL : layout pre thenBody elseBody post thenLen elseLen =
      (pre ++ (.ifInstr thenLen :: thenBody) ++ (.elseInstr elseLen :: elseBody)) ++ post := by
    simp [layout, List.append_assoc]
  rw [hL]
  have hlen : (pre ++ (.ifInstr thenLen :: thenBody) ++ (.elseInstr elseLen :: elseBody)).length
      = pre.length + thenBody.length + 2 + elseBody.length := by simp; omega
  rw [← hlen]
  exact get_append_length _ post 0

/-- The first instruction after the marker's position is the first of the else-body, or of `post`
when the else-body is empty. -/
theorem layout_else_entry (pre thenBody elseBody post : List Instr) (thenLen elseLen : Nat) :
    (layout pre thenBody elseBody post thenLen elseLen)[pre.length + thenBody.length + 2]? =
      (elseBody ++ post)[0]? := by
  have hL : layout pre thenBody elseBody post thenLen elseLen =
      (pre ++ (.ifInstr thenLen :: thenBody)) ++ ((.elseInstr elseLen :: elseBody) ++ post) := by
    simp [layout, List.append_assoc]
  rw [hL]
  have hlen : (pre ++ (.ifInstr thenLen :: thenBody)).length = pre.length + thenBody.length + 1 := by
    simp; omega
  rw [show pre.length + thenBody.length + 2 = (pre ++ (.ifInstr thenLen :: thenBody)).length + 1
    by omega]
  have := get_append_length (pre ++ (.ifInstr thenLen :: thenBody))
    ((.elseInstr elseLen :: elseBody) ++ post) 1
  simpa using this

/-- A zero condition lands at the first instruction of the else-body, or of `post` when the
else-body is empty. -/
theorem if_zero_lands (c : Configuration) (f : Frame) (fn : FunctionState) (thenLen elseLen : Nat)
    (pre thenBody elseBody post : List Instr) (rest : List Value)
    (hthen : thenBody.length = thenLen) (_helse : elseBody.length = elseLen)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = layout pre thenBody elseBody post thenLen elseLen)
    (hpc : f.pc = pre.length) (hstack : c.operandStack = .i32 0 :: rest) :
    ∃ c1, c1 = { c.advance.setActivePc (f.pc + 1 + thenLen + 1) with operandStack := rest } ∧ Step c (.step c1) ∧
      c1.activeFrame? = some { f with pc := pre.length + thenLen + 2 } ∧
      c1.operandStack = rest ∧ c1.store = c.store ∧
      c1.currentInstr? = (elseBody ++ post)[0]? := by
  have hA := advance_activeFrame c f hf
  have hfetch : c.currentInstr? = some (.ifInstr thenLen) := by
    rw [currentInstr_of c f fn _ hf hfn hbody, hpc]
    simp [layout]
  have hst : (c.advance).operandStack = .i32 0 :: rest := by
    rw [advance_operandStack]
    exact hstack
  have hrule := OpStep.ifElse c.advance thenLen { f with pc := f.pc + 1 } rest hA hst
  have hact : ({ (c.advance).setActivePc (f.pc + 1 + thenLen + 1) with operandStack := rest }
      : Configuration).activeFrame? = some { f with pc := pre.length + thenLen + 2 } := by
    have hB := setActivePc_activeFrame c.advance { f with pc := f.pc + 1 } (f.pc + 1 + thenLen + 1) hA
    have hq : f.pc + 1 + thenLen + 1 = pre.length + thenLen + 2 := by omega
    show (c.advance.setActivePc (f.pc + 1 + thenLen + 1)).activeFrame? = _
    rw [hB, hq]
  have hsto : ({ (c.advance).setActivePc (f.pc + 1 + thenLen + 1) with operandStack := rest }
      : Configuration).store = c.store := by
    rw [setActivePc_store, advance_store]
  refine ⟨_, rfl, Step.run hfetch hrule, hact, rfl, hsto, ?_⟩
  rw [currentInstr_of _ { f with pc := pre.length + thenLen + 2 } fn _ hact
    (by rw [hsto]; exact hfn) hbody]
  subst hthen
  exact layout_else_entry pre thenBody elseBody post thenBody.length elseLen

/-- A non-zero condition falls into the then-body: the next instruction is its first. -/
theorem if_nonzero_falls (c : Configuration) (f : Frame) (fn : FunctionState)
    (pre thenBody elseBody post : List Instr) (rest : List Value) (v : Int)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = layout pre thenBody elseBody post thenBody.length elseBody.length)
    (hpc : f.pc = pre.length) (hstack : c.operandStack = .i32 v :: rest) (hv : v ≠ 0) :
    ∃ c1, c1 = { c.advance with operandStack := rest } ∧ Step c (.step c1) ∧
      c1.activeFrame? = some { f with pc := pre.length + 1 } ∧
      c1.operandStack = rest ∧ c1.store = c.store ∧
      c1.currentInstr? = (thenBody ++ .elseInstr elseBody.length :: elseBody ++ post)[0]? := by
  have hA := advance_activeFrame c f hf
  have hfetch : c.currentInstr? = some (.ifInstr thenBody.length) := by
    rw [currentInstr_of c f fn _ hf hfn hbody, hpc]
    exact layout_if pre thenBody elseBody post _ _
  have hst : (c.advance).operandStack = .i32 v :: rest := by
    rw [advance_operandStack]
    exact hstack
  have hrule := OpStep.ifThen c.advance thenBody.length { f with pc := f.pc + 1 } v rest hA hst hv
  have hact : ({ c.advance with operandStack := rest } : Configuration).activeFrame? =
      some { f with pc := pre.length + 1 } := by
    show c.advance.activeFrame? = _
    rw [hA, hpc]
  have hsto : ({ c.advance with operandStack := rest } : Configuration).store = c.store := by
    show c.advance.store = c.store
    rw [advance_store]
  refine ⟨_, rfl, Step.run hfetch hrule, hact, rfl, hsto, ?_⟩
  rw [currentInstr_of _ { f with pc := pre.length + 1 } fn _ hact (by rw [hsto]; exact hfn) hbody]
  exact layout_then pre thenBody elseBody post thenBody.length elseBody.length

/-- The marker skips the else-body: control that reaches it lands at the first instruction of
`post`. -/
theorem else_marker_skips (c : Configuration) (f : Frame) (fn : FunctionState)
    (pre thenBody elseBody post : List Instr)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = layout pre thenBody elseBody post thenBody.length elseBody.length)
    (hpc : f.pc = pre.length + thenBody.length + 1) :
    ∃ c1, c1 = c.advance.setActivePc (f.pc + 1 + elseBody.length) ∧ Step c (.step c1) ∧
      c1.activeFrame? = some { f with pc := pre.length + thenBody.length + 2 + elseBody.length } ∧
      c1.store = c.store ∧
      c1.currentInstr? = post[0]? := by
  have hA := advance_activeFrame c f hf
  have hfetch : c.currentInstr? = some (.elseInstr elseBody.length) := by
    rw [currentInstr_of c f fn _ hf hfn hbody, hpc]
    exact layout_marker pre thenBody elseBody post _ _ rfl
  have hrule := OpStep.elseSkip c.advance elseBody.length { f with pc := f.pc + 1 } hA
  have hact : (c.advance.setActivePc (f.pc + 1 + elseBody.length)).activeFrame? =
      some { f with pc := pre.length + thenBody.length + 2 + elseBody.length } := by
    have hB := setActivePc_activeFrame c.advance { f with pc := f.pc + 1 }
      (f.pc + 1 + elseBody.length) hA
    have hq : f.pc + 1 + elseBody.length = pre.length + thenBody.length + 2 + elseBody.length := by
      omega
    rw [hB, hq]
  have hsto : (c.advance.setActivePc (f.pc + 1 + elseBody.length)).store = c.store := by
    rw [setActivePc_store, advance_store]
  refine ⟨_, rfl, Step.run hfetch hrule, hact, hsto, ?_⟩
  rw [currentInstr_of _ { f with pc := pre.length + thenBody.length + 2 + elseBody.length } fn _ hact
    (by rw [hsto]; exact hfn) hbody]
  exact layout_post pre thenBody elseBody post thenBody.length elseBody.length rfl rfl

end IfControl
end TptWasm
