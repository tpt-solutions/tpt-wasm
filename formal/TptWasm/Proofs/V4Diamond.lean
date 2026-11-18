import TptWasm.Diamond

/-!
# V4, control flow: one `if`/`else` diamond

`TptWasm.Diamond` defines a Wasm body of the shape `pre ; if ... else ... end ; post`, its meaning
`runShape`, and its lowering to a four-block IR function `lowerShape`. This file proves that the
lowered function computes what the Wasm body computes.

## What is proved

`diamond_simulation`: if the lowering succeeds, the Wasm configuration and the IR state correspond,
the globals are `i32`-typed, and the Wasm run returns `c'` with events `evs`, then the IR function
run from the entry with fuel `k + 3` returns exactly `evs` and the values `c'.operandStack`.

The proof reuses `V4Straight.simulation` for each of the four pieces. What is new here is the
binding of block parameters at each edge (`bindAll`), the fuel accounting of the block graph, and
the decision of the `condBranch`.

## What is not proved

Traps, and the store and locals after the run. The conclusion is about the returned events and
values, which is the observable behaviour of a function; the store correspondence is established
inside the proof but not exported, because `IR.runFunction` returns values and events only.
-/

namespace TptWasm
namespace V4Diamond

open V4Straight Diamond

/-! ## Facts about binding block parameters -/

/-- Binding a list of parameters does not change the store. -/
theorem bindAll_store : ∀ (ids : List IR.ValueId) (vs : List Value) (s : IR.State),
    (s.bindAll ids vs).store = s.store
  | [], _, _ => rfl
  | _ :: _, [], _ => rfl
  | b :: bs, v :: vs, s => by
    simp only [IR.State.bindAll]
    rw [bindAll_store bs vs (s.bind b v)]
    exact bind_store s b v

/-- Binding a list of parameters does not change a value outside the list. -/
theorem bindAll_outside : ∀ (ids : List IR.ValueId) (vs : List Value) (s : IR.State)
    (x : IR.ValueId), x ∉ ids → (s.bindAll ids vs).values x = s.values x
  | [], _, _, _, _ => rfl
  | _ :: _, [], _, _, _ => rfl
  | b :: bs, v :: vs, s, x, h => by
    simp only [IR.State.bindAll]
    rw [bindAll_outside bs vs (s.bind b v) x (fun hx => h (by simp [hx]))]
    have hxb : x ≠ b := fun hb => h (by simp [hb])
    simp [IR.State.bind, hxb]

/-- When the parameters are distinct and match the values one for one, binding them reads back
exactly those values, in order. -/
theorem bindAll_map : ∀ (ids : List IR.ValueId) (vs : List Value) (s : IR.State),
    ids.Nodup → ids.length = vs.length → ids.map (s.bindAll ids vs).values = vs
  | [], [], _, _, _ => rfl
  | [], _ :: _, _, _, h => by simp at h
  | _ :: _, [], _, _, h => by simp at h
  | b :: bs, v :: vs, s, hnd, hlen => by
    rw [List.nodup_cons] at hnd
    have hlen' : bs.length = vs.length := by simpa using hlen
    simp only [IR.State.bindAll, List.map_cons]
    congr 1
    · rw [bindAll_outside bs vs (s.bind b v) b hnd.1]
      simp [IR.State.bind]
    · exact bindAll_map bs vs (s.bind b v) hnd.2 hlen'

/-! ## Facts about the lowering -/

/-- A mapped option that is `some` came from a `some`, with the map applied to it. -/
theorem opt_map_some {α β : Type} : ∀ (o : Option α) (f : α → β) (y : β),
    o.map f = some y → ∃ a, o = some a ∧ f a = y
  | none, _, _, h => by simp at h
  | some a, _, _, h => by
    simp only [Option.map_some, Option.some.injEq] at h
    exact ⟨a, rfl, h⟩

