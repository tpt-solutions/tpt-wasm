import TptWasm.Proofs.IfControl
import TptWasm.Proofs.V4Diamond
import TptWasm.Proofs.V1

/-!
# The machine runs a flat diamond the way `runShape` does

`TptWasm.Proofs.IfControl` shows the control rules land the program counter correctly. This file
connects them to the list semantics that `TptWasm.Diamond.runShape` and the V4 simulation use.

The machine is the interpreter `execOp`, applied to the active frame with its program counter
advanced, as `exec` does. A run is `MReach`: a sequence of such steps, recording the same events
`runWasm` records. The key fact is that a straight-line instruction does not depend on the program
counter: `execOp` at a set program counter is `execOp` at the original one, with the program
counter set on the result (`execOp_setActivePc`). With that, a straight-line piece of the flat body
runs as `runWasm` runs it, and the control instructions land where `IfControl` says.

## Status

Proved, with no `sorry`, and depending only on `propext` and `Quot.sound`:

* `execOp_setActivePc`: for every instruction `V4Diamond.Supported` names, the interpreter does not
  depend on the program counter (setting it first and running the instruction is the same as
  running it and then setting the counter on the result).
* `segment`: a straight-line piece of the body, run by `runWasm`, is run by the machine from the pc
  where the piece starts, and leaves it at the pc where the piece ends, with the same events.
* `machine_diamond_zero` and `machine_diamond_nonzero`: the machine runs each path of a flat diamond
  body, through the pre piece, the `if`, the chosen arm (and the marker, on the non-zero path), and
  `post`, and reaches the end of the body with the events and operand stack that `runShape` gives.
* `machine_diamond`: the case split on the condition, from `runShape`.

What this does not yet connect is the machine to the block graph (`lowerShape`, `runFunction`). The
V4 diamond theorem is about the list semantics and the block graph; `machine_diamond` is about the
machine and the list semantics. Closing the triangle needs the lowering to be stated over the flat
body, which is the next step. Block, loop and branch instructions are not in the model.
-/

namespace TptWasm
namespace MachineDiamond

open V4Straight IfControl

/-- The outcome of an instruction, with the active frame's program counter set to `q` when it
steps. A trap or a return is unchanged. -/
def Outcome.atPc (q : Nat) : Outcome → Outcome
  | .step c => .step (c.setActivePc q)
  | o => o

/-- Setting the program counter does not change the operand stack, the store, or the grants, so a
record update of any of those fields commutes with it. -/
theorem setActivePc_with_stack (c : Configuration) (q : Nat) (s : List Value) :
    ({ c.setActivePc q with operandStack := s } : Configuration) =
      ({ c with operandStack := s }).setActivePc q := by
  cases h : c.frames.reverse with
  | nil => simp [Configuration.setActivePc, h]
  | cons frame rest => simp [Configuration.setActivePc, h]

theorem setActivePc_with_store (c : Configuration) (q : Nat) (st : Store) :
    ({ c.setActivePc q with store := st } : Configuration) =
      ({ c with store := st }).setActivePc q := by
  cases h : c.frames.reverse with
  | nil => simp [Configuration.setActivePc, h]
  | cons frame rest => simp [Configuration.setActivePc, h]

/-- The stack of a configuration with the program counter set is the stack it had. -/
theorem setActivePc_operandStack' (c : Configuration) (q : Nat) :
    (c.setActivePc q).operandStack = c.operandStack := by
  cases h : c.frames.reverse with
  | nil => simp [Configuration.setActivePc, h]
  | cons frame rest => simp [Configuration.setActivePc, h]

