import TptWasm.Nested.Frames
import TptWasm.Nested.LoopSteps

/-!
# The machine runs nested statements the way the list semantics does

`machine_stmt` and `machine_seq` say: a statement (or list) run by `runStmt` (or `runSeq`) from a
configuration is run by the machine from the pc where its flat code starts, with the same events,
and the machine ends at the pc where the flat code ends. The induction is over the statement
structure, so a nested `if` is handled by the same argument as the outer one, with its arm's
recursive run in the middle.

The three step lemmas are the pieces of an `if`: the zero path, the non-zero path, and the marker.
Each is `IfControl`'s landing, with the machine's start configuration and end configuration named.
-/

namespace TptWasm
namespace Nested

open V4Straight MachineDiamond IfControl

/-- The zero path of an `if`, from the pc of the `if`: the machine lands at the start of the
else-body, with the condition consumed and the stack empty. -/
theorem if_zero_machine (c1 : Configuration) (g : Frame) (fn : FunctionState)
    (pre thenBody elseBody post : List Instr)
    (hf1 : c1.activeFrame? = some g) (hfn1 : c1.store.function? g.functionIndex = some fn)
    (hbody : fn.body = IfControl.layout pre thenBody elseBody post thenBody.length elseBody.length)
    (hop : c1.operandStack = [.i32 0]) :
    MReach (c1.setActivePc pre.length)
      ({ c1 with operandStack := [] }.setActivePc (pre.length + thenBody.length + 2)) [] := by
  have hm1f : (c1.setActivePc pre.length).activeFrame? = some { g with pc := pre.length } := by
    rw [activeFrame_setActivePc, hf1]
    rfl
  have hm1fn : (c1.setActivePc pre.length).store.function?
      ({ g with pc := pre.length } : Frame).functionIndex = some fn := by
    rw [setActivePc_store]
    exact hfn1
  have hm1op : (c1.setActivePc pre.length).operandStack = [.i32 0] := by
    rw [setActivePc_operandStack']
    exact hop
  have hIf := IfControl.if_zero_lands (c1.setActivePc pre.length) { g with pc := pre.length } fn
    thenBody.length elseBody.length pre thenBody elseBody post [] rfl rfl hm1f hm1fn hbody rfl hm1op
  obtain ⟨cA, hcA_eq, hstepA, hA1, hA2, hA3, hA4⟩ := hIf
  have hev : ∀ i, (c1.setActivePc pre.length).currentInstr? = some i →
      stepEvents i (c1.setActivePc pre.length) = [] := by
    intro i hi
    rw [currentInstr_of _ { g with pc := pre.length } fn _ hm1f hm1fn hbody] at hi
    have hi' : (IfControl.layout pre thenBody elseBody post thenBody.length
        elseBody.length)[pre.length]? = some i := hi
    rw [IfControl.layout_if] at hi'
    obtain rfl := (Option.some.inj hi').symm
    rfl
  have hmA : MReach (c1.setActivePc pre.length) cA [] :=
    mreach_of_step hstepA hev (MReach.refl cA)
  have hq : pre.length + 1 + thenBody.length + 1 = pre.length + thenBody.length + 2 := by omega
  have hcA2 : cA = ({ c1 with operandStack := [] } : Configuration).setActivePc
      (pre.length + thenBody.length + 2) := by
    rw [hcA_eq]
    dsimp only
    rw [advance_setActivePc, setActivePc_setActivePc, hq, setActivePc_with_stack]
  rw [← hcA2]
  exact hmA

/-- The non-zero path of an `if`: the machine lands at the start of the then-body. -/
theorem if_nonzero_machine (c1 : Configuration) (g : Frame) (fn : FunctionState)
    (pre thenBody elseBody post : List Instr) (v : Int)
    (hf1 : c1.activeFrame? = some g) (hfn1 : c1.store.function? g.functionIndex = some fn)
    (hbody : fn.body = IfControl.layout pre thenBody elseBody post thenBody.length elseBody.length)
    (hop : c1.operandStack = [.i32 v]) (hv : v ≠ 0) :
    MReach (c1.setActivePc pre.length) ({ c1 with operandStack := [] }.setActivePc (pre.length + 1))
      [] := by
  have hm1f : (c1.setActivePc pre.length).activeFrame? = some { g with pc := pre.length } := by
    rw [activeFrame_setActivePc, hf1]
    rfl
  have hm1fn : (c1.setActivePc pre.length).store.function?
      ({ g with pc := pre.length } : Frame).functionIndex = some fn := by
    rw [setActivePc_store]
    exact hfn1
  have hm1op : (c1.setActivePc pre.length).operandStack = [.i32 v] := by
    rw [setActivePc_operandStack']
    exact hop
  obtain ⟨cA, hcA_eq, hstepA, hA1, hA2, hA3, hA4⟩ :=
    IfControl.if_nonzero_falls (c1.setActivePc pre.length) { g with pc := pre.length } fn
      pre thenBody elseBody post [] v hm1f hm1fn hbody rfl hm1op hv
  have hev : ∀ i, (c1.setActivePc pre.length).currentInstr? = some i →
      stepEvents i (c1.setActivePc pre.length) = [] := by
    intro i hi
    rw [currentInstr_of _ { g with pc := pre.length } fn _ hm1f hm1fn hbody] at hi
    have hi' : (IfControl.layout pre thenBody elseBody post thenBody.length
        elseBody.length)[pre.length]? = some i := hi
    rw [IfControl.layout_if] at hi'
    obtain rfl := (Option.some.inj hi').symm
    rfl
  have hmA : MReach (c1.setActivePc pre.length) cA [] :=
    mreach_of_step hstepA hev (MReach.refl cA)
  have hcA2 : cA = ({ c1 with operandStack := [] } : Configuration).setActivePc (pre.length + 1) := by
    rw [hcA_eq]
    dsimp only
    rw [advance_setActivePc, setActivePc_with_stack]
  rw [← hcA2]
  exact hmA

/-- The marker at the end of a then-body: the machine skips the else-body, to the end of the
statement's flat code. -/
theorem marker_machine (c3 : Configuration) (g3 : Frame) (fn : FunctionState)
    (pre thenBody elseBody post : List Instr)
    (hf3 : c3.activeFrame? = some g3) (hfn3 : c3.store.function? g3.functionIndex = some fn)
    (hbody : fn.body = IfControl.layout pre thenBody elseBody post thenBody.length elseBody.length) :
    MReach (c3.setActivePc (pre.length + thenBody.length + 1))
      (c3.setActivePc (pre.length + thenBody.length + 2 + elseBody.length)) [] := by
  have hfr3' : (c3.setActivePc (pre.length + thenBody.length + 1)).activeFrame? =
      some { g3 with pc := pre.length + thenBody.length + 1 } := by
    rw [activeFrame_setActivePc, hf3]
    rfl
  have hfn3' : (c3.setActivePc (pre.length + thenBody.length + 1)).store.function?
      ({ g3 with pc := pre.length + thenBody.length + 1 } : Frame).functionIndex = some fn := by
    rw [setActivePc_store]
    exact hfn3
  obtain ⟨cB, hcB_eq, hstepB, hB1, hB2, hB3⟩ :=
    IfControl.else_marker_skips (c3.setActivePc (pre.length + thenBody.length + 1))
      { g3 with pc := pre.length + thenBody.length + 1 } fn pre thenBody elseBody post
      hfr3' hfn3' hbody rfl
  have hevB : ∀ i, (c3.setActivePc (pre.length + thenBody.length + 1)).currentInstr? = some i →
      stepEvents i (c3.setActivePc (pre.length + thenBody.length + 1)) = [] := by
    intro i hi
    rw [currentInstr_of _ { g3 with pc := pre.length + thenBody.length + 1 } fn _ hfr3' hfn3' hbody]
      at hi
    have hi' : (IfControl.layout pre thenBody elseBody post thenBody.length
        elseBody.length)[pre.length + thenBody.length + 1]? = some i := hi
    rw [IfControl.layout_marker _ _ _ _ _ _ rfl] at hi'
    obtain rfl := (Option.some.inj hi').symm
    rfl
  have hmB : MReach (c3.setActivePc (pre.length + thenBody.length + 1)) cB [] :=
    mreach_of_step hstepB hevB (MReach.refl cB)
  have hcB2 : cB = c3.setActivePc (pre.length + thenBody.length + 2 + elseBody.length) := by
    rw [hcB_eq]
    dsimp only
    rw [advance_setActivePc, setActivePc_setActivePc]
  rw [← hcB2]
  exact hmB

mutual

/-- A statement, run by the list semantics, is run by the machine over its flat code. -/
theorem machine_stmt : ∀ (k : Nat) (s : Stmt) (p : Nat) (P R : List Instr) (cw cw' : Configuration)
    (evs : List Event) (f : Frame) (fn : FunctionState),
    P.length = p → SupStmt s → cw.activeFrame? = some f →
    cw.store.function? f.functionIndex = some fn → fn.body = P ++ flatStmt s ++ R →
    runStmt k s cw = some (cw', evs) →
    MReach (cw.setActivePc p) (cw'.setActivePc (p + (flatStmt s).length)) evs
  | k, .straight ops, p, P, R, cw, cw', evs, f, fn, hp, hs, hf, hfn, hbody, hr => by
    unfold runStmt at hr
    cases hw : runWasm ops cw with
    | none => simp [hw] at hr
    | some q =>
      obtain ⟨c1, e1⟩ := q
      simp only [hw] at hr
      split at hr
      · simp only [Option.some.injEq, Prod.mk.injEq] at hr
        obtain ⟨rfl, rfl⟩ := hr
        have hseg := segment ops p P R cw c1 e1 f fn hp hs hf hfn
          (by rw [hbody]; simp [flatStmt]) hw
        simpa [flatStmt] using hseg
      · simp at hr
  | k, .ifElse cond t el, p, P, R, cw, cw', evs, f, fn, hp, hs, hf, hfn, hbody, hr => by
    obtain ⟨hsc, hst, hse⟩ := hs
    unfold runStmt at hr
    by_cases hstk : cw.operandStack = []
    · simp only [hstk, ↓reduceIte] at hr
      cases hcond : runWasm cond cw with
      | none => simp [hcond] at hr
      | some q =>
        obtain ⟨c1, e1⟩ := q
        simp only [hcond] at hr
        split at hr
        next v hop =>
          cases harm : (if v = 0 then runSeq k el { c1 with operandStack := [] }
              else runSeq k t { c1 with operandStack := [] }) with
          | none => simp [harm] at hr
          | some q2 =>
            obtain ⟨c3, e2⟩ := q2
            simp only [harm] at hr
            split at hr
            next hemp =>
              simp only [Option.some.injEq, Prod.mk.injEq] at hr
              obtain ⟨rfl, rfl⟩ := hr
              have hcondM := segment cond p P ((Instr.ifInstr (flatSeq t).length :: flatSeq t) ++
                (Instr.elseInstr (flatSeq el).length :: flatSeq el) ++ R) cw c1 e1 f fn hp hsc hf hfn
                (by rw [hbody]; simp [flatStmt, List.append_assoc]) hcond
              have hpre : (P ++ cond).length = p + cond.length := by simp [hp]
              rw [← hpre] at hcondM
              obtain ⟨hfs1, hfr1⟩ := runWasm_frames cond cw c1 e1 hsc hcond
              obtain ⟨g, hg1, hgi⟩ := hfr1 f hf
              have hfn1 : c1.store.function? g.functionIndex = some fn := by
                rw [function?_of_functions cw.store c1.store hfs1, hgi]
                exact hfn
              have hbodyL : fn.body = IfControl.layout (P ++ cond) (flatSeq t) (flatSeq el) R
                  (flatSeq t).length (flatSeq el).length := by
                rw [hbody]
                simp [IfControl.layout, flatStmt, List.append_assoc]
              have hg2 : ({ c1 with operandStack := [] } : Configuration).activeFrame? =
                  some g := hg1
              have hfn2 : ({ c1 with operandStack := [] } : Configuration).store.function?
                  g.functionIndex = some fn := by
                show c1.store.function? g.functionIndex = some fn
                exact hfn1
              by_cases hv : v = 0
              · subst hv
                simp only [↓reduceIte] at harm
                have hop0 : c1.operandStack = [.i32 0] := hop
                have B := if_zero_machine c1 g fn (P ++ cond) (flatSeq t) (flatSeq el) R hg1 hfn1
                  hbodyL hop0
                have C := machine_seq k el ((P ++ cond).length + (flatSeq t).length + 2)
                  ((P ++ cond) ++ [Instr.ifInstr (flatSeq t).length] ++ flatSeq t ++
                    [Instr.elseInstr (flatSeq el).length]) R
                  ({ c1 with operandStack := [] }) c3 e2 g fn
                  (by simp; omega) hse hg2 hfn2
                  (by rw [hbody]; simp [flatStmt, List.append_assoc]) harm
                have heq : p + (flatStmt (.ifElse cond t el)).length =
                    (P ++ cond).length + (flatSeq t).length + 2 + (flatSeq el).length := by
                  simp [flatStmt]
                  omega
                rw [heq]
                have := MReach.trans hcondM (MReach.trans B C)
                simpa using this
              · simp only [hv, ↓reduceIte] at harm
                have hop1 : c1.operandStack = [.i32 v] := hop
                have B := if_nonzero_machine c1 g fn (P ++ cond) (flatSeq t) (flatSeq el) R v hg1
                  hfn1 hbodyL hop1 hv
                have C := machine_seq k t ((P ++ cond).length + 1)
                  ((P ++ cond) ++ [Instr.ifInstr (flatSeq t).length])
                  (Instr.elseInstr (flatSeq el).length :: flatSeq el ++ R)
                  ({ c1 with operandStack := [] }) c3 e2 g fn
                  (by simp; omega) hst hg2 hfn2
                  (by rw [hbody]; simp [flatStmt, List.append_assoc]) harm
                obtain ⟨hfs3, hfr3⟩ :=
                  runSeq_frames k t { c1 with operandStack := [] } c3 e2 hst harm
                obtain ⟨g3, hg3, hgi3⟩ := hfr3 g hg2
                have hfn3 : c3.store.function? g3.functionIndex = some fn := by
                  rw [function?_of_functions cw.store c3.store (hfs3.trans hfs1), hgi3, hgi]
                  exact hfn
                have D := marker_machine c3 g3 fn (P ++ cond) (flatSeq t) (flatSeq el) R hg3 hfn3
                  hbodyL
                have e1' : (P ++ cond).length + 1 + (flatSeq t).length =
                    (P ++ cond).length + (flatSeq t).length + 1 := by omega
                rw [e1'] at C
                have heq : p + (flatStmt (.ifElse cond t el)).length =
                    (P ++ cond).length + (flatSeq t).length + 2 + (flatSeq el).length := by
                  simp [flatStmt]
                  omega
                rw [heq]
                have := MReach.trans hcondM (MReach.trans B (MReach.trans C D))
                simpa using this
            next => simp at hr
        next => simp at hr
    · simp [hstk] at hr
  | 0, .whileNZ _ _, p, P, R, cw, cw', evs, f, fn, hp, _, _, _, _, hr => by
    simp [runStmt] at hr
  | k + 1, .whileNZ cond body, p, P, R, cw, cw', evs, f, fn, hp, hs, hf, hfn, hbody, hr => by
    have hs0 := hs
    obtain ⟨hsc, hsb⟩ := hs
    unfold runStmt at hr
    by_cases hstk : cw.operandStack = []
    · simp only [hstk, ↓reduceIte] at hr
      cases hcond : runWasm cond cw with
      | none => simp [hcond] at hr
      | some q =>
        obtain ⟨c1, e1⟩ := q
        simp only [hcond] at hr
        split at hr
        next v hop =>
          -- the condition runs, from the loop's start
          have hcondM := segment cond p P ((Instr.ifInstr (flatSeq body).length :: flatSeq body) ++
            (Instr.loopBack (cond.length + 1 + (flatSeq body).length) :: R)) cw c1 e1 f fn hp hsc hf
            hfn (by rw [hbody]; simp [flatStmt, List.append_assoc]) hcond
          obtain ⟨hfs1, hfr1⟩ := runWasm_frames cond cw c1 e1 hsc hcond
          obtain ⟨g, hg1, hgi⟩ := hfr1 f hf
          have hfn1 : c1.store.function? g.functionIndex = some fn := by
            rw [function?_of_functions cw.store c1.store hfs1, hgi]
            exact hfn
          -- the `if` on the condition's value, at the position after the condition
          have hX : (c1.setActivePc (p + cond.length)).activeFrame? =
              some { g with pc := p + cond.length } := by
            rw [activeFrame_setActivePc, hg1]
            rfl
          have hXfn : (c1.setActivePc (p + cond.length)).store.function?
              ({ g with pc := p + cond.length } : Frame).functionIndex = some fn := by
            rw [setActivePc_store]
            exact hfn1
          have hXop : (c1.setActivePc (p + cond.length)).operandStack = [.i32 v] := by
            rw [setActivePc_operandStack']
            exact hop
          have hbodyL : fn.body = (P ++ cond) ++
              (Instr.ifInstr (flatSeq body).length :: flatSeq body) ++
              (Instr.loopBack (cond.length + 1 + (flatSeq body).length) :: R) := by
            rw [hbody]
            simp [flatStmt, List.append_assoc]
          have hpcL : ({ g with pc := p + cond.length } : Frame).pc = (P ++ cond).length := by
            simp [hp]
          have hcst : ({ c1 with operandStack := [] } : Configuration).activeFrame? =
              some g := hg1
          have hcstfn : ({ c1 with operandStack := [] } : Configuration).store.function?
              g.functionIndex = some fn := hfn1
          by_cases hv : v = 0
          · -- the condition is zero: the loop ends, past its back-edge
            subst hv
            simp only [↓reduceIte, Option.some.injEq, Prod.mk.injEq] at hr
            obtain ⟨hc2, hev⟩ := hr
            subst hev
            subst hc2
            have Z := loop_zero_step (c1.setActivePc (p + cond.length)) { g with pc := p + cond.length }
              fn (P ++ cond) (flatSeq body) R (cond.length + 1 + (flatSeq body).length) []
              hX hXfn hbodyL hpcL (by rw [hXop])
            rw [exit_eq] at Z
            have hpos : ({ g with pc := p + cond.length } : Frame).pc + 1 +
                (flatSeq body).length + 1 = p + (flatStmt (.whileNZ cond body)).length := by
              simp [flatStmt]
              omega
            rw [hpos] at Z
            have T := MReach.trans hcondM Z
            simpa using T
          · -- the condition is non-zero: the body runs, then the back-edge returns to the start
            simp only [hv, ↓reduceIte] at hr
            have N := loop_nonzero_step (c1.setActivePc (p + cond.length))
              { g with pc := p + cond.length } fn (P ++ cond) (flatSeq body) R
              (cond.length + 1 + (flatSeq body).length) v [] hX hXfn hbodyL hpcL (by rw [hXop]) hv
            rw [nonzero_eq] at N
            cases harm : runSeq k body { c1 with operandStack := [] } with
            | none => simp [harm] at hr
            | some q2 =>
              obtain ⟨c3, e2⟩ := q2
              simp only [harm] at hr
              cases hrec : runStmt k (.whileNZ cond body) c3 with
              | none => simp [hrec] at hr
              | some q4 =>
                obtain ⟨c4, e3⟩ := q4
                simp only [hrec, Option.some.injEq, Prod.mk.injEq] at hr
                obtain ⟨hc4, hev⟩ := hr
                subst hev
                subst hc4
                -- the body, from the position after the `if`
                have hp' : (P ++ cond ++ [Instr.ifInstr (flatSeq body).length]).length =
                    p + cond.length + 1 := by
                  simp [hp] <;> omega
                have hbody' : fn.body = (P ++ cond ++ [Instr.ifInstr (flatSeq body).length]) ++
                    flatSeq body ++ (Instr.loopBack (cond.length + 1 + (flatSeq body).length) :: R) := by
                  rw [hbody]
                  simp [flatStmt, List.append_assoc]
                have Bd := machine_seq k body (p + cond.length + 1)
                  (P ++ cond ++ [Instr.ifInstr (flatSeq body).length])
                  (Instr.loopBack (cond.length + 1 + (flatSeq body).length) :: R)
                  ({ c1 with operandStack := [] } : Configuration) c3 e2 g fn hp' hsb hcst hcstfn
                  hbody' harm
                obtain ⟨hf3, hr3⟩ :=
                  runSeq_frames k body { c1 with operandStack := [] } c3 e2 hsb harm
                obtain ⟨g3, hg3, hgi3⟩ := hr3 g hcst
                have hfn3 : c3.store.function? g3.functionIndex = some fn := by
                  rw [function?_of_functions cw.store c3.store (hf3.trans hfs1), hgi3, hgi]
                  exact hfn
                -- the back-edge, from the end of the body to the condition
                have hX3 : (c3.setActivePc (p + cond.length + 1 + (flatSeq body).length)).activeFrame? =
                    some { g3 with pc := p + cond.length + 1 + (flatSeq body).length } := by
                  rw [activeFrame_setActivePc, hg3]
                  rfl
                have hX3fn : (c3.setActivePc (p + cond.length + 1 + (flatSeq body).length)).store.function?
                    ({ g3 with pc := p + cond.length + 1 + (flatSeq body).length } : Frame).functionIndex
                      = some fn := by
                  rw [setActivePc_store]
                  exact hfn3
                have hbodyB : fn.body = (P ++ cond ++ [Instr.ifInstr (flatSeq body).length] ++
                    flatSeq body) ++ (Instr.loopBack (cond.length + 1 + (flatSeq body).length) :: R) := by
                  rw [hbody]
                  simp [flatStmt, List.append_assoc]
                have Bk := loop_back_step (c3.setActivePc (p + cond.length + 1 + (flatSeq body).length))
                  { g3 with pc := p + cond.length + 1 + (flatSeq body).length } fn
                  (P ++ cond ++ [Instr.ifInstr (flatSeq body).length] ++ flatSeq body) R
                  (cond.length + 1 + (flatSeq body).length) p hX3 hX3fn hbodyB
                  (by simp [hp] <;> omega) (by simp [hp] <;> omega)
                rw [setActivePc_setActivePc] at Bk
                -- the rest of the loop, from the start again
                have IH := machine_stmt k (.whileNZ cond body) p P R c3 c4 e3 g3 fn hp hs0 hg3 hfn3
                  hbody hrec
                have T := MReach.trans hcondM (MReach.trans N (MReach.trans Bd (MReach.trans Bk IH)))
                simpa [List.append_assoc] using T
        next => simp at hr
    · simp [hstk] at hr

/-- A statement list, run by the list semantics, is run by the machine over its flat code. -/
theorem machine_seq : ∀ (k : Nat) (ss : List Stmt) (p : Nat) (P R : List Instr) (cw cw' : Configuration)
    (evs : List Event) (f : Frame) (fn : FunctionState),
    P.length = p → SupSeq ss → cw.activeFrame? = some f →
    cw.store.function? f.functionIndex = some fn → fn.body = P ++ flatSeq ss ++ R →
    runSeq k ss cw = some (cw', evs) →
    MReach (cw.setActivePc p) (cw'.setActivePc (p + (flatSeq ss).length)) evs
  | _, [], p, P, R, cw, cw', evs, f, fn, hp, _, hf, hfn, hbody, hr => by
    simp only [runSeq, Option.some.injEq, Prod.mk.injEq] at hr
    obtain ⟨rfl, rfl⟩ := hr
    simpa [flatSeq] using MReach.refl _
  | k, s :: rest, p, P, R, cw, cw', evs, f, fn, hp, hs, hf, hfn, hbody, hr => by
    obtain ⟨hs1, hs2⟩ := hs
    unfold runSeq at hr
    cases h1 : runStmt k s cw with
    | none => simp [h1] at hr
    | some q =>
      obtain ⟨c1, e1⟩ := q
      simp only [h1] at hr
      cases h2 : runSeq k rest c1 with
      | none => simp [h2] at hr
      | some q2 =>
        obtain ⟨c2, e2⟩ := q2
        simp only [h2, Option.some.injEq, Prod.mk.injEq] at hr
        obtain ⟨rfl, rfl⟩ := hr
        obtain ⟨f1, r1⟩ := runStmt_frames k s cw c1 e1 hs1 h1
        obtain ⟨g, hg, hgi⟩ := r1 f hf
        have hfn1 : c1.store.function? g.functionIndex = some fn := by
          rw [function?_of_functions cw.store c1.store f1, hgi]
          exact hfn
        have A := machine_stmt k s p P (flatSeq rest ++ R) cw c1 e1 f fn hp hs1 hf hfn
          (by rw [hbody]; simp [flatSeq, List.append_assoc]) h1
        have B := machine_seq k rest (p + (flatStmt s).length) (P ++ flatStmt s) R c1 c2 e2 g fn
          (by simp [hp]) hs2 hg hfn1 (by rw [hbody]; simp [flatSeq, List.append_assoc]) h2
        have heq : p + (flatSeq (s :: rest)).length =
            p + (flatStmt s).length + (flatSeq rest).length := by
          simp [flatSeq]
          omega
        rw [heq]
        exact MReach.trans A B

end

end Nested
end TptWasm