/-- A successful lowering keeps the number of locals and never moves the fresh-id counter down. -/
theorem lower_props : ∀ (body : List Instr) (stack locs : List IR.ValueId) (n : Nat)
    (ir : List IR.Instr) (final fl : List IR.ValueId) (m : Nat),
    lower body stack locs n = some (ir, final, fl, m) → fl.length = locs.length ∧ n ≤ m
  | [], stack, locs, n, ir, final, fl, m, h => by
    simp only [lower, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨_, _, rfl, rfl⟩ := h
    exact ⟨rfl, Nat.le_refl _⟩
  | i :: rest, stack, locs, n, ir, final, fl, m, h => by
    rcases stack with _ | ⟨a, _ | ⟨b, stack⟩⟩ <;> cases i <;> (try simp only [lower] at h) <;>
      (repeat' split at h)
    all_goals first
      | (simp at h; done)
      | (obtain ⟨⟨ir', f', fl', m'⟩, hlow, he⟩ := opt_map_some _ _ _ h
         simp only [Prod.mk.injEq] at he
         obtain ⟨-, rfl, rfl, rfl⟩ := he
         have hi := lower_props rest _ _ _ _ _ _ _ hlow
         exact ⟨hi.1, by have := hi.2; omega⟩)
      | (have hi := lower_props rest _ _ _ _ _ _ _ h
         try simp only [List.length_set] at hi
         exact ⟨hi.1, by have := hi.2; omega⟩)

/-! ## The `i32` global invariant is preserved by the straight-line instructions -/

/-- Every global is `i32`-typed, with an `i32` value. The straight-line simulation assumes this of
the store it runs from. -/
def GlobalsI32 (s : Store) : Prop := ∀ g ∈ s.globals, g.ty = .i32 ∧ g.value.type = .i32

/-- The instructions the straight-line lowering accepts, as far as the store is concerned: every
other instruction makes the lowering refuse, and so cannot appear in a lowered piece. -/
def Supported : Instr → Prop
  | .i32Const _ | .i32Bin _ | .i32Cmp _ | .load _ _ _ | .store _ _ _ | .globalGet _
  | .globalSet _ | .localGet _ | .localSet _ | .localTee _ => True
  | _ => False

/-- One supported instruction keeps every global `i32`-typed. A `global.set` writes a value of the
global's own type; the other rules leave the globals alone or change only memory. -/
theorem step_globals (i : Instr) (c c1 : Configuration) (hs : Supported i)
    (h : execOp i c = some (.step c1)) (hg : GlobalsI32 c.store) : GlobalsI32 c1.store := by
  cases i with
  | globalSet index =>
    simp only [execOp] at h
    cases hgl : c.store.global? index with
    | none => simp [hgl] at h
    | some global =>
      cases hop : c.operandStack with
      | nil => simp [hgl, hop] at h
      | cons value rest =>
        simp only [hgl, hop] at h
        split at h
        next hty =>
          simp only [Option.some.injEq, Outcome.step.injEq] at h
          subst h
          intro g hgm
          have hmem : g ∈ Store.set c.store.globals index { global with value := value } := hgm
          rcases set_mem _ _ _ _ hmem with h1 | h1
          · exact hg g h1
          · subst h1
            have hgt := hg global (get_mem _ _ _ hgl)
            exact ⟨hgt.1, by rw [hty, hgt.1]⟩
        next => simp at h
  | i32Const v =>
    simp only [execOp, Option.some.injEq, Outcome.step.injEq] at h
    subst h; exact hg
  | localGet index =>
    simp only [execOp] at h
    cases hf : c.activeFrame? with
    | none => simp [hf] at h
    | some frame =>
      cases hl : Store.get? frame.locals index with
      | none => simp [hf, hl] at h
      | some a =>
        simp only [hf, hl, Option.map_some, Option.some.injEq, Outcome.step.injEq] at h
        subst h; exact hg
  | localSet index =>
    simp only [execOp] at h
    cases hf : c.activeFrame? with
    | none => simp [hf] at h
    | some frame =>
      cases hop : c.operandStack with
      | nil => simp [hf, hop] at h
      | cons value rest =>
        simp only [hf, hop] at h
        cases hs : Configuration.setLocal? frame index value with
        | none => simp [hs] at h
        | some f =>
          simp only [hs, Option.map_some, Option.some.injEq, Outcome.step.injEq] at h
          subst h; exact hg
  | localTee index =>
    simp only [execOp] at h
    cases hf : c.activeFrame? with
    | none => simp [hf] at h
    | some frame =>
      cases hop : c.operandStack with
      | nil => simp [hf, hop] at h
      | cons value rest =>
        simp only [hf, hop] at h
        cases hs : Configuration.setLocal? frame index value with
        | none => simp [hs] at h
        | some f =>
          simp only [hs, Option.map_some, Option.some.injEq, Outcome.step.injEq] at h
          subst h; exact hg
  | globalGet index =>
    simp only [execOp] at h
    cases hgl : c.store.global? index with
    | none => simp [hgl] at h
    | some global =>
      simp only [hgl, Option.some.injEq, Outcome.step.injEq] at h
      subst h; exact hg
  | i32Bin op =>
    simp only [execOp] at h
    split at h
    · next right left rest hop =>
      split at h
      · next result hev =>
        simp only [Option.some.injEq, Outcome.step.injEq] at h
        subst h; exact hg
      · simp at h
    all_goals simp at h
  | i32Cmp op =>
    simp only [execOp] at h
    split at h
    · simp only [Option.some.injEq, Outcome.step.injEq] at h
      subst h; exact hg
    all_goals simp at h
  | load op index arg =>
    simp only [execOp] at h
    split at h
    · next address rest memory hmem =>
      split at h
      · next raw hr =>
        simp only [Option.some.injEq, Outcome.step.injEq] at h
        subst h; exact hg
      · simp at h
    all_goals simp at h
  | store op index arg =>
    simp only [execOp] at h
    split at h
    · next operand address rest memory hmem =>
      split at h
      · next hty =>
        split at h
        · next updated hw =>
          simp only [Option.some.injEq, Outcome.step.injEq] at h
          subst h; exact hg
        · simp at h
      · simp at h
    all_goals simp at h
  | _ => simp [Supported] at hs

/-- A body of supported instructions keeps every global `i32`-typed when it runs. -/
theorem runWasm_globals : ∀ (body : List Instr) (c c' : Configuration) (evs : List Event),
    (∀ i ∈ body, Supported i) → GlobalsI32 c.store → runWasm body c = some (c', evs) →
    GlobalsI32 c'.store
  | [], c, c', _, _, hg, h => by
    simp only [runWasm, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    exact hg
  | i :: rest, c, c', evs, hs, hg, h => by
    have hsi : Supported i := hs i (by simp)
    cases hexec : execOp i c with
    | none => simp [runWasm, hexec] at h
    | some o =>
      cases o with
      | trap t => simp [runWasm, hexec] at h
      | returning vs => simp [runWasm, hexec] at h
      | step c1 =>
        have hg1 := step_globals i c c1 hsi hexec hg
        obtain ⟨evs', hr', -⟩ := runWasm_cons_inv hexec h
        exact runWasm_globals rest c1 c' evs' (fun j hj => hs j (by simp [hj])) hg1 hr'

/-- A successful lowering lowers only supported instructions. -/
theorem lower_supported : ∀ (body : List Instr) (stack locs : List IR.ValueId) (n : Nat)
    (ir : List IR.Instr) (final fl : List IR.ValueId) (m : Nat),
    lower body stack locs n = some (ir, final, fl, m) → ∀ i ∈ body, Supported i
  | [], _, _, _, _, _, _, _, _, i, hi => by simp at hi
  | j :: rest, stack, locs, n, ir, final, fl, m, h, i, hi => by
    rcases stack with _ | ⟨a, _ | ⟨b, stack⟩⟩ <;> cases j <;> (try simp only [lower] at h) <;>
      (repeat' split at h)
    all_goals first
      | (simp at h; done)
      | (obtain ⟨⟨ir', f', fl', m'⟩, hlow, -⟩ := opt_map_some _ _ _ h
         have ih := lower_supported rest _ _ _ _ _ _ _ hlow
         simp only [List.mem_cons] at hi
         rcases hi with rfl | hi
         · simp [Supported]
         · exact ih i hi)
      | (have ih := lower_supported rest _ _ _ _ _ _ _ h
         simp only [List.mem_cons] at hi
         rcases hi with rfl | hi
         · simp [Supported]
         · exact ih i hi)

/-! ## Arithmetic of the fresh ids -/

/-- The then arm's parameters are below the then arm's first fresh id. -/
theorem arith_lt_then (n1 L id : Nat) (h : n1 ≤ id ∧ id < n1 + L) : id < n1 + 3 * L := by
  omega

/-- The else arm's parameters are below the else arm's start, which the then arm leaves below. -/
theorem arith_lt_else (n1 L nT id : Nat) (hnT : n1 + 3 * L ≤ nT)
    (h : n1 + L ≤ id ∧ id < n1 + L + L) : id < nT := by
  omega

/-! ## Running blocks -/

/-- The fuel-free body run and the block-run body agree on success. -/
theorem runBodyR_of_runBody : ∀ (body : List IR.Instr) (s : IR.State) (t : List Event)
    (s' : IR.State) (t' : List Event),
    IR.runBody body s t = some (s', t') → IR.runBodyR body s t = .inr (s', t')
  | [], s, t, s', t', h => by
    simp only [IR.runBody, Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    rfl
  | i :: rest, s, t, s', t', h => by
    cases hx : IR.execInstr i s with
    | trapped t0 => simp [IR.runBody, hx] at h
    | ran s1 ev res =>
      simp only [IR.runBody, IR.runBodyR, hx] at h ⊢
      exact runBodyR_of_runBody rest _ _ s' t' h

/-- The state an edge enters a block with, when the block's parameters are the fresh ids of the
locals and the edge carries the locals' values: the parameters read back the values, the frame
holds them, and the store is unchanged. -/
theorem enter_corr (c1 : Configuration) (s1 : IR.State) (locsPre P : List IR.ValueId)
    (n1 nA : Nat) (hloc : LocalsOK c1 s1 locsPre n1) (hst : c1.store = s1.store)
    (hlen : P.length = locsPre.length) (hnd : P.Nodup) (hPn : ∀ id ∈ P, id < nA) :
    Corresponds { c1 with operandStack := [] } (s1.bindAll P (locsPre.map s1.values)) [] P nA := by
  obtain ⟨⟨fr, hfr, hfl⟩, -, hi⟩ := hloc
  have hmap : P.map (s1.bindAll P (locsPre.map s1.values)).values = locsPre.map s1.values :=
    bindAll_map P _ s1 hnd (by simp [hlen])
  refine ⟨rfl, by simp, by simp, ?_, ?_⟩
  · rw [bindAll_store, hst]
  · refine ⟨⟨fr, hfr, ?_⟩, hPn, ?_⟩
    · rw [hmap]; exact hfl
    · intro id hid
      have hmem := List.mem_map_of_mem (f := (s1.bindAll P (locsPre.map s1.values)).values) hid
      rw [hmap] at hmem
      obtain ⟨a, ha, heq⟩ := List.mem_map.mp hmem
      obtain ⟨x, hx⟩ := hi a ha
      exact ⟨x, by rw [← heq, hx]⟩

/-- The join block: it runs the post-join piece from the values its edge carries, and returns
the function's results. -/
theorem join_run (blocks : List IR.Block) (k : Nat) (M : IR.Block) (post : List Instr)
    (final finalLocs mp : List IR.ValueId) (irM : List IR.Instr) (nE nM : Nat)
    (hMi : M.instrs = irM) (hMt : M.terminator = .ret final)
    (hlowM : lower post [] mp nE = some (irM, final, finalLocs, nM))
    (hnd : mp.Nodup) (hmpn : ∀ id ∈ mp, id < nE)
    (sA : IR.State) (c3 c4 : Configuration) (locsA : List IR.ValueId) (nA : Nat)
    (t e3 : List Event)
    (hc3 : Corresponds c3 sA [] locsA nA) (hlen : locsA.length = mp.length)
    (hg : GlobalsI32 c3.store) (hpost : runWasm post c3 = some (c4, e3)) :
    IR.runBlocks blocks (k + 1) M (sA.bindAll mp (locsA.map sA.values)) t
      = .returned (t ++ e3) c4.operandStack := by
  obtain ⟨hstk, -, -, hst, hloc⟩ := hc3
  have hmap : mp.map (sA.bindAll mp (locsA.map sA.values)).values = locsA.map sA.values :=
    bindAll_map mp _ sA hnd (by simp [hlen])
  have hcorr : Corresponds c3 (sA.bindAll mp (locsA.map sA.values)) [] mp nE := by
    refine ⟨by simpa using hstk, by simp, by simp, ?_, ?_⟩
    · rw [bindAll_store, hst]
    · obtain ⟨⟨fr, hfr, hfl⟩, -, hi⟩ := hloc
      refine ⟨⟨fr, hfr, ?_⟩, hmpn, ?_⟩
      · rw [hmap]; exact hfl
      · intro id hid
        have hmem := List.mem_map_of_mem (f := (sA.bindAll mp (locsA.map sA.values)).values) hid
        rw [hmap] at hmem
        obtain ⟨a, ha, heq⟩ := List.mem_map.mp hmem
        obtain ⟨x, hx⟩ := hi a ha
        exact ⟨x, by rw [← heq, hx]⟩
  obtain ⟨sM', hrun, hcorr'⟩ :=
    simulation post c3 c4 e3 _ [] mp nE irM final finalLocs nM hcorr hg hlowM hpost t
  have hR := runBodyR_of_runBody irM _ t sM' (t ++ e3) hrun
  simp only [IR.runBlocks, hMi, hR, hMt]
  rw [hcorr'.1]

/-- One arm, then the join. The arm's block enters the join with the locals the arm left. -/
theorem arm_run (blocks : List IR.Block) (k : Nat) (B M : IR.Block) (armSrc post : List Instr)
    (irA : List IR.Instr) (locsA P mp : List IR.ValueId) (nA nB : Nat)
    (hBi : B.instrs = irA) (hBt : B.terminator = .branch joinBlock locsA)
    (hJ : IR.findBlock blocks joinBlock = some M) (hMp : M.params = mp)
    (hlowA : lower armSrc [] P nA = some (irA, [], locsA, nB))
    (hjoin : ∀ (sA : IR.State) (c3 c4 : Configuration) (t e3 : List Event),
        Corresponds c3 sA [] locsA nB → GlobalsI32 c3.store → runWasm post c3 = some (c4, e3) →
        IR.runBlocks blocks (k + 1) M (sA.bindAll mp (locsA.map sA.values)) t
          = .returned (t ++ e3) c4.operandStack)
    (sB : IR.State) (c2 c3 : Configuration) (e2 t : List Event)
    (hc2 : Corresponds c2 sB [] P nA) (hg : GlobalsI32 c2.store)
    (hArm : runWasm armSrc c2 = some (c3, e2)) (c4 : Configuration) (e3 : List Event)
    (hpost : runWasm post c3 = some (c4, e3)) :
    IR.runBlocks blocks (k + 2) B sB t = .returned (t ++ e2 ++ e3) c4.operandStack := by
  obtain ⟨sA, hrunA, hcA⟩ :=
    simulation armSrc c2 c3 e2 sB [] P nA irA [] locsA nB hc2 hg hlowA hArm t
  have hg3 := runWasm_globals armSrc c2 c3 e2 (lower_supported armSrc [] P nA irA [] locsA nB hlowA)
    hg hArm
  have hR := runBodyR_of_runBody irA sB t sA (t ++ e2) hrunA
  simp only [IR.runBlocks, hBi, hR, hBt, hJ, hMp]
  exact hjoin sA c3 c4 (t ++ e2) e3 hcA hg3 hpost

/-- A successful lowering of a diamond decomposes into the four lowered pieces, with the parameter
ids the lowering allocates. -/
theorem lowerShape_some (d : Shape) (locs : List IR.ValueId) (n : Nat) (f : IR.Function)
    (h : lowerShape d locs n = some f) :
    ∃ (irPre : List IR.Instr) (cond : IR.ValueId) (locsPre : List IR.ValueId) (n1 : Nat)
      (irT : List IR.Instr) (locsT : List IR.ValueId) (nT : Nat)
      (irE : List IR.Instr) (locsE : List IR.ValueId) (nE : Nat)
      (irM : List IR.Instr) (final finalLocs : List IR.ValueId) (nM : Nat),
      lower d.pre [] locs n = some (irPre, [cond], locsPre, n1) ∧
      lower d.thenArm [] (List.range' n1 locs.length) (n1 + 3 * locs.length)
        = some (irT, [], locsT, nT) ∧
      lower d.elseArm [] (List.range' (n1 + locs.length) locs.length) nT
        = some (irE, [], locsE, nE) ∧
      lower d.post [] (List.range' (n1 + 2 * locs.length) locs.length) nE
        = some (irM, final, finalLocs, nM) ∧
      f = { params := locs, locals := [], declared := [],
            blocks := [blkEntry irPre cond locsPre,
              blkThen (List.range' n1 locs.length) irT locsT,
              blkElse (List.range' (n1 + locs.length) locs.length) irE locsE,
              blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final],
            entry := entryBlock } := by
  unfold lowerShape at h
  dsimp only at h
  split at h
  · rename_i irPre cond locsPre n1 hpre
    split at h
    · rename_i irT locsT nT hthen
      split at h
      · rename_i irE locsE nE helse
        split at h
        · rename_i irM final finalLocs nM hpost
          exact ⟨irPre, cond, locsPre, n1, irT, locsT, nT, irE, locsE, nE, irM, final, finalLocs,
            nM, hpre, hthen, helse, hpost, (Option.some.inj h).symm⟩
        · simp at h
      · simp at h
    · simp at h
  · simp at h

/-! ## One step of the block graph -/

theorem runFunction_entry (f : IR.Function) (fuel : Nat) (s : IR.State) (b : IR.Block)
    (h : IR.findBlock f.blocks f.entry = some b) :
    IR.runFunction f fuel s = IR.runBlocks f.blocks fuel b s [] := by
  simp [IR.runFunction, h]

theorem runBlocks_branch (blocks : List IR.Block) (n : Nat) (b tb : IR.Block) (s s' : IR.State)
    (tr tr' : List Event) (target : IR.BlockId) (args : List IR.ValueId)
    (hB : IR.runBodyR b.instrs s tr = .inr (s', tr')) (hT : b.terminator = .branch target args)
    (hf : IR.findBlock blocks target = some tb) :
    IR.runBlocks blocks (n + 1) b s tr
      = IR.runBlocks blocks n tb (s'.bindAll tb.params (args.map s'.values)) tr' := by
  simp only [IR.runBlocks, hB, hT, hf]

theorem runBlocks_cond_else (blocks : List IR.Block) (n : Nat) (b tb : IR.Block) (s s' : IR.State)
    (tr tr' : List Event) (c : IR.ValueId) (tt et : IR.BlockId) (ta ea : List IR.ValueId)
    (hB : IR.runBodyR b.instrs s tr = .inr (s', tr'))
    (hT : b.terminator = .condBranch c (tt, ta) (et, ea))
    (hv : IR.intPayload (s'.values c) = 0) (hf : IR.findBlock blocks et = some tb) :
    IR.runBlocks blocks (n + 1) b s tr
      = IR.runBlocks blocks n tb (s'.bindAll tb.params (ea.map s'.values)) tr' := by
  simp only [IR.runBlocks, hB, hT, hv, hf, ite_true]

theorem runBlocks_cond_then (blocks : List IR.Block) (n : Nat) (b tb : IR.Block) (s s' : IR.State)
    (tr tr' : List Event) (c : IR.ValueId) (tt et : IR.BlockId) (ta ea : List IR.ValueId)
    (hB : IR.runBodyR b.instrs s tr = .inr (s', tr'))
    (hT : b.terminator = .condBranch c (tt, ta) (et, ea))
    (hv : IR.intPayload (s'.values c) ≠ 0) (hf : IR.findBlock blocks tt = some tb) :
    IR.runBlocks blocks (n + 1) b s tr
      = IR.runBlocks blocks n tb (s'.bindAll tb.params (ta.map s'.values)) tr' := by
  simp only [IR.runBlocks, hB, hT, hv, hf, ite_false]

theorem runBlocks_ret (blocks : List IR.Block) (n : Nat) (b : IR.Block) (s s' : IR.State)
    (tr tr' : List Event) (values : List IR.ValueId)
    (hB : IR.runBodyR b.instrs s tr = .inr (s', tr')) (hT : b.terminator = .ret values) :
    IR.runBlocks blocks (n + 1) b s tr = .returned tr' (values.map s'.values) := by
  simp only [IR.runBlocks, hB, hT]

/-- The diamond simulation.

If the lowering of a diamond succeeds, the Wasm configuration and the IR state correspond, every
global is `i32`-typed, and the Wasm run of the diamond returns `c'` with events `evs`, then the IR
function run from its entry with fuel `k + 3` returns exactly `evs` and the values `c'` leaves on
its operand stack. -/
theorem diamond_simulation (d : Shape) (c c' : Configuration) (evs : List Event)
    (s : IR.State) (locs : List IR.ValueId) (n : Nat) (f : IR.Function) (k : Nat)
    (hc : Corresponds c s [] locs n) (hg : GlobalsI32 c.store)
    (hl : lowerShape d locs n = some f) (hr : runShape d c = some (c', evs)) :
    IR.runFunction f (k + 3) s = .returned evs c'.operandStack := by
  obtain ⟨irPre, cond, locsPre, n1, irT, locsT, nT, irE, locsE, nE, irM, final, finalLocs, nM,
    hpre, hthen, helse, hpost, rfl⟩ := lowerShape_some d locs n f hl
  have hL1 := lower_props d.pre [] locs n irPre [cond] locsPre n1 hpre
  have hLT := lower_props d.thenArm _ _ _ _ _ _ _ hthen
  have hLE := lower_props d.elseArm _ _ _ _ _ _ _ helse
  have hLM := lower_props d.post _ _ _ _ _ _ _ hpost
  have hnT : n1 + 3 * locs.length ≤ nT := hLT.2
  have hnE : nT ≤ nE := hLE.2
  have hlPre : locsPre.length = locs.length := hL1.1
  unfold runShape at hr
  cases hpreR : runWasm d.pre c with
  | none => simp [hpreR] at hr
  | some p =>
    obtain ⟨c1, e1⟩ := p
    simp only [hpreR] at hr
    split at hr
    next v hop =>
      obtain ⟨s1, hs1, hc1⟩ :=
        simulation d.pre c c1 e1 s [] locs n irPre [cond] locsPre n1 hc hg hpre hpreR []
      have hv : s1.values cond = .i32 v := by
        have h1 := hc1.1
        rw [hop] at h1
        simp only [List.map_cons, List.map_nil, List.cons.injEq, and_true] at h1
        exact h1.symm
      have hg1 := runWasm_globals d.pre c c1 e1
        (lower_supported d.pre [] locs n irPre [cond] locsPre n1 hpre) hg hpreR
      have hloc1 := hc1.2.2.2.2
      have hst1 := hc1.2.2.2.1
      have hR0 := runBodyR_of_runBody irPre s [] s1 e1 (by simpa using hs1)
      have hmpn : ∀ id ∈ List.range' (n1 + 2 * locs.length) locs.length, id < nE := by
        intro id hid
        rw [List.mem_range'_1] at hid
        omega
      have hndM : (List.range' (n1 + 2 * locs.length) locs.length).Nodup :=
        List.nodup_range' (s := n1 + 2 * locs.length) (n := locs.length)
      -- The entry block runs `pre`, then the condition selects an arm.
      rw [runFunction_entry _ (k + 3) s (blkEntry irPre cond locsPre) rfl]
      by_cases hv0 : v = 0
      · -- The condition is zero, so the else arm runs.
        have hv' : IR.intPayload (s1.values cond) = 0 := by rw [hv]; simp [hv0, IR.intPayload]
        rw [runBlocks_cond_else
          [blkEntry irPre cond locsPre,
            blkThen (List.range' n1 locs.length) irT locsT,
            blkElse (List.range' (n1 + locs.length) locs.length) irE locsE,
            blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final]
          (k + 2) (blkEntry irPre cond locsPre)
          (blkElse (List.range' (n1 + locs.length) locs.length) irE locsE) s s1 [] e1 cond
          thenBlock elseBlock locsPre locsPre hR0 rfl hv' rfl]
        simp only [hv0, ↓reduceIte] at hr
        cases harm : runWasm d.elseArm { c1 with operandStack := [] } with
        | none => simp [harm] at hr
        | some p2 =>
          obtain ⟨c3, e2⟩ := p2
          simp only [harm] at hr
          split at hr
          next hemp =>
            cases hp4 : runWasm d.post c3 with
            | none => simp [hp4] at hr
            | some p3 =>
              obtain ⟨c4, e3⟩ := p3
              simp only [hp4, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hr
              obtain ⟨rfl, rfl⟩ := hr
              have hc2 := enter_corr c1 s1 locsPre (List.range' (n1 + locs.length) locs.length)
                n1 nT hloc1 hst1 (by simp [hlPre]) (List.nodup_range' (s := n1 + locs.length) (n := locs.length))
                (fun id hid => arith_lt_else n1 locs.length nT id hnT (List.mem_range'_1.mp hid))
              have hg2 : GlobalsI32 { c1 with operandStack := [] }.store := hg1
              exact arm_run
                [blkEntry irPre cond locsPre,
                  blkThen (List.range' n1 locs.length) irT locsT,
                  blkElse (List.range' (n1 + locs.length) locs.length) irE locsE,
                  blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final]
                k
                (blkElse (List.range' (n1 + locs.length) locs.length) irE locsE)
                (blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final)
                d.elseArm d.post irE locsE (List.range' (n1 + locs.length) locs.length)
                (List.range' (n1 + 2 * locs.length) locs.length) nT nE rfl rfl rfl rfl helse
                (fun sA c3' c4' t e3' hc3' hg3' hp' =>
                  join_run
                    [blkEntry irPre cond locsPre,
                      blkThen (List.range' n1 locs.length) irT locsT,
                      blkElse (List.range' (n1 + locs.length) locs.length) irE locsE,
                      blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final]
                    k
                    (blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final)
                    d.post final finalLocs (List.range' (n1 + 2 * locs.length) locs.length) irM nE nM
                    rfl rfl hpost hndM hmpn sA c3' c4' locsE nE t e3' hc3'
                    (by simp [(lower_props d.elseArm _ _ _ _ _ _ _ helse).1]) hg3' hp')
                (s1.bindAll (List.range' (n1 + locs.length) locs.length) (locsPre.map s1.values))
                { c1 with operandStack := [] } c3 e2 e1 hc2 hg2 harm c4 e3 hp4
          next => simp at hr
      · -- The condition is non-zero, so the then arm runs.
        have hv' : IR.intPayload (s1.values cond) ≠ 0 := by rw [hv]; simp [hv0, IR.intPayload]
        rw [runBlocks_cond_then
          [blkEntry irPre cond locsPre,
            blkThen (List.range' n1 locs.length) irT locsT,
            blkElse (List.range' (n1 + locs.length) locs.length) irE locsE,
            blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final]
          (k + 2) (blkEntry irPre cond locsPre)
          (blkThen (List.range' n1 locs.length) irT locsT) s s1 [] e1 cond
          thenBlock elseBlock locsPre locsPre hR0 rfl hv' rfl]
        simp only [hv0, ↓reduceIte] at hr
        cases harm : runWasm d.thenArm { c1 with operandStack := [] } with
        | none => simp [harm] at hr
        | some p2 =>
          obtain ⟨c3, e2⟩ := p2
          simp only [harm] at hr
          split at hr
          next hemp =>
            cases hp4 : runWasm d.post c3 with
            | none => simp [hp4] at hr
            | some p3 =>
              obtain ⟨c4, e3⟩ := p3
              simp only [hp4, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hr
              obtain ⟨rfl, rfl⟩ := hr
              have hc2 := enter_corr c1 s1 locsPre (List.range' n1 locs.length)
                n1 (n1 + 3 * locs.length) hloc1 hst1 (by simp [hlPre]) (List.nodup_range' (s := n1) (n := locs.length))
                (fun id hid => arith_lt_then n1 locs.length id (List.mem_range'_1.mp hid))
              have hg2 : GlobalsI32 { c1 with operandStack := [] }.store := hg1
              exact arm_run
                [blkEntry irPre cond locsPre,
                  blkThen (List.range' n1 locs.length) irT locsT,
                  blkElse (List.range' (n1 + locs.length) locs.length) irE locsE,
                  blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final]
                k
                (blkThen (List.range' n1 locs.length) irT locsT)
                (blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final)
                d.thenArm d.post irT locsT (List.range' n1 locs.length)
                (List.range' (n1 + 2 * locs.length) locs.length) (n1 + 3 * locs.length) nT
                rfl rfl rfl rfl hthen
                (fun sA c3' c4' t e3' hc3' hg3' hp' =>
                  join_run
                    [blkEntry irPre cond locsPre,
                      blkThen (List.range' n1 locs.length) irT locsT,
                      blkElse (List.range' (n1 + locs.length) locs.length) irE locsE,
                      blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final]
                    k
                    (blkJoin (List.range' (n1 + 2 * locs.length) locs.length) irM final)
                    d.post final finalLocs (List.range' (n1 + 2 * locs.length) locs.length) irM nE nM
                    rfl rfl hpost hndM hmpn sA c3' c4' locsT nT t e3' hc3'
                    (by simp [(lower_props d.thenArm _ _ _ _ _ _ _ hthen).1]) hg3' hp')
                (s1.bindAll (List.range' n1 locs.length) (locsPre.map s1.values))
                { c1 with operandStack := [] } c3 e2 e1 hc2 hg2 harm c4 e3 hp4
          next => simp at hr
    next => simp at hr

end V4Diamond
end TptWasm