/-- Pushing onto a configuration with the program counter set is the same as setting the program
counter after the push. -/
theorem setActivePc_push (c : Configuration) (q : Nat) (v : Value) :
    (c.setActivePc q).push v = (c.push v).setActivePc q := by
  unfold Configuration.push
  rw [setActivePc_with_stack, setActivePc_operandStack']

/-- The active frame after setting the program counter is the active frame with that pc. -/
theorem activeFrame_setActivePc (X : Configuration) (q : Nat) :
    (X.setActivePc q).activeFrame? = X.activeFrame?.map (fun f => { f with pc := q }) := by
  cases h : X.activeFrame? with
  | some f =>
    rw [setActivePc_activeFrame X f q h]
    rfl
  | none =>
    rcases List.eq_nil_or_concat X.frames with hn | ⟨L, b, hL⟩
    · simp [Configuration.setActivePc, Configuration.activeFrame?, hn, Configuration.last?]
    · exfalso
      have : X.activeFrame? = some b := by
        simp [Configuration.activeFrame?, hL, last_append_single]
      rw [h] at this
      cases this

/-- Setting the program counter of a configuration whose frames end in `f` replaces that last
frame by `f` with the new counter. -/
theorem setActivePc_frames_concat (X : Configuration) (L : List Frame) (f : Frame) (q : Nat)
    (h : X.frames = L ++ [f]) :
    (X.setActivePc q).frames = L ++ [{ f with pc := q }] := by
  unfold Configuration.setActivePc
  rw [h, List.reverse_append]
  simp

/-- Setting the local on a frame whose pc is `q` is setting it on the frame, then the pc. -/
theorem setLocal_setActivePc (f : Frame) (q index : Nat) (value : Value) :
    Configuration.setLocal? { f with pc := q } index value =
      (Configuration.setLocal? f index value).map (fun g => { g with pc := q }) := by
  unfold Configuration.setLocal?
  rw [Option.map_map]
  rfl

/-- Setting the program counter does not change the operand stack of a configuration. -/
theorem execOp_setActivePc_localGet (X : Configuration) (q index : Nat) :
    execOp (.localGet index) (X.setActivePc q) = (execOp (.localGet index) X).map (Outcome.atPc q) := by
  simp only [execOp, activeFrame_setActivePc]
  cases hf : X.activeFrame? with
  | none => simp
  | some f =>
    simp only [Option.map_some, Option.map_map]
    cases hl : Store.get? f.locals index with
    | none => simp
    | some v => simp [setActivePc_push, Outcome.atPc]

/-- Setting the program counter of a configuration whose frames are `L ++ [f]` gives the frames
`L ++ [f]` with the last frame's pc set; nothing else changes. -/
theorem setActivePc_of_concat (c : Configuration) (L : List Frame) (f : Frame) (q : Nat)
    (h : c.frames = L ++ [f]) :
    c.setActivePc q = { c with frames := L ++ [{ f with pc := q }] } := by
  unfold Configuration.setActivePc
  rw [h, List.reverse_append]
  simp

theorem execOp_setActivePc_localSet (X : Configuration) (q index : Nat) :
    execOp (.localSet index) (X.setActivePc q) = (execOp (.localSet index) X).map (Outcome.atPc q) := by
  simp only [execOp, activeFrame_setActivePc, setActivePc_operandStack']
  cases hf : X.activeFrame? with
  | none => simp
  | some f =>
    simp only [Option.map_some]
    cases hop : X.operandStack with
    | nil => simp
    | cons v rest =>
      obtain ⟨L, hL⟩ := frames_of_active X f hf
      simp only [setLocal_setActivePc]
      rw [Option.map_map, Option.map_map]
      congr 1
      funext g
      simp only [Function.comp, Outcome.atPc]
      rw [setActivePc_of_concat X L f q hL]
      simp only [List.dropLast_concat, hL]
      rw [setActivePc_of_concat { X with operandStack := rest, frames := L ++ [g] } L g q rfl]

theorem execOp_setActivePc_localTee (X : Configuration) (q index : Nat) :
    execOp (.localTee index) (X.setActivePc q) = (execOp (.localTee index) X).map (Outcome.atPc q) := by
  simp only [execOp, activeFrame_setActivePc, setActivePc_operandStack']
  cases hf : X.activeFrame? with
  | none => simp
  | some f =>
    simp only [Option.map_some]
    cases hop : X.operandStack with
    | nil => simp
    | cons v rest =>
      obtain ⟨L, hL⟩ := frames_of_active X f hf
      simp only [setLocal_setActivePc]
      rw [Option.map_map, Option.map_map]
      congr 1
      funext g
      simp only [Function.comp, Outcome.atPc]
      rw [setActivePc_of_concat X L f q hL]
      simp only [List.dropLast_concat, hL]
      have hR : ({ X with operandStack := v :: rest, frames := L ++ [g] } : Configuration).frames = L ++ [g] := rfl
      rw [setActivePc_of_concat _ L g q hR]

theorem execOp_setActivePc_globalGet (X : Configuration) (q index : Nat) :
    execOp (.globalGet index) (X.setActivePc q) = (execOp (.globalGet index) X).map (Outcome.atPc q) := by
  simp only [execOp, setActivePc_store]
  cases h : X.store.global? index with
  | none => simp
  | some global => simp [setActivePc_push, Outcome.atPc]

theorem execOp_setActivePc_globalSet (X : Configuration) (q index : Nat) :
    execOp (.globalSet index) (X.setActivePc q) = (execOp (.globalSet index) X).map (Outcome.atPc q) := by
  simp only [execOp, setActivePc_store, setActivePc_operandStack']
  cases hg : X.store.global? index with
  | none => simp
  | some global =>
    cases hop : X.operandStack with
    | nil => simp
    | cons v rest =>
      cases hr : X.frames.reverse with
      | nil => simp [Configuration.setActivePc, hr, Outcome.atPc] <;> split <;> simp_all
      | cons g rest2 => simp [Configuration.setActivePc, hr, Outcome.atPc] <;> split <;> simp_all

theorem execOp_setActivePc_i32Const (X : Configuration) (q : Nat) (v : Int) :
    execOp (.i32Const v) (X.setActivePc q) = (execOp (.i32Const v) X).map (Outcome.atPc q) := by
  simp only [execOp, setActivePc_push, Option.map_some, Outcome.atPc]

-- The shared script for instructions that act on the operand stack and the store only: split the
-- frame list and the operand stack, then unfold `setActivePc` and compare.
set_option hygiene false in
macro "stack_pc_tac" : tactic => `(tactic| (
  cases hr : X.frames.reverse <;>
  rcases hs : X.operandStack with _ | ⟨a, _ | ⟨b, rest⟩⟩ <;>
  (try cases a) <;> (try cases b) <;>
  simp_all [execOp, Configuration.setActivePc, Outcome.atPc] <;>
  (repeat' split) <;> simp_all [execOp, Configuration.setActivePc, Outcome.atPc]))

theorem execOp_setActivePc_i32Bin (X : Configuration) (q : Nat) (op : IBinOp) :
    execOp (.i32Bin op) (X.setActivePc q) = (execOp (.i32Bin op) X).map (Outcome.atPc q) := by
  stack_pc_tac

theorem execOp_setActivePc_i32Cmp (X : Configuration) (q : Nat) (op : ICmp) :
    execOp (.i32Cmp op) (X.setActivePc q) = (execOp (.i32Cmp op) X).map (Outcome.atPc q) := by
  stack_pc_tac

theorem execOp_setActivePc_load (X : Configuration) (q : Nat) (op : LoadOp) (m : Nat) (arg : MemArg) :
    execOp (.load op m arg) (X.setActivePc q) = (execOp (.load op m arg) X).map (Outcome.atPc q) := by
  stack_pc_tac

theorem execOp_setActivePc_store (X : Configuration) (q : Nat) (op : StoreOp) (m : Nat) (arg : MemArg) :
    execOp (.store op m arg) (X.setActivePc q) = (execOp (.store op m arg) X).map (Outcome.atPc q) := by
  stack_pc_tac

/-- The interpreter does not depend on the program counter for any instruction that is not a
control instruction: setting the counter first and running the instruction gives the same
outcome as running it and then setting the counter on the result. -/
theorem execOp_setActivePc (i : Instr) (hs : V4Diamond.Supported i) (X : Configuration) (q : Nat) :
    execOp i (X.setActivePc q) = (execOp i X).map (Outcome.atPc q) := by
  cases i with
  | i32Const v => exact execOp_setActivePc_i32Const X q v
  | i32Bin op => exact execOp_setActivePc_i32Bin X q op
  | i32Cmp op => exact execOp_setActivePc_i32Cmp X q op
  | load op m arg => exact execOp_setActivePc_load X q op m arg
  | store op m arg => exact execOp_setActivePc_store X q op m arg
  | globalGet index => exact execOp_setActivePc_globalGet X q index
  | globalSet index => exact execOp_setActivePc_globalSet X q index
  | localGet index => exact execOp_setActivePc_localGet X q index
  | localSet index => exact execOp_setActivePc_localSet X q index
  | localTee index => exact execOp_setActivePc_localTee X q index
  | _ => simp [V4Diamond.Supported] at hs

/-- Advancing a configuration whose pc is set is setting the pc one further on. -/
theorem advance_setActivePc (X : Configuration) (p : Nat) :
    (X.setActivePc p).advance = X.setActivePc (p + 1) := by
  cases hr : X.frames.reverse with
  | nil => simp [Configuration.advance, Configuration.setActivePc, hr]
  | cons g rest => simp [Configuration.advance, Configuration.setActivePc, hr]

/-- Setting a local keeps the frame's function index. -/
theorem setLocal_functionIndex (frame : Frame) (index : Nat) (v : Value) (g : Frame)
    (h : Configuration.setLocal? frame index v = some g) : g.functionIndex = frame.functionIndex := by
  unfold Configuration.setLocal? at h
  cases hr : Configuration.replaceAt? frame.locals index v with
  | none => simp [hr] at h
  | some l =>
    simp only [hr, Option.map_some, Option.some.injEq] at h
    subst h
    rfl

/-- A supported step keeps the store's functions, and the active frame's function index. -/
theorem execOp_frames (i : Instr) (hs : V4Diamond.Supported i) (c c1 : Configuration)
    (h : execOp i c = some (.step c1)) :
    c1.store.functions = c.store.functions ∧
      ∀ f, c.activeFrame? = some f → ∃ g, c1.activeFrame? = some g ∧ g.functionIndex = f.functionIndex := by
  cases i <;> simp only [V4Diamond.Supported] at hs
  case localGet index =>
    simp only [execOp] at h
    cases hf : c.activeFrame? with
    | none => simp [hf] at h
    | some frame =>
      cases hl : Store.get? frame.locals index with
      | none => simp [hf, hl] at h
      | some v =>
        simp only [hf, hl, Option.map_some, Option.some.injEq, Outcome.step.injEq] at h
        subst h
        refine ⟨rfl, fun f hf' => ?_⟩
        have hff := Option.some.inj hf'
        subst hff
        exact ⟨frame, hf, rfl⟩
  case localSet index =>
    simp only [execOp] at h
    cases hf : c.activeFrame? with
    | none => simp [hf] at h
    | some frame =>
      cases hop : c.operandStack with
      | nil => simp [hf, hop] at h
      | cons v rest =>
        simp only [hf, hop] at h
        cases hs : Configuration.setLocal? frame index v with
        | none => simp [hs] at h
        | some g =>
          simp only [hs, Option.map_some, Option.some.injEq, Outcome.step.injEq] at h
          subst h
          obtain ⟨L, hL⟩ := frames_of_active c frame hf
          refine ⟨rfl, fun f hf' => ?_⟩
          have hff := Option.some.inj hf'
          subst hff
          refine ⟨g, ?_, setLocal_functionIndex frame index v g hs⟩
          show Configuration.last? (c.frames.dropLast ++ [g]) = some g
          rw [hL, List.dropLast_concat]
          exact last_append_single L g
  case localTee index =>
    simp only [execOp] at h
    cases hf : c.activeFrame? with
    | none => simp [hf] at h
    | some frame =>
      cases hop : c.operandStack with
      | nil => simp [hf, hop] at h
      | cons v rest =>
        simp only [hf, hop] at h
        cases hs : Configuration.setLocal? frame index v with
        | none => simp [hs] at h
        | some g =>
          simp only [hs, Option.map_some, Option.some.injEq, Outcome.step.injEq] at h
          subst h
          obtain ⟨L, hL⟩ := frames_of_active c frame hf
          refine ⟨rfl, fun f hf' => ?_⟩
          have hff := Option.some.inj hf'
          subst hff
          refine ⟨g, ?_, setLocal_functionIndex frame index v g hs⟩
          show Configuration.last? (c.frames.dropLast ++ [g]) = some g
          rw [hL, List.dropLast_concat]
          exact last_append_single L g
  all_goals (
    simp only [execOp] at h
    repeat' split at h
    all_goals (
      first
      | (rcases h with ⟨a, -, rfl⟩
         exact ⟨rfl, fun f hf' => ⟨f, hf', rfl⟩⟩; done)
      | (simp only [Option.some.injEq] at h
         subst h
         exact ⟨rfl, fun f hf' => ⟨f, hf', rfl⟩⟩; done)
      | (simp at h; done)))

/-- The events an instruction records do not depend on the program counter. -/
theorem stepEvents_setActivePc (i : Instr) (X : Configuration) (q : Nat) :
    stepEvents i (X.setActivePc q) = stepEvents i X := by
  cases i <;> simp [stepEvents, setActivePc_operandStack']

/-- A machine run: each step runs the instruction at the active frame's pc, with the pc advanced,
and records the events `runWasm` records for it. -/
inductive MReach : Configuration → Configuration → List Event → Prop
  | refl (c : Configuration) : MReach c c []
  | step {c c1 c2 : Configuration} {i : Instr} {evs : List Event} :
      c.currentInstr? = some i → execOp i c.advance = some (.step c1) →
      MReach c1 c2 evs → MReach c c2 (stepEvents i c ++ evs)

theorem MReach.trans {a b c : Configuration} {e1 e2 : List Event} (h1 : MReach a b e1)
    (h2 : MReach b c e2) : MReach a c (e1 ++ e2) := by
  induction h1 with
  | refl _ => simpa using h2
  | @step c c1 c2 i evs hcur hexec _ ih =>
    rw [List.append_assoc]
    exact MReach.step hcur hexec (ih h2)

theorem runWasm_cons_step {i : Instr} {rest : List Instr} {c c' : Configuration} {evs : List Event}
    (hr : runWasm (i :: rest) c = some (c', evs)) :
    ∃ c1 evs', execOp i c = some (.step c1) ∧ runWasm rest c1 = some (c', evs') ∧
      evs = stepEvents i c ++ evs' := by
  cases hexec : execOp i c with
  | none => simp [runWasm, hexec] at hr
  | some o =>
    cases o with
    | trap t => simp [runWasm, hexec] at hr
    | returning vs => simp [runWasm, hexec] at hr
    | step c1 =>
      obtain ⟨evs', hr', hev⟩ := runWasm_cons_inv hexec hr
      exact ⟨c1, evs', rfl, hr', hev⟩

/-- A straight-line piece of the body, run by `runWasm`, is run by the machine from the pc where it
starts, and leaves the machine where the list semantics leaves it, with the same events. -/
theorem segment : ∀ (xs : List Instr) (p : Nat) (P R : List Instr)
    (cw cw' : Configuration) (evs : List Event) (f : Frame) (fn : FunctionState),
    P.length = p → (∀ i ∈ xs, V4Diamond.Supported i) → cw.activeFrame? = some f →
    cw.store.function? f.functionIndex = some fn → fn.body = P ++ xs ++ R →
    runWasm xs cw = some (cw', evs) →
    MReach (cw.setActivePc p) (cw'.setActivePc (p + xs.length)) evs
  | [], p, P, R, cw, cw', evs, f, fn, hp, _, _, _, _, hr => by
    simp only [runWasm, Option.some.injEq, Prod.mk.injEq] at hr
    obtain ⟨rfl, rfl⟩ := hr
    simpa using MReach.refl _
  | i :: xs, p, P, R, cw, cw', evs, f, fn, hp, hs, hf, hfn, hbody, hr => by
    have hsi : V4Diamond.Supported i := hs i (by simp)
    obtain ⟨cw1, evs', hexec, hr', hev⟩ := runWasm_cons_step hr
    obtain ⟨hfs, hfr⟩ := execOp_frames i hsi cw cw1 hexec
    obtain ⟨g, hg, hgi⟩ := hfr f hf
    have hb : fn.body = P ++ (i :: (xs ++ R)) := by
      rw [hbody]
      simp [List.append_assoc]
    have hcur : (cw.setActivePc p).currentInstr? = some i := by
      rw [currentInstr_of (cw.setActivePc p) { f with pc := p } fn _
        (by rw [activeFrame_setActivePc, hf]; rfl)
        (by rw [setActivePc_store]; exact hfn) hb]
      subst hp
      simpa using get_append_length P (i :: (xs ++ R)) 0
    have hexs : execOp i (cw.setActivePc p).advance = some (.step (cw1.setActivePc (p + 1))) := by
      rw [advance_setActivePc, execOp_setActivePc i hsi cw (p + 1), hexec]
      rfl
    have hfn1 : cw1.store.function? g.functionIndex = some fn := by
      unfold Store.function? at hfn ⊢
      rw [hgi, hfs]
      exact hfn
    have hrest := segment xs (p + 1) (P ++ [i]) R cw1 cw' evs' g fn
      (by simp [hp]) (fun j hj => hs j (by simp [hj])) hg hfn1
      (by rw [hb]; simp [List.append_assoc]) hr'
    have hcw : stepEvents i (cw.setActivePc p) = stepEvents i cw := stepEvents_setActivePc i cw p
    have hlen : p + (i :: xs).length = p + 1 + xs.length := by simp; omega
    rw [hev, ← hcw, hlen]
    exact MReach.step hcur hexs hrest

/-- Setting the program counter twice is setting it once, the second value winning. -/
theorem setActivePc_setActivePc (X : Configuration) (a b : Nat) :
    (X.setActivePc a).setActivePc b = X.setActivePc b := by
  rcases List.eq_nil_or_concat X.frames with hn | ⟨L, g, hL⟩
  · simp [Configuration.setActivePc, hn]
  · simp only [List.concat_eq_append] at hL
    rw [setActivePc_of_concat X L g a hL]
    rw [setActivePc_of_concat { X with frames := L ++ [{ g with pc := a }] } L { g with pc := a } b rfl]
    rw [setActivePc_of_concat X L g b hL]

/-- A control step, which the interpreter runs with no events, is a machine step. -/
theorem mreach_of_step {c c1 c2 : Configuration} {evs : List Event}
    (h : Step c (.step c1)) (hev : ∀ i, c.currentInstr? = some i → stepEvents i c = [])
    (hrest : MReach c1 c2 evs) : MReach c c2 evs := by
  cases h with
  | run fetched rule =>
    have hx := execOp_sound rule
    have he := hev _ fetched
    have := MReach.step fetched hx hrest
    rw [he, List.nil_append] at this
    exact this

/-- A supported body keeps the active frame's function index and the store's functions. -/
theorem runWasm_frames : ∀ (xs : List Instr) (c c' : Configuration) (evs : List Event),
    (∀ i ∈ xs, V4Diamond.Supported i) → runWasm xs c = some (c', evs) →
    c'.store.functions = c.store.functions ∧
      ∀ f, c.activeFrame? = some f → ∃ g, c'.activeFrame? = some g ∧ g.functionIndex = f.functionIndex
  | [], c, c', evs, _, h => by
    simp only [runWasm, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    exact ⟨rfl, fun f hf => ⟨f, hf, rfl⟩⟩
  | i :: xs, c, c', evs, hs, h => by
    obtain ⟨c1, evs', hexec, hr', -⟩ := runWasm_cons_step h
    obtain ⟨h1, h2⟩ := execOp_frames i (hs i (by simp)) c c1 hexec
    obtain ⟨h3, h4⟩ := runWasm_frames xs c1 c' evs' (fun j hj => hs j (by simp [hj])) hr'
    refine ⟨h3.trans h1, fun f hf => ?_⟩
    obtain ⟨g, hg, hgi⟩ := h2 f hf
    obtain ⟨g2, hg2, hgi2⟩ := h4 g hg
    exact ⟨g2, hg2, hgi2.trans hgi⟩

/-- A store with the same function table answers the same function lookups. -/
theorem function?_of_functions (s t : Store) (h : t.functions = s.functions) (idx : Nat) :
    t.function? idx = s.function? idx := by
  unfold Store.function?
  rw [h]

/-- The machine runs the diamond's zero-condition path: the pre piece, the `if` landing at the
else-body, the else arm, and `post`. -/
theorem machine_diamond_zero (d : Diamond.Shape) (c c1 c3 c4 : Configuration) (e1 e2 e3 : List Event)
    (f : Frame) (fn : FunctionState)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length d.elseArm.length)
    (hpre : ∀ i ∈ d.pre, V4Diamond.Supported i)
    (helse : ∀ i ∈ d.elseArm, V4Diamond.Supported i)
    (hpost : ∀ i ∈ d.post, V4Diamond.Supported i)
    (hpreR : runWasm d.pre c = some (c1, e1))
    (hop : c1.operandStack = [.i32 0])
    (harm : runWasm d.elseArm { c1 with operandStack := [] } = some (c3, e2))
    (_hemp : c3.operandStack = [])
    (hp4 : runWasm d.post c3 = some (c4, e3)) :
    ∃ cEnd, MReach (c.setActivePc 0) cEnd (e1 ++ e2 ++ e3) ∧ cEnd.currentInstr? = none ∧
      cEnd.operandStack = c4.operandStack := by
  have hbody0 : fn.body = [] ++ d.pre ++ ((Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      (Instr.elseInstr d.elseArm.length :: d.elseArm) ++ d.post) := by
    rw [hbody]
    simp [IfControl.layout, List.append_assoc]
  have hA := segment d.pre 0 [] _ c c1 e1 f fn (by simp) hpre hf hfn hbody0 hpreR
  simp only [Nat.zero_add] at hA
  obtain ⟨hfs1, hfr1⟩ := runWasm_frames d.pre c c1 e1 hpre hpreR
  obtain ⟨g, hg1, hgi⟩ := hfr1 f hf
  have hm1f : (c1.setActivePc d.pre.length).activeFrame? = some { g with pc := d.pre.length } := by
    rw [activeFrame_setActivePc, hg1]
    rfl
  have hm1fn : (c1.setActivePc d.pre.length).store.function?
      ({ g with pc := d.pre.length } : Frame).functionIndex = some fn := by
    rw [setActivePc_store]
    show c1.store.function? g.functionIndex = some fn
    rw [function?_of_functions c.store c1.store hfs1, hgi]
    exact hfn
  have hm1op : (c1.setActivePc d.pre.length).operandStack = [.i32 0] := by
    rw [setActivePc_operandStack']
    exact hop
  obtain ⟨cA, hcA_eq, hstepA, hA1, hA2, hA3, hA4⟩ :=
    IfControl.if_zero_lands (c1.setActivePc d.pre.length) { g with pc := d.pre.length } fn
      d.thenArm.length d.elseArm.length d.pre d.thenArm d.elseArm d.post [] rfl rfl hm1f hm1fn
      hbody rfl hm1op
  have hev : ∀ i, (c1.setActivePc d.pre.length).currentInstr? = some i →
      stepEvents i (c1.setActivePc d.pre.length) = [] := by
    intro i hi
    rw [currentInstr_of _ { g with pc := d.pre.length } fn _ hm1f hm1fn hbody] at hi
    have hi' : (IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length
        d.elseArm.length)[d.pre.length]? = some i := hi
    rw [IfControl.layout_if] at hi'
    obtain rfl := (Option.some.inj hi').symm
    rfl
  have hmA : MReach (c1.setActivePc d.pre.length) cA [] :=
    mreach_of_step hstepA hev (MReach.refl cA)
  have hq : d.pre.length + 1 + d.thenArm.length + 1 = d.pre.length + d.thenArm.length + 2 := by
    omega
  have hcA2 : cA = ({ c1 with operandStack := [] } : Configuration).setActivePc
      (d.pre.length + d.thenArm.length + 2) := by
    rw [hcA_eq]
    dsimp only
    rw [advance_setActivePc, setActivePc_setActivePc, hq, setActivePc_with_stack]
  have hg2 : ({ c1 with operandStack := [] } : Configuration).activeFrame? = some g := hg1
  have hfn2 : ({ c1 with operandStack := [] } : Configuration).store.function?
      g.functionIndex = some fn := by
    show c1.store.function? g.functionIndex = some fn
    rw [function?_of_functions c.store c1.store hfs1, hgi]
    exact hfn
  have hPlen : (d.pre ++ (Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      [Instr.elseInstr d.elseArm.length]).length = d.pre.length + d.thenArm.length + 2 := by
    simp
    omega
  have hbodyE : fn.body = (d.pre ++ (Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      [Instr.elseInstr d.elseArm.length]) ++ d.elseArm ++ d.post := by
    rw [hbody]
    simp [IfControl.layout, List.append_assoc]
  have hB := segment d.elseArm (d.pre.length + d.thenArm.length + 2) _ d.post
    ({ c1 with operandStack := [] } : Configuration) c3 e2 g fn hPlen helse hg2 hfn2 hbodyE harm
  rw [← hcA2] at hB
  obtain ⟨hfs3, hfr3⟩ := runWasm_frames d.elseArm { c1 with operandStack := [] } c3 e2 helse harm
  obtain ⟨g3, hg3, hgi3⟩ := hfr3 g hg2
  have hfn3 : c3.store.function? g3.functionIndex = some fn := by
    rw [function?_of_functions c.store c3.store (hfs3.trans hfs1), hgi3, hgi]
    exact hfn
  have hPlen2 : (d.pre ++ (Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      (Instr.elseInstr d.elseArm.length :: d.elseArm)).length =
      d.pre.length + d.thenArm.length + 2 + d.elseArm.length := by
    simp
    omega
  have hbodyP : fn.body = (d.pre ++ (Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      (Instr.elseInstr d.elseArm.length :: d.elseArm)) ++ d.post ++ [] := by
    rw [hbody]
    simp [IfControl.layout, List.append_assoc]
  have hC := segment d.post (d.pre.length + d.thenArm.length + 2 + d.elseArm.length) _ []
    c3 c4 e3 g3 fn hPlen2 hpost hg3 hfn3 hbodyP hp4
  obtain ⟨hfs4, hfr4⟩ := runWasm_frames d.post c3 c4 e3 hpost hp4
  obtain ⟨g4, hg4, hgi4⟩ := hfr4 g3 hg3
  have hend : (c4.setActivePc (d.pre.length + d.thenArm.length + 2 + d.elseArm.length +
      d.post.length)).currentInstr? = none := by
    rw [currentInstr_of _ { g4 with pc := d.pre.length + d.thenArm.length + 2 + d.elseArm.length +
      d.post.length } fn _ (by rw [activeFrame_setActivePc, hg4]; rfl)
      (by
        rw [setActivePc_store]
        show c4.store.function? g4.functionIndex = some fn
        rw [function?_of_functions c.store c4.store (hfs4.trans (hfs3.trans hfs1)), hgi4, hgi3, hgi]
        exact hfn) hbody]
    apply List.getElem?_eq_none
    simp [IfControl.layout]
    omega
  refine ⟨c4.setActivePc (d.pre.length + d.thenArm.length + 2 + d.elseArm.length + d.post.length),
    ?_, hend, setActivePc_operandStack' _ _⟩
  have := hA.trans (hmA.trans (hB.trans hC))
  simpa [List.append_assoc] using this

/-- The machine runs the diamond's non-zero path: the pre piece, the `if` falling into the
then-body, the then arm, the marker skipping the else-body, and `post`. -/
theorem machine_diamond_nonzero (d : Diamond.Shape) (c c1 c3 c4 : Configuration)
    (e1 e2 e3 : List Event) (f : Frame) (fn : FunctionState) (v : Int)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length d.elseArm.length)
    (hpre : ∀ i ∈ d.pre, V4Diamond.Supported i)
    (hthen : ∀ i ∈ d.thenArm, V4Diamond.Supported i)
    (hpost : ∀ i ∈ d.post, V4Diamond.Supported i)
    (hpreR : runWasm d.pre c = some (c1, e1))
    (hop : c1.operandStack = [.i32 v]) (hv : v ≠ 0)
    (harm : runWasm d.thenArm { c1 with operandStack := [] } = some (c3, e2))
    (_hemp : c3.operandStack = [])
    (hp4 : runWasm d.post c3 = some (c4, e3)) :
    ∃ cEnd, MReach (c.setActivePc 0) cEnd (e1 ++ e2 ++ e3) ∧ cEnd.currentInstr? = none ∧
      cEnd.operandStack = c4.operandStack := by
  have hbody0 : fn.body = [] ++ d.pre ++ ((Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      (Instr.elseInstr d.elseArm.length :: d.elseArm) ++ d.post) := by
    rw [hbody]
    simp [IfControl.layout, List.append_assoc]
  have hA := segment d.pre 0 [] _ c c1 e1 f fn (by simp) hpre hf hfn hbody0 hpreR
  simp only [Nat.zero_add] at hA
  obtain ⟨hfs1, hfr1⟩ := runWasm_frames d.pre c c1 e1 hpre hpreR
  obtain ⟨g, hg1, hgi⟩ := hfr1 f hf
  have hm1f : (c1.setActivePc d.pre.length).activeFrame? = some { g with pc := d.pre.length } := by
    rw [activeFrame_setActivePc, hg1]
    rfl
  have hm1fn : (c1.setActivePc d.pre.length).store.function?
      ({ g with pc := d.pre.length } : Frame).functionIndex = some fn := by
    rw [setActivePc_store]
    show c1.store.function? g.functionIndex = some fn
    rw [function?_of_functions c.store c1.store hfs1, hgi]
    exact hfn
  have hm1op : (c1.setActivePc d.pre.length).operandStack = [.i32 v] := by
    rw [setActivePc_operandStack']
    exact hop
  obtain ⟨cA, hcA_eq, hstepA, hA1, hA2, hA3, hA4⟩ :=
    IfControl.if_nonzero_falls (c1.setActivePc d.pre.length) { g with pc := d.pre.length } fn
      d.pre d.thenArm d.elseArm d.post [] v hm1f hm1fn hbody rfl hm1op hv
  have hev : ∀ i, (c1.setActivePc d.pre.length).currentInstr? = some i →
      stepEvents i (c1.setActivePc d.pre.length) = [] := by
    intro i hi
    rw [currentInstr_of _ { g with pc := d.pre.length } fn _ hm1f hm1fn hbody] at hi
    have hi' : (IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length
        d.elseArm.length)[d.pre.length]? = some i := hi
    rw [IfControl.layout_if] at hi'
    obtain rfl := (Option.some.inj hi').symm
    rfl
  have hmA : MReach (c1.setActivePc d.pre.length) cA [] :=
    mreach_of_step hstepA hev (MReach.refl cA)
  have hcA2 : cA = ({ c1 with operandStack := [] } : Configuration).setActivePc
      (d.pre.length + 1) := by
    rw [hcA_eq]
    dsimp only
    rw [advance_setActivePc, setActivePc_with_stack]
  have hg2 : ({ c1 with operandStack := [] } : Configuration).activeFrame? = some g := hg1
  have hfn2 : ({ c1 with operandStack := [] } : Configuration).store.function?
      g.functionIndex = some fn := by
    show c1.store.function? g.functionIndex = some fn
    rw [function?_of_functions c.store c1.store hfs1, hgi]
    exact hfn
  have hPlen : (d.pre ++ [Instr.ifInstr d.thenArm.length]).length = d.pre.length + 1 := by
    simp
  have hbodyT : fn.body = (d.pre ++ [Instr.ifInstr d.thenArm.length]) ++ d.thenArm ++
      (Instr.elseInstr d.elseArm.length :: d.elseArm ++ d.post) := by
    rw [hbody]
    simp [IfControl.layout, List.append_assoc]
  have hB := segment d.thenArm (d.pre.length + 1) _
    (Instr.elseInstr d.elseArm.length :: d.elseArm ++ d.post)
    ({ c1 with operandStack := [] } : Configuration) c3 e2 g fn hPlen hthen hg2 hfn2 hbodyT harm
  rw [← hcA2] at hB
  obtain ⟨hfs3, hfr3⟩ := runWasm_frames d.thenArm { c1 with operandStack := [] } c3 e2 hthen harm
  obtain ⟨g3, hg3, hgi3⟩ := hfr3 g hg2
  have hfn3 : c3.store.function? g3.functionIndex = some fn := by
    rw [function?_of_functions c.store c3.store (hfs3.trans hfs1), hgi3, hgi]
    exact hfn
  have hmpc : d.pre.length + 1 + d.thenArm.length = d.pre.length + d.thenArm.length + 1 := by
    omega
  rw [hmpc] at hB
  have hfr3' : (c3.setActivePc (d.pre.length + d.thenArm.length + 1)).activeFrame? =
      some { g3 with pc := d.pre.length + d.thenArm.length + 1 } := by
    rw [activeFrame_setActivePc, hg3]
    rfl
  have hfn3' : (c3.setActivePc (d.pre.length + d.thenArm.length + 1)).store.function?
      ({ g3 with pc := d.pre.length + d.thenArm.length + 1 } : Frame).functionIndex = some fn := by
    rw [setActivePc_store]
    show c3.store.function? g3.functionIndex = some fn
    exact hfn3
  obtain ⟨cB, hcB_eq, hstepB, hB1, hB2, hB3⟩ :=
    IfControl.else_marker_skips (c3.setActivePc (d.pre.length + d.thenArm.length + 1))
      { g3 with pc := d.pre.length + d.thenArm.length + 1 } fn d.pre d.thenArm d.elseArm d.post
      hfr3' hfn3' hbody rfl
  have hevB : ∀ i, (c3.setActivePc (d.pre.length + d.thenArm.length + 1)).currentInstr? = some i →
      stepEvents i (c3.setActivePc (d.pre.length + d.thenArm.length + 1)) = [] := by
    intro i hi
    rw [currentInstr_of _ { g3 with pc := d.pre.length + d.thenArm.length + 1 } fn _ hfr3' hfn3' hbody]
      at hi
    have hi' : (IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length
        d.elseArm.length)[d.pre.length + d.thenArm.length + 1]? = some i := hi
    rw [IfControl.layout_marker _ _ _ _ _ _ rfl] at hi'
    obtain rfl := (Option.some.inj hi').symm
    rfl
  have hmB : MReach (c3.setActivePc (d.pre.length + d.thenArm.length + 1)) cB [] :=
    mreach_of_step hstepB hevB (MReach.refl cB)
  have hcB2 : cB = c3.setActivePc (d.pre.length + d.thenArm.length + 2 + d.elseArm.length) := by
    rw [hcB_eq]
    dsimp only
    rw [advance_setActivePc, setActivePc_setActivePc]
  rw [hcB2] at hmB
  have hPlen2 : (d.pre ++ (Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      (Instr.elseInstr d.elseArm.length :: d.elseArm)).length =
      d.pre.length + d.thenArm.length + 2 + d.elseArm.length := by
    simp
    omega
  have hbodyP : fn.body = (d.pre ++ (Instr.ifInstr d.thenArm.length :: d.thenArm) ++
      (Instr.elseInstr d.elseArm.length :: d.elseArm)) ++ d.post ++ [] := by
    rw [hbody]
    simp [IfControl.layout, List.append_assoc]
  have hC := segment d.post (d.pre.length + d.thenArm.length + 2 + d.elseArm.length) _ []
    c3 c4 e3 g3 fn hPlen2 hpost hg3 hfn3 hbodyP hp4
  obtain ⟨hfs4, hfr4⟩ := runWasm_frames d.post c3 c4 e3 hpost hp4
  obtain ⟨g4, hg4, hgi4⟩ := hfr4 g3 hg3
  have hend : (c4.setActivePc (d.pre.length + d.thenArm.length + 2 + d.elseArm.length +
      d.post.length)).currentInstr? = none := by
    rw [currentInstr_of _ { g4 with pc := d.pre.length + d.thenArm.length + 2 + d.elseArm.length +
      d.post.length } fn _ (by rw [activeFrame_setActivePc, hg4]; rfl)
      (by
        rw [setActivePc_store]
        show c4.store.function? g4.functionIndex = some fn
        rw [function?_of_functions c.store c4.store (hfs4.trans (hfs3.trans hfs1)), hgi4, hgi3, hgi]
        exact hfn) hbody]
    apply List.getElem?_eq_none
    simp [IfControl.layout]
    omega
  refine ⟨c4.setActivePc (d.pre.length + d.thenArm.length + 2 + d.elseArm.length + d.post.length),
    ?_, hend, setActivePc_operandStack' _ _⟩
  have := hA.trans (hmA.trans (hB.trans (hmB.trans hC)))
  simpa [List.append_assoc] using this

/-- The machine runs a diamond's flat body the way `runShape` runs the diamond.

If the flat body of the function is the diamond's layout, its instructions are straight-line or
control instructions, and `runShape` returns `c'` with events `evs`, then the machine, started at
the entry of the body, reaches the end of the body with the same events and the same operand
stack that `c'` ends with. -/
theorem machine_diamond (d : Diamond.Shape) (c c' : Configuration) (evs : List Event)
    (f : Frame) (fn : FunctionState)
    (hf : c.activeFrame? = some f) (hfn : c.store.function? f.functionIndex = some fn)
    (hbody : fn.body = IfControl.layout d.pre d.thenArm d.elseArm d.post d.thenArm.length
      d.elseArm.length)
    (hpre : ∀ i ∈ d.pre, V4Diamond.Supported i)
    (hthen : ∀ i ∈ d.thenArm, V4Diamond.Supported i)
    (helse : ∀ i ∈ d.elseArm, V4Diamond.Supported i)
    (hpost : ∀ i ∈ d.post, V4Diamond.Supported i)
    (hrun : Diamond.runShape d c = some (c', evs)) :
    ∃ cEnd, MReach (c.setActivePc 0) cEnd evs ∧ cEnd.currentInstr? = none ∧
      cEnd.operandStack = c'.operandStack := by
  unfold Diamond.runShape at hrun
  cases hpreR : runWasm d.pre c with
  | none => simp [hpreR] at hrun
  | some p =>
    obtain ⟨c1, e1⟩ := p
    simp only [hpreR] at hrun
    split at hrun
    next v hop =>
      by_cases hv0 : v = 0
      · subst hv0
        simp only [↓reduceIte] at hrun
        cases harm : runWasm d.elseArm { c1 with operandStack := [] } with
        | none => simp [harm] at hrun
        | some p2 =>
          obtain ⟨c3, e2⟩ := p2
          simp only [harm] at hrun
          split at hrun
          next hemp =>
            cases hp4 : runWasm d.post c3 with
            | none => simp [hp4] at hrun
            | some p3 =>
              obtain ⟨c4, e3⟩ := p3
              simp only [hp4, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hrun
              obtain ⟨rfl, rfl⟩ := hrun
              exact machine_diamond_zero d c c1 c3 c4 e1 e2 e3
                f fn hf hfn hbody hpre helse hpost hpreR hop harm hemp hp4
          next => simp at hrun
      · simp only [hv0, ↓reduceIte] at hrun
        cases harm : runWasm d.thenArm { c1 with operandStack := [] } with
        | none => simp [harm] at hrun
        | some p2 =>
          obtain ⟨c3, e2⟩ := p2
          simp only [harm] at hrun
          split at hrun
          next hemp =>
            cases hp4 : runWasm d.post c3 with
            | none => simp [hp4] at hrun
            | some p3 =>
              obtain ⟨c4, e3⟩ := p3
              simp only [hp4, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hrun
              obtain ⟨rfl, rfl⟩ := hrun
              exact machine_diamond_nonzero d c c1 c3 c4 e1 e2 e3
                f fn v hf hfn hbody hpre hthen hpost hpreR hop hv0 harm hemp hp4
          next => simp at hrun
    next => simp at hrun

end MachineDiamond
end TptWasm
