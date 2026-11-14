import TptWasm.Proofs.FoldGlobal

/-!
# Constant-condition folding: the whole function

`fold_behaviour`: folding every conditional branch whose condition is a known constant
leaves every function's behaviour unchanged, at every fuel. The two functions have the
same states at every point of a run, so they agree exactly. See `TptWasm.Proofs.FoldGlobal`
for the invariant `Good` and the checks `FoldCtx` states.
-/

namespace TptWasm
namespace IR

/-- The verifier's checks the fold relies on, for the model. -/
structure FoldCtx (f : Function) (consts : ValueId → Option (BlockId × Int)) : Prop where
  hnodup : (f.blocks.map Block.id).Nodup
  hex : ∀ c D k, consts c = some (D, k) →
    ∃ B ∈ f.blocks, B.id = D ∧ Instr.constI32 c k ∈ B.instrs
  hdefs : ∀ B ∈ f.blocks, ∀ i ∈ B.instrs, ∀ c D k, consts c = some (D, k) →
    c ∈ definedBy i → B.id = D ∧ i = Instr.constI32 c k
  hnopar : ∀ B ∈ f.blocks, ∀ c D k, consts c = some (D, k) → c ∉ B.params
  hdom : ∀ X ∈ f.blocks, ∀ c D k, consts c = some (D, k) → c ∈ X.terminator.reads →
    X.id = D ∨ Dom f D X.id

/-- The invariant: a state entering `X` binds every constant whose defining block strictly
dominates `X`. -/
def Good (f : Function) (consts : ValueId → Option (BlockId × Int)) (X : Block) (s : State) :
    Prop :=
  ∀ c D k, consts c = some (D, k) → D ≠ X.id → Dom f D X.id → s.values c = .i32 k

/-- Two blocks of a function with unique identifiers that share an identifier are equal. -/
theorem eq_of_id_nodup : ∀ (l : List Block), (l.map Block.id).Nodup →
    ∀ a ∈ l, ∀ b ∈ l, a.id = b.id → a = b
  | [], _ => fun a ha => absurd ha (by simp)
  | x :: rest, hnd => by
    intro a ha b hb hab
    rw [List.map_cons, List.nodup_cons] at hnd
    rcases List.mem_cons.mp ha with rfl | ha' <;> rcases List.mem_cons.mp hb with rfl | hb'
    · rfl
    · exact absurd (by rw [hab]; exact List.mem_map_of_mem hb') hnd.1
    · exact absurd (by rw [← hab]; exact List.mem_map_of_mem ha') hnd.1
    · exact eq_of_id_nodup rest hnd.2 a ha' b hb' hab

/-- A body that defines the constant `c` as `k` leaves `c` bound to `k` once it has run. -/
theorem body_produce (c : ValueId) (k : Int) (B : List Instr)
    (hdef : ∀ i ∈ B, c ∈ definedBy i → i = .constI32 c k)
    (hmem : Instr.constI32 c k ∈ B) (s : State) (tr : List Event) (s' : State)
    (tr' : List Event) (h : runBodyR B s tr = .inr (s', tr')) : s'.values c = .i32 k := by
  obtain ⟨l1, l2, rfl⟩ := List.append_of_mem hmem
  rw [runBodyR_append] at h
  cases hL : runBodyR l1 s tr with
  | inl p => rw [hL] at h; simp at h
  | inr p =>
    obtain ⟨s1, t1⟩ := p
    rw [hL] at h
    dsimp only at h
    have hstep : runBodyR (Instr.constI32 c k :: l2) s1 t1 =
        runBodyR l2 (s1.bind c (.i32 k)) t1 := rfl
    rw [hstep] at h
    have hv : (s1.bind c (.i32 k)).values c = .i32 k := by simp [State.bind]
    exact body_stable c k l2
      (fun i hi hci => hdef i (List.mem_append_right _ (List.mem_cons_of_mem _ hi)) hci)
      (s1.bind c (.i32 k)) t1 s' tr' hv h

/-- The fold of a terminator: a conditional branch on a constant becomes the arm it selects. -/
def foldTermConsts (consts : ValueId → Option (BlockId × Int)) : Terminator → Terminator
  | .condBranch c a b =>
    match consts c with
    | some (_, k) => foldCond c k (.condBranch c a b)
    | none => .condBranch c a b
  | T => T

/-- A block with its terminator folded. -/
def foldBlockConsts (consts : ValueId → Option (BlockId × Int)) (B : Block) : Block :=
  { B with terminator := foldTermConsts consts B.terminator }

/-- A function with every constant-condition branch folded. -/
def foldFnConsts (f : Function) (consts : ValueId → Option (BlockId × Int)) : Function :=
  { f with blocks := f.blocks.map (foldBlockConsts consts) }

theorem findBlock_map_id (φ : Block → Block) (hφ : ∀ b, (φ b).id = b.id) :
    ∀ (l : List Block) (id : BlockId),
      findBlock (l.map φ) id = (findBlock l id).map φ := by
  intro l
  induction l with
  | nil => intro id; rfl
  | cons b rest ih =>
    intro id
    unfold findBlock at ih ⊢
    simp only [List.map_cons, List.find?_cons, hφ]
    cases hb : (b.id == id) with
    | true => rfl
    | false => simpa using ih id

/-- The invariant holds for the post-body state of a block that dominates the constant's
definition. -/
theorem post_binding (f : Function) (consts : ValueId → Option (BlockId × Int))
    (C : FoldCtx f consts) (X : Block) (hX : X ∈ f.blocks) (s : State) (tr : List Event)
    (s' : State) (tr' : List Event) (hG : Good f consts X s)
    (hinr : runBodyR X.instrs s tr = .inr (s', tr')) (c : ValueId) (D : BlockId) (k : Int)
    (hc : consts c = some (D, k)) (hd : X.id = D ∨ Dom f D X.id) :
    s'.values c = .i32 k := by
  by_cases hXD : X.id = D
  · obtain ⟨B, hB, hBid, hBc⟩ := C.hex c D k hc
    have hBX : B = X := eq_of_id_nodup f.blocks C.hnodup B hB X hX (hBid.trans hXD.symm)
    rw [hBX] at hBc
    exact body_produce c k X.instrs (fun i hi hci => (C.hdefs X hX i hi c D k hc hci).2)
      hBc s tr s' tr' hinr
  · have hdomX : Dom f D X.id := hd.resolve_left hXD
    have hsX : s.values c = .i32 k := hG c D k hc (fun e => hXD e.symm) hdomX
    have hkeep : s'.values c = s.values c :=
      runBodyR_values_keep X.instrs s tr s' tr' c
        (fun i hi hci => hXD (C.hdefs X hX i hi c D k hc hci).1) hinr
    rw [hkeep, hsX]

/-- The invariant is preserved along an edge into a block of the function. -/
theorem good_target (f : Function) (consts : ValueId → Option (BlockId × Int))
    (C : FoldCtx f consts) (X : Block) (hX : X ∈ f.blocks) (s : State) (tr : List Event)
    (s' : State) (tr' : List Event) (hG : Good f consts X s)
    (hinr : runBodyR X.instrs s tr = .inr (s', tr')) (Y : Block) (hY : Y ∈ f.blocks)
    (p : BlockId × List ValueId) (hp : p ∈ X.terminator.targets) (hpY : p.1 = Y.id) :
    Good f consts Y (s'.bindAll Y.params (p.2.map s'.values)) := by
  intro c D k hc hDY hdomY
  rw [bindAll_keep Y.params _ s' c (C.hnopar Y hY c D k hc)]
  refine post_binding f consts C X hX s tr s' tr' hG hinr c D k hc ?_
  by_cases hXD : X.id = D
  · exact Or.inl hXD
  · exact Or.inr (dom_pred hdomY ⟨X, hX, rfl, p, hp, hpY⟩ (Ne.symm hDY))

/-- A folded terminator gives the same result as the original, given the constant binding
of its condition and that the targets agree. -/
theorem term_fold_eq (consts : ValueId → Option (BlockId × Int)) (G F : List Block) (n : Nat)
    (s' : State) (tr' : List Event) (T : Terminator)
    (hpost : ∀ c D k, consts c = some (D, k) → c ∈ T.reads → s'.values c = .i32 k)
    (htarget : ∀ p ∈ T.targets, runAt G n p.1 p.2 s' tr' = runAt F n p.1 p.2 s' tr') :
    termResult G n (foldTermConsts consts T) s' tr' = termResult F n T s' tr' := by
  cases T with
  | ret values => rfl
  | trap t => rfl
  | unreachable => rfl
  | branch t a => exact htarget (t, a) (by simp [Terminator.targets])
  | condBranch c a b =>
    obtain ⟨t1, a1⟩ := a
    obtain ⟨t2, a2⟩ := b
    simp only [foldTermConsts]
    cases hcc : consts c with
    | none =>
      simp only [termResult]
      by_cases hz : intPayload (s'.values c) = 0
      · simp only [hz, ↓reduceIte]
        exact htarget (t2, a2) (by simp [Terminator.targets])
      · simp only [hz, ↓reduceIte]
        exact htarget (t1, a1) (by simp [Terminator.targets])
    | some p =>
      obtain ⟨D, k⟩ := p
      have hc : s'.values c = .i32 k :=
        hpost c D k hcc (by simp [Terminator.reads])
      rw [termResult_foldCond G n c k (Terminator.condBranch c (t1, a1) (t2, a2)) s' tr' hc]
      simp only [termResult]
      by_cases hz : intPayload (s'.values c) = 0
      · simp only [hz, ↓reduceIte]
        exact htarget (t2, a2) (by simp [Terminator.targets])
      · simp only [hz, ↓reduceIte]
        exact htarget (t1, a1) (by simp [Terminator.targets])

/-- The fold preserves the block-level runs, at every fuel, from every state satisfying the
invariant. -/
theorem fold_block_eq (f : Function) (consts : ValueId → Option (BlockId × Int))
    (C : FoldCtx f consts) (n : Nat) :
    ∀ X ∈ f.blocks, ∀ (s : State) (tr : List Event), Good f consts X s →
      runBlocks (foldFnConsts f consts).blocks n (foldBlockConsts consts X) s tr =
        runBlocks f.blocks n X s tr := by
  induction n with
  | zero =>
    intro X _ s tr _
    rw [runBlocks_zero, runBlocks_zero]
  | succ n ih =>
    intro X hX s tr hG
    rw [runBlocks_succ, runBlocks_succ]
    dsimp only [foldBlockConsts]
    generalize hbody : runBodyR X.instrs s tr = res
    rcases res with ⟨t1, x1⟩ | ⟨s', tr'⟩
    · rfl
    · apply term_fold_eq consts (foldFnConsts f consts).blocks f.blocks n s' tr' X.terminator
      · intro c D k hc hread
        exact post_binding f consts C X hX s tr s' tr' hG hbody c D k hc
          (C.hdom X hX c D k hc hread)
      · intro p hp
        have hG' : findBlock (foldFnConsts f consts).blocks p.1 =
            (findBlock f.blocks p.1).map (foldBlockConsts consts) :=
          findBlock_map_id (foldBlockConsts consts) (fun b => rfl) f.blocks p.1
        rcases Option.eq_none_or_eq_some (findBlock f.blocks p.1) with hf | ⟨Y, hf⟩
        · rw [runAt_none (by rw [hG', hf]; rfl), runAt_none hf]
        · rw [runAt_some (c := foldBlockConsts consts Y) (by rw [hG', hf]; rfl), runAt_some hf]
          exact ih Y (findBlock_mem hf) _ tr'
            (good_target f consts C X hX s tr s' tr' hG hbody Y (findBlock_mem hf) p hp
              (findBlock_id hf).symm)

/-- At the entry, the invariant holds vacuously: nothing strictly dominates the entry. -/
theorem good_entry (f : Function) (consts : ValueId → Option (BlockId × Int)) (X : Block)
    (hXid : X.id = f.entry) (s : State) : Good f consts X s := by
  intro c D k hc hD hdomE
  have hne : f.entry ≠ D := fun e => hD (e.symm.trans hXid.symm)
  have hR : RA f D X.id := by
    rw [hXid]
    exact RA.entry hne
  unfold Dom at hdomE
  exact (hdomE hR).elim

/-- Folding every constant-condition branch leaves the behaviour of the function unchanged,
at every fuel and from every state. -/
theorem fold_behaviour (f : Function) (consts : ValueId → Option (BlockId × Int))
    (C : FoldCtx f consts) (s : State) (n : Nat) :
    runFunction (foldFnConsts f consts) n s = runFunction f n s := by
  unfold runFunction
  have hfind : findBlock (foldFnConsts f consts).blocks (foldFnConsts f consts).entry =
      (findBlock f.blocks f.entry).map (foldBlockConsts consts) :=
    findBlock_map_id (foldBlockConsts consts) (fun b => rfl) f.blocks f.entry
  rw [hfind]
  cases hf : findBlock f.blocks f.entry with
  | none => rfl
  | some X =>
    exact fold_block_eq f consts C n X (findBlock_mem hf) s [] (good_entry f consts X
      (findBlock_id hf) s)

end IR
end TptWasm
