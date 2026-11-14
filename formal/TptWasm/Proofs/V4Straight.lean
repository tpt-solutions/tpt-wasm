import TptWasm.Transition
import TptWasm.IRExec
import TptWasm.Proofs.V4

/-!
# V4, straight-line simulation: a Wasm body and its lowering step together

`TptWasm.Proofs.V4` states the per-instruction correspondence `Related` and proves the
store-level facts. This file proves the simulation those facts were the base case for, on the
subset that has an IR counterpart and no control flow: `i32` constants, the six `i32` binary
operators that `liftI32` lifts, the ten `i32` comparisons, the `i32`-valued loads and stores, the
global accesses, and the local accesses.

## What is proved

`lower` is the lowering the Rust `tpt-wasm-ir` lowerer performs on this subset. It turns a Wasm
operand stack into SSA values: each constant, operator, comparison, load, and global read defines
a fresh value id; a store and a global write define none. Locals need no instruction at all in
SSA form. The lowering keeps a list of the value id each local holds, `local.get` pushes that id,
and `local.set` and `local.tee` replace it, so a local access is a renaming.

`runWasm` runs the Wasm body with the executable interpreter `execOp`, the same one V1 relates
to the rules, and returns the final configuration together with the memory and global events the
run performs. `IR.runBody` runs the lowered body with the IR executor.

`simulation` says: if the Wasm operand stack, the frame's locals, the store, and the IR state
correspond, the lowering succeeds, and the Wasm run returns `c'` and events `evs`, then from any
trace the IR run succeeds and appends exactly `evs`. Afterwards the Wasm operand stack holds the
lowered final stack, the Wasm store is the IR store, and the Wasm frame's locals hold the lowered
final locals.

## What is not proved

Control flow, calls, traps (a trapping run is `none`, outside the statement), and the lifted-away
operators (division, shifts, rotations). The correspondence is over `i32` values only, because
that is the type the lowering produces; the globals are assumed to be `i32`-typed.
-/

namespace TptWasm
namespace V4Straight

/-- The IR comparison operator a Wasm comparison lowers to. Every Wasm `i32` comparison has one,
so this is total. -/
def liftCmp : ICmp → IR.ICmp
  | .eq => .eq
  | .ne => .ne
  | .ltS => .ltS
  | .ltU => .ltU
  | .gtS => .gtS
  | .gtU => .gtU
  | .leS => .leS
  | .leU => .leU
  | .geS => .geS
  | .geU => .geU

/-- Lower a straight-line body. `stack` is the operand stack as value ids, top first; `locs` is the
value id each local holds; and `n` is the next fresh id. The result is the IR body, the final
stack, the final locals, and the next fresh id.

Division, shifts, and rotations have no entry: `liftI32` refuses them, so `none` is the answer.
A load or store is lowered only when its value type is `i32`, because the correspondence is
stated over `i32` values. A local access names a local that exists. -/
def lower : List Instr → List IR.ValueId → List IR.ValueId → Nat →
    Option (List IR.Instr × List IR.ValueId × List IR.ValueId × Nat)
  | [], stack, locs, n => some ([], stack, locs, n)
  | .i32Const v :: rest, stack, locs, n =>
    (lower rest (n :: stack) locs (n + 1)).map fun (body, final, fl, m) =>
      (IR.Instr.constI32 n v :: body, final, fl, m)
  | .i32Bin op :: rest, r :: l :: stack, locs, n =>
    match liftI32 op with
    | some b =>
      (lower rest (n :: stack) locs (n + 1)).map fun (body, final, fl, m) =>
        (IR.Instr.binI32 n b l r :: body, final, fl, m)
    | none => none
  | .i32Cmp op :: rest, r :: l :: stack, locs, n =>
    (lower rest (n :: stack) locs (n + 1)).map fun (body, final, fl, m) =>
      (IR.Instr.cmpI32 n (liftCmp op) l r :: body, final, fl, m)
  | .load op index arg :: rest, a :: stack, locs, n =>
    if op.resultType = .i32 then
      (lower rest (n :: stack) locs (n + 1)).map fun (body, final, fl, m) =>
        (IR.Instr.load n index a arg.offset op :: body, final, fl, m)
    else none
  | .store op index arg :: rest, v :: a :: stack, locs, n =>
    if op.operandType = .i32 then
      (lower rest stack locs n).map fun (body, final, fl, m) =>
        (IR.Instr.store index a v arg.offset op.width :: body, final, fl, m)
    else none
  | .globalGet index :: rest, stack, locs, n =>
    (lower rest (n :: stack) locs (n + 1)).map fun (body, final, fl, m) =>
      (IR.Instr.globalGet n index :: body, final, fl, m)
  | .globalSet index :: rest, v :: stack, locs, n =>
    (lower rest stack locs n).map fun (body, final, fl, m) =>
      (IR.Instr.globalSet index v :: body, final, fl, m)
  | .localGet k :: rest, stack, locs, n =>
    match locs[k]? with
    | some id => lower rest (id :: stack) locs n
    | none => none
  | .localSet k :: rest, v :: stack, locs, n =>
    if k < locs.length then lower rest stack (locs.set k v) n else none
  | .localTee k :: rest, v :: stack, locs, n =>
    if k < locs.length then lower rest (v :: stack) (locs.set k v) n else none
  | _, _, _, _ => none

/-- The memory and global events an instruction performs, read from the configuration it runs
in. A load produces `Event.load` at its effective address; a store produces `Event.store` with its
memory index; a global access produces its global event. Other instructions perform none. -/
def stepEvents : Instr → Configuration → List Event
  | .load op _ arg, c =>
    match c.operandStack with
    | .i32 a :: _ => [Event.load (effectiveAddress a arg) op.width]
    | _ => []
  | .store op index arg, c =>
    match c.operandStack with
    | _ :: .i32 a :: _ => [Event.store index (effectiveAddress a arg) op.width]
    | _ => []
  | .globalGet index, _ => [Event.globalGet index]
  | .globalSet index, _ => [Event.globalSet index]
  | _, _ => []

/-- Run a Wasm body with the executable interpreter. The result is the final configuration and
the events the run performed, in order. A step that does not produce `.step` (a trap, or a rule
that does not apply) makes the whole run `none`. -/
def runWasm : List Instr → Configuration → Option (Configuration × List Event)
  | [], c => some (c, [])
  | i :: rest, c =>
    match execOp i c with
    | some (.step c1) => (runWasm rest c1).map fun (c', evs) => (c', stepEvents i c ++ evs)
    | _ => none

/-- Splitting a run at its first step: if the body `i :: rest` runs from `c` to `c'` with events
`evs`, the first step goes to `c1` and the rest runs from `c1` with events `evs'`, and `evs` is
the first step's events followed by `evs'`. -/
theorem runWasm_cons_inv {i : Instr} {c c1 : Configuration} {rest : List Instr}
    {c' : Configuration} {evs : List Event} (hexec : execOp i c = some (.step c1))
    (hr : runWasm (i :: rest) c = some (c', evs)) :
    ∃ evs', runWasm rest c1 = some (c', evs') ∧ evs = stepEvents i c ++ evs' := by
  simp only [runWasm, hexec] at hr
  obtain ⟨p, h⟩ : ∃ p, runWasm rest c1 = p := ⟨_, rfl⟩
  rw [h] at hr
  cases p with
  | none => simp at hr
  | some q =>
    obtain ⟨c2, evs'⟩ := q
    rw [Option.map_some, Option.some.injEq] at hr
    simp only [Prod.mk.injEq] at hr
    obtain ⟨hc, he⟩ := hr
    subst hc
    exact ⟨evs', h, he.symm⟩

/-- The frame's locals and the lowering's local ids agree: the frame is the active one, its locals
are the values of the ids, and every id is below `n` and holds an `i32`. -/
def LocalsOK (c : Configuration) (s : IR.State) (locs : List IR.ValueId) (n : Nat) : Prop :=
  (∃ frame, c.activeFrame? = some frame ∧ frame.locals = locs.map s.values) ∧
    (∀ id ∈ locs, id < n) ∧
    (∀ id ∈ locs, ∃ x : Int, s.values id = .i32 x)

/-- The Wasm configuration and the IR state agree. Their operand stacks hold the same values, in
the same order, through the ids of `stack`. Every id is below `n`, the next fresh id, and every
id holds an `i32`. Their stores are the same, and the frame's locals agree with `locs`. -/
def Corresponds (c : Configuration) (s : IR.State) (stack : List IR.ValueId)
    (locs : List IR.ValueId) (n : Nat) : Prop :=
  c.operandStack = stack.map s.values ∧
    (∀ id ∈ stack, id < n) ∧
    (∀ id ∈ stack, ∃ x : Int, s.values id = .i32 x) ∧
    c.store = s.store ∧
    LocalsOK c s locs n

/-! ## Facts about the lifted operators and the state -/

/-- Where an operator lifts, the Wasm evaluation and the IR evaluation agree. -/
theorem evalI32Bin_lifts (op : IBinOp) (b : IR.BinOp) (h : liftI32 op = some b)
    (x y : Int) :
    evalI32Bin op x y = some (IR.binI32Eval b x y) := by
  cases op <;> simp [liftI32] at h <;> subst h <;> simp [evalI32Bin, IR.binI32Eval]

/-- Binding a fresh id leaves every other id as it was. -/
theorem bind_values_ne (s : IR.State) (n : Nat) (v : Value) (id : IR.ValueId) (h : id ≠ n) :
    (s.bind n v).values id = s.values id := by
  simp [IR.State.bind, h]

/-- The bound id holds the bound value. -/
theorem bind_values_self (s : IR.State) (n : Nat) (v : Value) :
    (s.bind n v).values n = v := by
  simp [IR.State.bind]

/-- Binding a value leaves the store alone. -/
theorem bind_store (s : IR.State) (n : Nat) (v : Value) : (s.bind n v).store = s.store := rfl

/-- Reading a stack whose ids are all below `n` is unchanged by binding `n`. -/
theorem map_bind_fresh (s : IR.State) (n : Nat) (v : Value) :
    ∀ (stack : List IR.ValueId), (∀ id ∈ stack, id < n) →
      stack.map (s.bind n v).values = stack.map s.values
  | [], _ => rfl
  | id :: rest, h => by
    have hid : id ≠ n := Nat.ne_of_lt (h id (by simp))
    have hrest : ∀ x ∈ rest, x < n := fun x hx => h x (by simp [hx])
    simp only [List.map_cons]
    rw [bind_values_ne s n v id hid, map_bind_fresh s n v rest hrest]

/-- The locals still correspond after binding a fresh id, when the configuration keeps its frame. -/
theorem locals_bind {c c' : Configuration} {s : IR.State} {locs : List IR.ValueId} {n : Nat}
    (hloc : LocalsOK c s locs n) (hf : c'.activeFrame? = c.activeFrame?) (v : Value) :
    LocalsOK c' (s.bind n v) locs (n + 1) := by
  obtain ⟨⟨frame, hframe, hflocals⟩, hlbound, hltyped⟩ := hloc
  refine ⟨⟨frame, hf.trans hframe, ?_⟩, fun id hid => Nat.lt_succ_of_lt (hlbound id hid), ?_⟩
  · rw [hflocals, map_bind_fresh s n v locs hlbound]
  · intro id hid
    obtain ⟨x, hx⟩ := hltyped id hid
    exact ⟨x, by rw [bind_values_ne s n v id (Nat.ne_of_lt (hlbound id hid))]; exact hx⟩

/-- A load's result, for an `i32`-valued load, is an `i32`. -/
theorem loadedValue_i32 (op : LoadOp) (raw : Nat) (h : op.resultType = .i32) :
    ∃ y : Int, loadedValue op raw = .i32 y := by
  cases op <;> simp_all [loadedValue, LoadOp.resultType]

/-- One `constI32` step of the IR executor: it binds its result and nothing else. -/
theorem runBody_const (r : IR.ValueId) (v : Int) (body : List IR.Instr) (s : IR.State)
    (t : List Event) :
    IR.runBody (IR.Instr.constI32 r v :: body) s t =
      IR.runBody body (s.bind r (.i32 v)) t := by
  simp [IR.runBody, IR.execInstr]

/-- One `binI32` step of the IR executor, when its operands are `i32`s. -/
theorem runBody_bin (r : IR.ValueId) (b : IR.BinOp) (l rr : IR.ValueId) (body : List IR.Instr)
    (s : IR.State) (t : List Event) (x y : Int) (hl : s.values l = .i32 x)
    (hr : s.values rr = .i32 y) :
    IR.runBody (IR.Instr.binI32 r b l rr :: body) s t =
      IR.runBody body (s.bind r (.i32 (IR.binI32Eval b x y))) t := by
  simp [IR.runBody, IR.execInstr, IR.intPayload, hl, hr]

/-- The IR comparison and the Wasm comparison produce the same `i32`, for every operator. This
is the agreement the comparison case of the simulation rests on: the IR executor's `cmpI32Eval`
and the Wasm model's `evalCmpBits 32` both read the operands as signed or as unsigned 32-bit
patterns, and yield `1` or `0`. -/
theorem cmpI32Eval_agrees (op : ICmp) (x y : Int) :
    IR.cmpI32Eval (liftCmp op) x y = .i32 (evalCmpBits 32 op x y) := by
  cases op <;> simp [liftCmp, IR.cmpI32Eval, evalCmpBits, IR.boolValue, ultBits] <;>
    split <;> simp_all

/-- One `cmpI32` step of the IR executor, when its operands are `i32`s. -/
theorem runBody_cmp (r : IR.ValueId) (op : ICmp) (l rr : IR.ValueId) (body : List IR.Instr)
    (s : IR.State) (t : List Event) (x y : Int) (hl : s.values l = .i32 x)
    (hr : s.values rr = .i32 y) :
    IR.runBody (IR.Instr.cmpI32 r (liftCmp op) l rr :: body) s t =
      IR.runBody body (s.bind r (.i32 (evalCmpBits 32 op x y))) t := by
  simp [IR.runBody, IR.execInstr, IR.intPayload, hl, hr, cmpI32Eval_agrees]

/-- One `load` step of the IR executor: it reads the bytes at the effective address, binds the
converted result, and appends its load event. -/
theorem runBody_load (r : IR.ValueId) (index : Nat) (a : IR.ValueId) (offset : Nat)
    (op : LoadOp) (body : List IR.Instr) (s : IR.State) (t : List Event) (x : Int)
    (m : MemoryState) (raw : Nat) (ha : s.values a = .i32 x)
    (hm : s.store.memory? index = some m)
    (hread : m.readLE? (effectiveAddress x { offset := offset }) op.width = some raw) :
    IR.runBody (IR.Instr.load r index a offset op :: body) s t =
      IR.runBody body (s.bind r (loadedValue op raw)) (t ++ [Event.load
        (effectiveAddress x { offset := offset }) op.width]) := by
  simp [IR.runBody, IR.execInstr, IR.intPayload, ha, hm, hread]

/-- One `store` step of the IR executor: it writes the operand's bytes at the effective address
and changes only the store. -/
theorem runBody_store (index : Nat) (a v : IR.ValueId) (offset width : Nat) (body : List IR.Instr)
    (s : IR.State) (t : List Event) (x : Int) (operand : Value) (m updated : MemoryState)
    (ha : s.values a = .i32 x) (hv : s.values v = operand)
    (hm : s.store.memory? index = some m)
    (hwrite : m.writeLE? (effectiveAddress x { offset := offset }) width (storedBits operand)
      = some updated) :
    IR.runBody (IR.Instr.store index a v offset width :: body) s t =
      IR.runBody body { s with store := s.store.setMemory index updated } (t ++ [Event.store index
        (effectiveAddress x { offset := offset }) width]) := by
  simp [IR.runBody, IR.execInstr, IR.intPayload, ha, hv, hm, hwrite]

/-- An entry that `Store.get?` returns is a member of the list. -/
theorem get_mem {α : Type} : ∀ (l : List α) (i : Nat) (x : α), Store.get? l i = some x → x ∈ l
  | [], _, _, h => by simp [Store.get?] at h
  | y :: ys, 0, x, h => by
    have hyx : y = x := Option.some.inj h
    simp [hyx]
  | y :: ys, i + 1, x, h => by
    have h' : Store.get? ys i = some x := h
    exact List.mem_cons_of_mem _ (get_mem ys i x h')

/-- An entry of `Store.set` is either an entry of the original list or the value written. -/
theorem set_mem {α : Type} : ∀ (l : List α) (i : Nat) (x y : α),
    y ∈ Store.set l i x → y ∈ l ∨ y = x
  | [], _, _, _, h => by simp [Store.set] at h
  | z :: zs, 0, x, y, h => by
    have h' : y ∈ x :: zs := h
    rcases List.mem_cons.mp h' with rfl | hz
    · exact Or.inr rfl
    · exact Or.inl (List.mem_cons_of_mem _ hz)
  | z :: zs, i + 1, x, y, h => by
    have h' : y ∈ z :: Store.set zs i x := h
    rcases List.mem_cons.mp h' with rfl | hz
    · exact Or.inl (by simp)
    · rcases set_mem zs i x y hz with h2 | h2
      · exact Or.inl (List.mem_cons_of_mem _ h2)
      · exact Or.inr h2

/-- A value whose type is `i32` is an `i32`. -/
theorem value_i32_of_type (v : Value) (h : v.type = .i32) : ∃ x : Int, v = .i32 x := by
  cases v with
  | i32 x => exact ⟨x, rfl⟩
  | i64 _ => simp [Value.type] at h
  | f32 _ => simp [Value.type] at h
  | f64 _ => simp [Value.type] at h

/-- One `globalGet` step of the IR executor: it binds the global's value and appends its event. -/
theorem runBody_globalGet (r : IR.ValueId) (index : Nat) (body : List IR.Instr) (s : IR.State)
    (t : List Event) (g : GlobalState) (hg : s.store.global? index = some g) :
    IR.runBody (IR.Instr.globalGet r index :: body) s t =
      IR.runBody body (s.bind r g.value) (t ++ [Event.globalGet index]) := by
  simp [IR.runBody, IR.execInstr, hg]

/-- One `globalSet` step of the IR executor: it writes the operand into the global and appends its
event. -/
theorem runBody_globalSet (index : Nat) (v : IR.ValueId) (body : List IR.Instr) (s : IR.State)
    (t : List Event) (g : GlobalState) (hg : s.store.global? index = some g) :
    IR.runBody (IR.Instr.globalSet index v :: body) s t =
      IR.runBody body { s with store := s.store.setGlobal index { g with value := s.values v } }
        (t ++ [Event.globalSet index]) := by
  simp [IR.runBody, IR.execInstr, hg]

/-! ## Facts about the frame's locals -/

/-- `Store.get?` is the list index. -/
theorem get?_eq_getElem? {α : Type} : ∀ (l : List α) (k : Nat), Store.get? l k = l[k]?
  | [], _ => rfl
  | _ :: _, 0 => rfl
  | _ :: xs, k + 1 => get?_eq_getElem? xs k

/-- Replacing an index inside the list is `List.set`, when the index is in range. -/
theorem replaceAt?_eq_set {α : Type} : ∀ (xs : List α) (k : Nat) (v : α), k < xs.length →
    Configuration.replaceAt? xs k v = some (xs.set k v)
  | [], _, _, h => absurd h (by simp)
  | _ :: _, 0, _, _ => rfl
  | x :: xs, k + 1, v, h => by
    have h' : k < xs.length := by simpa using h
    simp only [Configuration.replaceAt?, replaceAt?_eq_set xs k v h', Option.map_some,
      List.set]

/-- Setting a local in range replaces that entry of the frame's locals. -/
theorem setLocal?_eq (frame : Frame) (k : Nat) (v : Value) (h : k < frame.locals.length) :
    Configuration.setLocal? frame k v = some { frame with locals := frame.locals.set k v } := by
  simp [Configuration.setLocal?, replaceAt?_eq_set _ _ _ h]

/-- The last element of a nonempty list, with its last element replaced, is the new last element. -/
theorem last_dropLast_snoc {α : Type} : ∀ (xs : List α) (f : α), xs ≠ [] →
    Configuration.last? (xs.dropLast ++ [f]) = some f
  | [], _, h => absurd rfl h
  | [_], f, _ => by simp [List.dropLast, Configuration.last?]
  | x :: y :: ys, f, _ => by
    have ih := last_dropLast_snoc (y :: ys) f (by simp)
    have hsplit : (x :: y :: ys).dropLast ++ [f] = x :: ((y :: ys).dropLast ++ [f]) := by
      simp [List.dropLast]
    have hne : (y :: ys).dropLast ++ [f] ≠ [] := by simp
    rw [hsplit]
    cases hl : (y :: ys).dropLast ++ [f] with
    | nil => exact absurd hl hne
    | cons b bs =>
      rw [hl] at ih
      exact ih

/-- Replacing the active frame keeps it the active frame, now holding the replacement. -/
theorem activeFrame_replace (c : Configuration) (frame f' : Frame)
    (h : c.activeFrame? = some frame) :
    ({ c with frames := c.frames.dropLast ++ [f'] } : Configuration).activeFrame? = some f' := by
  have hne : c.frames ≠ [] := by
    intro hn
    simp [Configuration.activeFrame?, hn, Configuration.last?] at h
  exact last_dropLast_snoc c.frames f' hne

/-! ## The simulation -/

/-- The straight-line simulation.

For every Wasm body, Wasm configuration, IR state, and correspondence: if the lowering succeeds,
the globals are `i32`-typed, and the Wasm run returns `c'` and the events `evs`, then from any
trace the IR run succeeds and appends exactly `evs`; the Wasm operand stack afterwards holds the
lowered final stack; the Wasm store afterwards is the IR store; and the Wasm frame's locals
afterwards hold the lowered final locals. -/
theorem simulation (body : List Instr) :
    ∀ (c c' : Configuration) (evs : List Event) (s : IR.State) (stack locs : List IR.ValueId)
      (n : Nat) (ir : List IR.Instr) (final finalLocs : List IR.ValueId) (m : Nat),
      Corresponds c s stack locs n →
      (∀ g ∈ c.store.globals, g.ty = .i32 ∧ g.value.type = .i32) →
      lower body stack locs n = some (ir, final, finalLocs, m) →
      runWasm body c = some (c', evs) →
      ∀ (trace : List Event), ∃ (s' : IR.State),
        IR.runBody ir s trace = some (s', trace ++ evs) ∧
        Corresponds c' s' final finalLocs m := by
  induction body with
  | nil =>
    intro c c' evs s stack locs n ir final finalLocs m hc hgl hl hr trace
    simp only [lower, Option.some.injEq, Prod.mk.injEq] at hl
    obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
    simp only [runWasm, Option.some.injEq, Prod.mk.injEq] at hr
    obtain ⟨rfl, rfl⟩ := hr
    exact ⟨s, by simp [IR.runBody], hc⟩
  | cons i rest ih =>
    intro c c' evs s stack locs n ir final finalLocs m hc hgl hl hr trace
    cases i with
    | i32Const v =>
      cases hres : lower rest (n :: stack) locs (n + 1) with
      | none => simp [lower, hres] at hl
      | some res =>
        obtain ⟨body', final', fl', m'⟩ := res
        simp only [lower, hres, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hl
        obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
        have hexec : execOp (.i32Const v) c = some (.step (c.push (.i32 v))) := rfl
        obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
        have hev : stepEvents (.i32Const v) c = [] := rfl
        obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
        have hc' : Corresponds (c.push (.i32 v)) (s.bind n (.i32 v)) (n :: stack) locs (n + 1) := by
          refine ⟨?_, ?_, ?_, ?_, ?_⟩
          · simp only [Configuration.push, List.map_cons, bind_values_self]
            rw [hstack, map_bind_fresh s n (.i32 v) stack hbound]
          · intro id hid
            rcases List.mem_cons.mp hid with rfl | hid
            · exact Nat.lt_succ_self _
            · exact Nat.lt_succ_of_lt (hbound id hid)
          · intro id hid
            rcases List.mem_cons.mp hid with rfl | hid
            · exact ⟨v, bind_values_self s id (.i32 v)⟩
            · obtain ⟨x, hx⟩ := htyped id hid
              exact ⟨x, by rw [bind_values_ne s n (.i32 v) id (Nat.ne_of_lt (hbound id hid))]; exact hx⟩
          · simp [Configuration.push, bind_store, hstore]
          · exact locals_bind hloc rfl (.i32 v)
        obtain ⟨s', hrun, hcorr⟩ := ih _ c' _ (s.bind n (.i32 v)) (n :: stack)
          locs (n + 1) body' final' fl' m' hc' hgl hres hrest trace
        refine ⟨s', ?_, hcorr⟩
        rw [runBody_const, hevs, hev, List.nil_append]
        exact hrun
    | i32Bin op =>
      rcases stack with _ | ⟨r, _ | ⟨l, stack0⟩⟩
      · simp [lower] at hl
      · simp [lower] at hl
      cases hop : liftI32 op with
      | none => simp [lower, hop] at hl
      | some b =>
        cases hres : lower rest (n :: stack0) locs (n + 1) with
        | none => simp [lower, hop, hres] at hl
        | some res =>
          obtain ⟨body', final', fl', m'⟩ := res
          simp only [lower, hop, hres, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hl
          obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
          obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
          obtain ⟨xl, hxl⟩ := htyped l (by simp)
          obtain ⟨xr, hxr⟩ := htyped r (by simp)
          have hshape : c.operandStack = .i32 xr :: .i32 xl :: stack0.map s.values := by
            rw [hstack]
            simp [hxl, hxr]
          have hexec : execOp (.i32Bin op) c =
              some (.step { c with operandStack :=
                (.i32 (IR.binI32Eval b xl xr)) :: stack0.map s.values }) := by
            simp [execOp, hshape, evalI32Bin_lifts op b hop]
          obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
          have hev : stepEvents (.i32Bin op) c = [] := by
            simp [stepEvents]
          have hc' : Corresponds
              { c with operandStack := (.i32 (IR.binI32Eval b xl xr)) :: stack0.map s.values }
              (s.bind n (.i32 (IR.binI32Eval b xl xr))) (n :: stack0) locs (n + 1) := by
            refine ⟨?_, ?_, ?_, ?_, ?_⟩
            · simp only [List.map_cons, bind_values_self]
              rw [map_bind_fresh s n _ stack0 (fun id h => hbound id (by simp [h]))]
            · intro id hid
              rcases List.mem_cons.mp hid with rfl | hid
              · exact Nat.lt_succ_self _
              · exact Nat.lt_succ_of_lt (hbound id (by simp [hid]))
            · intro id hid
              rcases List.mem_cons.mp hid with rfl | hid
              · exact ⟨_, bind_values_self s id _⟩
              · obtain ⟨x, hx⟩ := htyped id (by simp [hid])
                exact ⟨x, by
                  rw [bind_values_ne s n _ id (Nat.ne_of_lt (hbound id (by simp [hid])))]
                  exact hx⟩
            · simp [bind_store, hstore]
            · exact locals_bind hloc rfl _
          obtain ⟨s', hrun, hcorr⟩ := ih _ c'
            evs' (s.bind n (.i32 (IR.binI32Eval b xl xr))) (n :: stack0) locs (n + 1) body'
            final' fl' m' hc' hgl hres hrest trace
          refine ⟨s', ?_, hcorr⟩
          rw [runBody_bin n b l r body' s trace xl xr hxl hxr, hevs, hev, List.nil_append]
          exact hrun
    | i32Cmp op =>
      rcases stack with _ | ⟨r, _ | ⟨l, stack0⟩⟩
      · simp [lower] at hl
      · simp [lower] at hl
      cases hres : lower rest (n :: stack0) locs (n + 1) with
      | none => simp [lower, hres] at hl
      | some res =>
        obtain ⟨body', final', fl', m'⟩ := res
        simp only [lower, hres, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hl
        obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
        obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
        obtain ⟨xl, hxl⟩ := htyped l (by simp)
        obtain ⟨xr, hxr⟩ := htyped r (by simp)
        have hshape : c.operandStack = .i32 xr :: .i32 xl :: stack0.map s.values := by
          rw [hstack]
          simp [hxl, hxr]
        have hexec : execOp (.i32Cmp op) c =
            some (.step { c with operandStack :=
              (.i32 (evalCmpBits 32 op xl xr)) :: stack0.map s.values }) := by
          simp [execOp, hshape, evalI32Cmp]
        obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
        have hev : stepEvents (.i32Cmp op) c = [] := by
          simp [stepEvents]
        have hc' : Corresponds
            { c with operandStack := (.i32 (evalCmpBits 32 op xl xr)) :: stack0.map s.values }
            (s.bind n (.i32 (evalCmpBits 32 op xl xr))) (n :: stack0) locs (n + 1) := by
          refine ⟨?_, ?_, ?_, ?_, ?_⟩
          · simp only [List.map_cons, bind_values_self]
            rw [map_bind_fresh s n _ stack0 (fun id h => hbound id (by simp [h]))]
          · intro id hid
            rcases List.mem_cons.mp hid with rfl | hid
            · exact Nat.lt_succ_self _
            · exact Nat.lt_succ_of_lt (hbound id (by simp [hid]))
          · intro id hid
            rcases List.mem_cons.mp hid with rfl | hid
            · exact ⟨_, bind_values_self s id _⟩
            · obtain ⟨x, hx⟩ := htyped id (by simp [hid])
              exact ⟨x, by
                rw [bind_values_ne s n _ id (Nat.ne_of_lt (hbound id (by simp [hid])))]
                exact hx⟩
          · simp [bind_store, hstore]
          · exact locals_bind hloc rfl _
        obtain ⟨s', hrun, hcorr⟩ := ih _ c'
          evs' (s.bind n (.i32 (evalCmpBits 32 op xl xr))) (n :: stack0) locs (n + 1) body'
          final' fl' m' hc' hgl hres hrest trace
        refine ⟨s', ?_, hcorr⟩
        rw [runBody_cmp n op l r body' s trace xl xr hxl hxr, hevs, hev, List.nil_append]
        exact hrun
    | load op index arg =>
      rcases stack with _ | ⟨a, stack0⟩
      · simp [lower] at hl
      have hop : op.resultType = .i32 := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      cases hres : lower rest (n :: stack0) locs (n + 1) with
      | none => simp [lower, hop, hres] at hl
      | some res =>
        obtain ⟨body', final', fl', m'⟩ := res
        simp only [lower, hop, hres, ↓reduceIte, Option.map_some, Option.some.injEq,
          Prod.mk.injEq] at hl
        obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
        obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
        obtain ⟨x, hx⟩ := htyped a (by simp)
        have hshape : c.operandStack = .i32 x :: stack0.map s.values := by
          rw [hstack]
          simp [hx]
        cases hm : c.store.memory? index with
        | none => simp [runWasm, execOp, hshape, hm] at hr
        | some mem =>
          cases hread : mem.readLE? (effectiveAddress x arg) op.width with
          | none => simp [runWasm, execOp, hshape, hm, hread] at hr
          | some raw =>
            have hexec : execOp (.load op index arg) c =
                some (.step { c with operandStack := loadedValue op raw :: stack0.map s.values }) := by
              simp [execOp, hshape, hm, hread]
            obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
            have hev : stepEvents (.load op index arg) c =
                [Event.load (effectiveAddress x arg) op.width] := by
              simp [stepEvents, hshape]
            obtain ⟨y, hy⟩ := loadedValue_i32 op raw hop
            have hc' : Corresponds
                { c with operandStack := loadedValue op raw :: stack0.map s.values }
                (s.bind n (loadedValue op raw)) (n :: stack0) locs (n + 1) := by
              refine ⟨?_, ?_, ?_, ?_, ?_⟩
              · simp only [List.map_cons, bind_values_self]
                rw [map_bind_fresh s n _ stack0 (fun id h => hbound id (by simp [h]))]
              · intro id hid
                rcases List.mem_cons.mp hid with rfl | hid
                · exact Nat.lt_succ_self _
                · exact Nat.lt_succ_of_lt (hbound id (by simp [hid]))
              · intro id hid
                rcases List.mem_cons.mp hid with rfl | hid
                · exact ⟨y, by rw [bind_values_self, hy]⟩
                · obtain ⟨x', hx'⟩ := htyped id (by simp [hid])
                  exact ⟨x', by
                    rw [bind_values_ne s n _ id (Nat.ne_of_lt (hbound id (by simp [hid])))]
                    exact hx'⟩
              · simp [bind_store, hstore]
              · exact locals_bind hloc rfl _
            have harg : ({ offset := arg.offset } : MemArg) = arg := by cases arg; rfl
            obtain ⟨s', hrun, hcorr⟩ := ih _ c'
              evs' (s.bind n (loadedValue op raw)) (n :: stack0) locs (n + 1) body' final' fl' m'
              hc' hgl hres hrest (trace ++ [Event.load (effectiveAddress x arg) op.width])
            refine ⟨s', ?_, hcorr⟩
            rw [runBody_load n index a arg.offset op body' s trace x mem raw hx
              (by rw [hstore] at hm; exact hm) (by rw [harg]; exact hread), hevs, hev, harg]
            simpa [List.append_assoc] using hrun
    | store op index arg =>
      rcases stack with _ | ⟨v, _ | ⟨a, stack0⟩⟩
      · simp [lower] at hl
      · simp [lower] at hl
      have hop : op.operandType = .i32 := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      cases hres : lower rest stack0 locs n with
      | none => simp [lower, hop, hres] at hl
      | some res =>
        obtain ⟨body', final', fl', m'⟩ := res
        simp only [lower, hop, hres, ↓reduceIte, Option.map_some, Option.some.injEq,
          Prod.mk.injEq] at hl
        obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
        obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
        obtain ⟨xv, hxv⟩ := htyped v (by simp)
        obtain ⟨xa, hxa⟩ := htyped a (by simp)
        have hshape : c.operandStack = .i32 xv :: .i32 xa :: stack0.map s.values := by
          rw [hstack]
          simp [hxv, hxa]
        cases hm : c.store.memory? index with
        | none => simp [runWasm, execOp, hshape, hm] at hr
        | some mem =>
          have htype : (Value.i32 xv).type = op.operandType := by
            refine Classical.byContradiction fun hne => ?_
            simp [runWasm, execOp, hshape, hm, hne] at hr
          cases hwrite : mem.writeLE? (effectiveAddress xa arg) op.width
              (storedBits (.i32 xv)) with
          | none => simp [runWasm, execOp, hshape, hm, htype, hwrite] at hr
          | some updated =>
            have hexec : execOp (.store op index arg) c =
                some (.step { c with operandStack := stack0.map s.values, store := c.store.setMemory index updated }) := by
              simp [execOp, hshape, hm, htype, hwrite]
            obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
            have hev : stepEvents (.store op index arg) c =
                [Event.store index (effectiveAddress xa arg) op.width] := by
              simp [stepEvents, hshape]
            have hc' : Corresponds
                { c with operandStack := stack0.map s.values, store := c.store.setMemory index updated }
                { s with store := s.store.setMemory index updated } stack0 locs n := by
              refine ⟨rfl, ?_, ?_, ?_, ?_⟩
              · exact fun id hid => hbound id (by simp [hid])
              · intro id hid
                obtain ⟨x, hx⟩ := htyped id (by simp [hid])
                exact ⟨x, hx⟩
              · simp [hstore]
              · exact hloc
            have harg : ({ offset := arg.offset } : MemArg) = arg := by cases arg; rfl
            obtain ⟨s', hrun, hcorr⟩ := ih _ c'
              evs' { s with store := s.store.setMemory index updated } stack0 locs n body' final'
              fl' m' hc' hgl hres hrest (trace ++ [Event.store index (effectiveAddress xa arg) op.width])
            refine ⟨s', ?_, hcorr⟩
            rw [runBody_store index a v arg.offset op.width body' s trace xa (.i32 xv) mem updated
              hxa hxv (by rw [hstore] at hm; exact hm) (by rw [harg]; exact hwrite), hevs, hev, harg]
            simpa [List.append_assoc] using hrun
    | globalGet index =>
      cases hres : lower rest (n :: stack) locs (n + 1) with
      | none => simp [lower, hres] at hl
      | some res =>
        obtain ⟨body', final', fl', m'⟩ := res
        simp only [lower, hres, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hl
        obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
        obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
        cases hg : c.store.global? index with
        | none => simp [runWasm, execOp, hg] at hr
        | some g =>
          have hexec : execOp (.globalGet index) c = some (.step (c.push g.value)) := by
            simp [execOp, hg]
          obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
          have hev : stepEvents (.globalGet index) c = [Event.globalGet index] := rfl
          have hgmem : g ∈ c.store.globals := get_mem _ _ _ hg
          obtain ⟨-, hgv⟩ := hgl g hgmem
          obtain ⟨x, hx⟩ := value_i32_of_type g.value hgv
          have hc' : Corresponds (c.push g.value) (s.bind n g.value) (n :: stack) locs (n + 1) := by
            refine ⟨?_, ?_, ?_, ?_, ?_⟩
            · simp only [Configuration.push, List.map_cons, bind_values_self]
              rw [hstack, map_bind_fresh s n g.value stack hbound]
            · intro id hid
              rcases List.mem_cons.mp hid with rfl | hid
              · exact Nat.lt_succ_self _
              · exact Nat.lt_succ_of_lt (hbound id hid)
            · intro id hid
              rcases List.mem_cons.mp hid with rfl | hid
              · exact ⟨x, by rw [bind_values_self, hx]⟩
              · obtain ⟨y, hy⟩ := htyped id hid
                exact ⟨y, by rw [bind_values_ne s n _ id (Nat.ne_of_lt (hbound id hid))]; exact hy⟩
            · simp [Configuration.push, bind_store, hstore]
            · exact locals_bind hloc rfl _
          have hsg : s.store.global? index = some g := by rw [← hstore]; exact hg
          obtain ⟨s', hrun, hcorr⟩ := ih _ c' _ (s.bind n g.value) (n :: stack)
            locs (n + 1) body' final' fl' m' hc' hgl hres hrest (trace ++ [Event.globalGet index])
          refine ⟨s', ?_, hcorr⟩
          rw [runBody_globalGet n index body' s trace g hsg, hevs, hev]
          simpa [List.append_assoc] using hrun
    | globalSet index =>
      rcases stack with _ | ⟨v, stack0⟩
      · simp [lower] at hl
      cases hres : lower rest stack0 locs n with
      | none => simp [lower, hres] at hl
      | some res =>
        obtain ⟨body', final', fl', m'⟩ := res
        simp only [lower, hres, Option.map_some, Option.some.injEq, Prod.mk.injEq] at hl
        obtain ⟨rfl, rfl, rfl, rfl⟩ := hl
        obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
        obtain ⟨xv, hxv⟩ := htyped v (by simp)
        have hshape : c.operandStack = .i32 xv :: stack0.map s.values := by
          rw [hstack]
          simp [hxv]
        cases hg : c.store.global? index with
        | none => simp [runWasm, execOp, hshape, hg] at hr
        | some g =>
          have hgmem : g ∈ c.store.globals := get_mem _ _ _ hg
          obtain ⟨hgty, -⟩ := hgl g hgmem
          have hexec : execOp (.globalSet index) c =
              some (.step { c with operandStack := stack0.map s.values, store := c.store.setGlobal index { g with value := .i32 xv } }) := by
            simp [execOp, hshape, hg, hgty, Value.type]
          obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
          have hev : stepEvents (.globalSet index) c = [Event.globalSet index] := rfl
          have hsg : s.store.global? index = some g := by rw [← hstore]; exact hg
          have hgl' : ∀ g' ∈ (c.store.setGlobal index { g with value := .i32 xv }).globals,
              g'.ty = .i32 ∧ g'.value.type = .i32 := by
            intro g' hg'
            rcases set_mem _ _ _ _ hg' with h2 | h2
            · exact hgl g' h2
            · subst h2
              exact ⟨hgty, rfl⟩
          have hc' : Corresponds
              { c with operandStack := stack0.map s.values, store := c.store.setGlobal index { g with value := .i32 xv } }
              { s with store := s.store.setGlobal index { g with value := s.values v } }
              stack0 locs n := by
            refine ⟨rfl, ?_, ?_, ?_, ?_⟩
            · exact fun id hid => hbound id (by simp [hid])
            · intro id hid
              obtain ⟨y, hy⟩ := htyped id (by simp [hid])
              exact ⟨y, hy⟩
            · simp [hstore, hxv]
            · exact hloc
          obtain ⟨s', hrun, hcorr⟩ := ih _ c' _
            { s with store := s.store.setGlobal index { g with value := s.values v } } stack0 locs n
            body' final' fl' m' hc' hgl' hres hrest (trace ++ [Event.globalSet index])
          refine ⟨s', ?_, hcorr⟩
          rw [runBody_globalSet index v body' s trace g hsg, hevs, hev]
          simpa [List.append_assoc] using hrun
    | localGet k =>
      cases hk : locs[k]? with
      | none => simp [lower, hk] at hl
      | some id =>
        have hl' : lower rest (id :: stack) locs n = some (ir, final, finalLocs, m) := by
          simpa [lower, hk] using hl
        obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
        obtain ⟨⟨frame, hframe, hflocals⟩, hlbound, hltyped⟩ := hloc
        have hidmem : id ∈ locs := List.mem_of_getElem? hk
        have hget : Store.get? frame.locals k = some (s.values id) := by
          rw [hflocals, get?_eq_getElem?, List.getElem?_map, hk]
          rfl
        have hexec : execOp (.localGet k) c = some (.step (c.push (s.values id))) := by
          simp [execOp, hframe, hget]
        obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
        have hev : stepEvents (.localGet k) c = [] := rfl
        obtain ⟨x, hx⟩ := hltyped id hidmem
        have hc' : Corresponds (c.push (s.values id)) s (id :: stack) locs n := by
          refine ⟨?_, ?_, ?_, ?_, ?_⟩
          · simp [Configuration.push, hstack]
          · intro y hy
            rcases List.mem_cons.mp hy with rfl | hy
            · exact hlbound y hidmem
            · exact hbound y hy
          · intro y hy
            rcases List.mem_cons.mp hy with rfl | hy
            · exact ⟨x, hx⟩
            · exact htyped y hy
          · simp [Configuration.push, hstore]
          · exact ⟨⟨frame, hframe, hflocals⟩, hlbound, hltyped⟩
        obtain ⟨s', hrun, hcorr⟩ := ih _ c' evs' s (id :: stack)
          locs n ir final finalLocs m hc' hgl hl' hrest trace
        refine ⟨s', ?_, hcorr⟩
        rw [hevs, hev, List.nil_append]
        exact hrun
    | localSet k =>
      rcases stack with _ | ⟨v, stack0⟩
      · simp [lower] at hl
      have hk : k < locs.length := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      have hl' : lower rest stack0 (locs.set k v) n = some (ir, final, finalLocs, m) := by
        simpa [lower, hk] using hl
      obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
      obtain ⟨xv, hxv⟩ := htyped v (by simp)
      have hshape : c.operandStack = .i32 xv :: stack0.map s.values := by
        rw [hstack]
        simp [hxv]
      obtain ⟨⟨frame, hframe, hflocals⟩, hlbound, hltyped⟩ := hloc
      have hlen : k < frame.locals.length := by
        rw [hflocals]
        simpa using hk
      have hexec : execOp (.localSet k) c = some (.step { c with operandStack := stack0.map s.values, frames := c.frames.dropLast ++ [{ frame with locals := frame.locals.set k (.i32 xv) }] }) := by
        simp [execOp, hshape, hframe, setLocal?_eq _ _ _ hlen]
      obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
      have hev : stepEvents (.localSet k) c = [] := rfl
      have hact := activeFrame_replace c frame { frame with locals := frame.locals.set k (.i32 xv) } hframe
      have hc' : Corresponds
          { c with operandStack := stack0.map s.values, frames := c.frames.dropLast ++ [{ frame with locals := frame.locals.set k (.i32 xv) }] }
          s stack0 (locs.set k v) n := by
        refine ⟨rfl, ?_, ?_, ?_, ?_⟩
        · exact fun id hid => hbound id (by simp [hid])
        · intro id hid
          obtain ⟨y, hy⟩ := htyped id (by simp [hid])
          exact ⟨y, hy⟩
        · simp [hstore]
        · refine ⟨⟨_, hact, ?_⟩, ?_, ?_⟩
          · show frame.locals.set k (.i32 xv) = (locs.set k v).map s.values
            rw [hflocals, List.map_set, hxv]
          · intro id hid
            rcases List.mem_or_eq_of_mem_set hid with h2 | h2
            · exact hlbound id h2
            · subst h2
              exact hbound id (by simp)
          · intro id hid
            rcases List.mem_or_eq_of_mem_set hid with h2 | h2
            · exact hltyped id h2
            · subst h2
              exact ⟨xv, hxv⟩
      obtain ⟨s', hrun, hcorr⟩ := ih _ _ evs' s stack0 (locs.set k v) n ir final
        finalLocs m hc' hgl hl' hrest trace
      refine ⟨s', ?_, hcorr⟩
      rw [hevs, hev, List.nil_append]
      exact hrun
    | localTee k =>
      rcases stack with _ | ⟨v, stack0⟩
      · simp [lower] at hl
      have hk : k < locs.length := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      have hl' : lower rest (v :: stack0) (locs.set k v) n = some (ir, final, finalLocs, m) := by
        simpa [lower, hk] using hl
      obtain ⟨hstack, hbound, htyped, hstore, hloc⟩ := hc
      obtain ⟨xv, hxv⟩ := htyped v (by simp)
      have hshape : c.operandStack = .i32 xv :: stack0.map s.values := by
        rw [hstack]
        simp [hxv]
      obtain ⟨⟨frame, hframe, hflocals⟩, hlbound, hltyped⟩ := hloc
      have hlen : k < frame.locals.length := by
        rw [hflocals]
        simpa using hk
      have hexec : execOp (.localTee k) c = some (.step { c with
          frames := c.frames.dropLast ++ [{ frame with locals := frame.locals.set k (.i32 xv) }] }) := by
        simp [execOp, hshape, hframe, setLocal?_eq _ _ _ hlen]
      obtain ⟨evs', hrest, hevs⟩ := runWasm_cons_inv hexec hr
      have hev : stepEvents (.localTee k) c = [] := rfl
      have hact := activeFrame_replace c frame { frame with locals := frame.locals.set k (.i32 xv) } hframe
      have hc' : Corresponds
          { c with frames := c.frames.dropLast ++ [{ frame with locals := frame.locals.set k (.i32 xv) }] }
          s (v :: stack0) (locs.set k v) n := by
        refine ⟨?_, ?_, ?_, ?_, ?_⟩
        · simp [hshape, hxv]
        · intro id hid
          rcases List.mem_cons.mp hid with rfl | hid
          · exact hbound id (by simp)
          · exact hbound id (by simp [hid])
        · intro id hid
          rcases List.mem_cons.mp hid with rfl | hid
          · exact ⟨xv, hxv⟩
          · obtain ⟨y, hy⟩ := htyped id (by simp [hid])
            exact ⟨y, hy⟩
        · simp [hstore]
        · refine ⟨⟨_, hact, ?_⟩, ?_, ?_⟩
          · show frame.locals.set k (.i32 xv) = (locs.set k v).map s.values
            rw [hflocals, List.map_set, hxv]
          · intro id hid
            rcases List.mem_or_eq_of_mem_set hid with h2 | h2
            · exact hlbound id h2
            · subst h2
              exact hbound id (by simp)
          · intro id hid
            rcases List.mem_or_eq_of_mem_set hid with h2 | h2
            · exact hltyped id h2
            · subst h2
              exact ⟨xv, hxv⟩
      obtain ⟨s', hrun, hcorr⟩ := ih _ _ evs' s (v :: stack0) (locs.set k v) n ir
        final finalLocs m hc' hgl hl' hrest trace
      refine ⟨s', ?_, hcorr⟩
      rw [hevs, hev, List.nil_append]
      exact hrun
    | _ => simp [lower] at hl


/-! ## Trap position -/

theorem lower_append (pre q : List Instr) :
    ∀ (stack locs : List IR.ValueId) (n : Nat) (ir : List IR.Instr) (final fl : List IR.ValueId)
      (m : Nat), lower (pre ++ q) stack locs n = some (ir, final, fl, m) →
      ∃ (b1 : List IR.Instr) (st1 l1 : List IR.ValueId) (k1 : Nat) (b2 : List IR.Instr),
        lower pre stack locs n = some (b1, st1, l1, k1) ∧
        lower q st1 l1 k1 = some (b2, final, fl, m) ∧ ir = b1 ++ b2 := by
  induction pre with
  | nil =>
    intro stack locs n ir final fl m hl
    exact ⟨[], stack, locs, n, ir, by simp [lower], by simpa [lower] using hl, by simp⟩
  | cons p pre' ih =>
    intro stack locs n ir final fl m hl
    cases p with
    | i32Const v =>
      simp only [List.cons_append, lower, Option.map_eq_some_iff] at hl
      obtain ⟨⟨body, final', fl', m'⟩, hres, heq⟩ := hl
      simp only [Prod.mk.injEq] at heq
      obtain ⟨rfl, rfl, rfl, rfl⟩ := heq
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hres
      exact ⟨IR.Instr.constI32 n v :: b1, st1, l1, k1, b2, by simp [lower, h1], h2, by simp [h3]⟩
    | i32Bin op =>
      rcases stack with _ | ⟨r, _ | ⟨l, st0⟩⟩
      · simp [lower] at hl
      · simp [lower] at hl
      cases hop : liftI32 op with
      | none => simp [lower, hop] at hl
      | some b =>
        simp only [List.cons_append, lower, hop, Option.map_eq_some_iff] at hl
        obtain ⟨⟨body, final', fl', m'⟩, hres, heq⟩ := hl
        simp only [Prod.mk.injEq] at heq
        obtain ⟨rfl, rfl, rfl, rfl⟩ := heq
        obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hres
        exact ⟨IR.Instr.binI32 n b l r :: b1, st1, l1, k1, b2, by simp [lower, hop, h1], h2,
          by simp [h3]⟩
    | i32Cmp op =>
      rcases stack with _ | ⟨r, _ | ⟨l, st0⟩⟩
      · simp [lower] at hl
      · simp [lower] at hl
      simp only [List.cons_append, lower, Option.map_eq_some_iff] at hl
      obtain ⟨⟨body, final', fl', m'⟩, hres, heq⟩ := hl
      simp only [Prod.mk.injEq] at heq
      obtain ⟨rfl, rfl, rfl, rfl⟩ := heq
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hres
      exact ⟨IR.Instr.cmpI32 n (liftCmp op) l r :: b1, st1, l1, k1, b2, by simp [lower, h1], h2,
        by simp [h3]⟩
    | load op index arg =>
      rcases stack with _ | ⟨a, st0⟩
      · simp [lower] at hl
      have hop : op.resultType = .i32 := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      simp only [List.cons_append, lower, hop, ↓reduceIte, Option.map_eq_some_iff] at hl
      obtain ⟨⟨body, final', fl', m'⟩, hres, heq⟩ := hl
      simp only [Prod.mk.injEq] at heq
      obtain ⟨rfl, rfl, rfl, rfl⟩ := heq
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hres
      exact ⟨IR.Instr.load n index a arg.offset op :: b1, st1, l1, k1, b2,
        by simp [lower, hop, h1], h2, by simp [h3]⟩
    | store op index arg =>
      rcases stack with _ | ⟨v, _ | ⟨a, st0⟩⟩
      · simp [lower] at hl
      · simp [lower] at hl
      have hop : op.operandType = .i32 := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      simp only [List.cons_append, lower, hop, ↓reduceIte, Option.map_eq_some_iff] at hl
      obtain ⟨⟨body, final', fl', m'⟩, hres, heq⟩ := hl
      simp only [Prod.mk.injEq] at heq
      obtain ⟨rfl, rfl, rfl, rfl⟩ := heq
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hres
      exact ⟨IR.Instr.store index a v arg.offset op.width :: b1, st1, l1, k1, b2,
        by simp [lower, hop, h1], h2, by simp [h3]⟩
    | globalGet index =>
      simp only [List.cons_append, lower, Option.map_eq_some_iff] at hl
      obtain ⟨⟨body, final', fl', m'⟩, hres, heq⟩ := hl
      simp only [Prod.mk.injEq] at heq
      obtain ⟨rfl, rfl, rfl, rfl⟩ := heq
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hres
      exact ⟨IR.Instr.globalGet n index :: b1, st1, l1, k1, b2, by simp [lower, h1], h2,
        by simp [h3]⟩
    | globalSet index =>
      rcases stack with _ | ⟨v, st0⟩
      · simp [lower] at hl
      simp only [List.cons_append, lower, Option.map_eq_some_iff] at hl
      obtain ⟨⟨body, final', fl', m'⟩, hres, heq⟩ := hl
      simp only [Prod.mk.injEq] at heq
      obtain ⟨rfl, rfl, rfl, rfl⟩ := heq
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hres
      exact ⟨IR.Instr.globalSet index v :: b1, st1, l1, k1, b2, by simp [lower, h1], h2,
        by simp [h3]⟩
    | localGet k =>
      cases hk : locs[k]? with
      | none => simp [lower, hk] at hl
      | some id =>
        simp only [List.cons_append, lower, hk] at hl
        obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hl
        exact ⟨b1, st1, l1, k1, b2, by simp [lower, hk, h1], h2, h3⟩
    | localSet k =>
      rcases stack with _ | ⟨v, st0⟩
      · simp [lower] at hl
      have hk : k < locs.length := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      simp only [List.cons_append, lower, hk, ↓reduceIte] at hl
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hl
      exact ⟨b1, st1, l1, k1, b2, by simp [lower, hk, h1], h2, h3⟩
    | localTee k =>
      rcases stack with _ | ⟨v, st0⟩
      · simp [lower] at hl
      have hk : k < locs.length := by
        refine Classical.byContradiction fun hne => ?_
        simp [lower, hne] at hl
      simp only [List.cons_append, lower, hk, ↓reduceIte] at hl
      obtain ⟨b1, st1, l1, k1, b2, h1, h2, h3⟩ := ih _ _ _ _ _ _ _ hl
      exact ⟨b1, st1, l1, k1, b2, by simp [lower, hk, h1], h2, h3⟩
    | _ => simp [lower] at hl

theorem execOp_load_trap (op : LoadOp) (index : Nat) (arg : MemArg) (c : Configuration) (t : Trap)
    (h : execOp (.load op index arg) c = some (.trap t)) :
    ∃ (x : Int) (rest : List Value) (mem : MemoryState),
      c.operandStack = .i32 x :: rest ∧ c.store.memory? index = some mem ∧
      mem.readLE? (effectiveAddress x arg) op.width = none := by
  simp only [execOp] at h
  split at h
  · rename_i address rest memory hstack hm
    split at h
    · cases h
    · rename_i hread
      exact ⟨address, rest, memory, by rw [hstack], hm, hread⟩
  · simp at h

theorem execOp_store_trap (op : StoreOp) (index : Nat) (arg : MemArg) (c : Configuration) (t : Trap)
    (h : execOp (.store op index arg) c = some (.trap t)) :
    ∃ (operand : Value) (x : Int) (rest : List Value) (mem : MemoryState),
      c.operandStack = operand :: .i32 x :: rest ∧ c.store.memory? index = some mem ∧
      mem.writeLE? (effectiveAddress x arg) op.width (storedBits operand) = none := by
  simp only [execOp] at h
  split at h
  · rename_i operand address rest memory hstack hm
    split at h
    · split at h
      · cases h
      · rename_i hwrite
        exact ⟨operand, address, rest, memory, hstack, hm, hwrite⟩
    · simp at h
  · simp at h


theorem i32Bin_trap_unlifted (op : IBinOp) (c : Configuration) (t : Trap)
    (h : execOp (.i32Bin op) c = some (.trap t)) : liftI32 op = none := by
  cases hop : liftI32 op with
  | none => rfl
  | some b =>
    exfalso
    simp only [execOp] at h
    split at h
    · split at h
      · cases h
      · rename_i hnone
        rw [evalI32Bin_lifts op b hop] at hnone
        simp at hnone
    · simp at h

theorem lower_head_load (op : LoadOp) (index : Nat) (arg : MemArg) (post : List Instr)
    (a : IR.ValueId) (st0 l : List IR.ValueId) (k : Nat) (b2 : List IR.Instr)
    (f fl : List IR.ValueId) (m : Nat)
    (hl : lower (.load op index arg :: post) (a :: st0) l k = some (b2, f, fl, m)) :
    ∃ body', b2 = IR.Instr.load k index a arg.offset op :: body' := by
  have hop : op.resultType = .i32 := by
    refine Classical.byContradiction fun hne => ?_
    simp [lower, hne] at hl
  simp only [lower, hop, ↓reduceIte, Option.map_eq_some_iff] at hl
  obtain ⟨⟨body, f', fl', m'⟩, -, heq⟩ := hl
  simp only [Prod.mk.injEq] at heq
  obtain ⟨rfl, -, -, -⟩ := heq
  exact ⟨body, rfl⟩

theorem lower_head_store (op : StoreOp) (index : Nat) (arg : MemArg) (post : List Instr)
    (v a : IR.ValueId) (st0 l : List IR.ValueId) (k : Nat) (b2 : List IR.Instr)
    (f fl : List IR.ValueId) (m : Nat)
    (hl : lower (.store op index arg :: post) (v :: a :: st0) l k = some (b2, f, fl, m)) :
    ∃ body', b2 = IR.Instr.store index a v arg.offset op.width :: body' := by
  have hop : op.operandType = .i32 := by
    refine Classical.byContradiction fun hne => ?_
    simp [lower, hne] at hl
  simp only [lower, hop, ↓reduceIte, Option.map_eq_some_iff] at hl
  obtain ⟨⟨body, f', fl', m'⟩, -, heq⟩ := hl
  simp only [Prod.mk.injEq] at heq
  obtain ⟨rfl, -, -, -⟩ := heq
  exact ⟨body, rfl⟩

theorem runBody_load_trap (r index a : Nat) (offset : Nat) (op : LoadOp) (body : List IR.Instr)
    (s : IR.State) (t : List Event) (x : Int) (m : MemoryState)
    (ha : s.values a = .i32 x) (hm : s.store.memory? index = some m)
    (hread : m.readLE? (effectiveAddress x { offset := offset }) op.width = none) :
    IR.runBody (IR.Instr.load r index a offset op :: body) s t = none := by
  simp [IR.runBody, IR.execInstr, IR.intPayload, ha, hm, hread]

theorem runBody_store_trap (index : Nat) (a v : IR.ValueId) (offset width : Nat)
    (body : List IR.Instr) (s : IR.State) (t : List Event) (x : Int) (operand : Value)
    (m : MemoryState) (ha : s.values a = .i32 x) (hv : s.values v = operand)
    (hm : s.store.memory? index = some m)
    (hwrite : m.writeLE? (effectiveAddress x { offset := offset }) width (storedBits operand) = none) :
    IR.runBody (IR.Instr.store index a v offset width :: body) s t = none := by
  simp [IR.runBody, IR.execInstr, IR.intPayload, ha, hv, hm, hwrite]


theorem runBody_append (a b : List IR.Instr) (s : IR.State) (t : List Event) :
    IR.runBody (a ++ b) s t =
      (match IR.runBody a s t with
        | some (s', t') => IR.runBody b s' t'
        | none => none) := by
  induction a generalizing s t with
  | nil => simp [IR.runBody]
  | cons x xs ih =>
    cases hx : IR.execInstr x s with
    | trapped trap => simp [IR.runBody, hx]
    | ran s1 ev res =>
      simp only [List.cons_append, IR.runBody, hx]
      exact ih _ _

inductive WasmTraps : List Instr → Configuration → Prop
  | here (i : Instr) (rest : List Instr) (c : Configuration) (t : Trap)
      (h : execOp i c = some (.trap t)) : WasmTraps (i :: rest) c
  | later (i : Instr) (rest : List Instr) (c c1 : Configuration)
      (h : execOp i c = some (.step c1)) (hr : WasmTraps rest c1) : WasmTraps (i :: rest) c

theorem traps_split (body : List Instr) (c : Configuration) (h : WasmTraps body c) :
    ∃ (pre : List Instr) (i : Instr) (post : List Instr) (ck : Configuration) (evs : List Event)
      (t : Trap), body = pre ++ i :: post ∧ runWasm pre c = some (ck, evs) ∧
        execOp i ck = some (.trap t) := by
  induction h with
  | here i rest c t hexec => exact ⟨[], i, rest, c, [], t, rfl, rfl, hexec⟩
  | later i rest c c1 hexec _ ih =>
    obtain ⟨pre, i', post, ck, evs, t, hsplit, hpre, htrap⟩ := ih
    refine ⟨i :: pre, i', post, ck, stepEvents i c ++ evs, t, ?_, ?_, htrap⟩
    · rw [hsplit]; rfl
    · simp [runWasm, hexec, hpre]

theorem trap_position (body : List Instr) (c : Configuration) (htr : WasmTraps body c) :
    ∀ (s : IR.State) (stack locs : List IR.ValueId) (n : Nat) (ir : List IR.Instr)
      (final finalLocs : List IR.ValueId) (m : Nat),
      Corresponds c s stack locs n →
      (∀ g ∈ c.store.globals, g.ty = .i32 ∧ g.value.type = .i32) →
      lower body stack locs n = some (ir, final, finalLocs, m) →
      ∀ (trace : List Event), IR.runBody ir s trace = none := by
  intro s stack locs n ir final finalLocs m hc hgl hl trace
  obtain ⟨pre, i, post, ck, evs, t, hsplit, hpre, htrap⟩ := traps_split body c htr
  subst hsplit
  obtain ⟨b1, st1, l1, k1, b2, hlp, hlq, hir⟩ :=
    lower_append pre (i :: post) stack locs n ir final finalLocs m hl
  obtain ⟨s1, hrun1, hc1⟩ :=
    simulation pre c ck evs s stack locs n b1 st1 l1 k1 hc hgl hlp hpre trace
  have hstk1 : ck.operandStack = (st1.map s1.values) := hc1.1
  have hst1 : ck.store = s1.store := hc1.2.2.2.1
  rw [hir, runBody_append, hrun1]
  show IR.runBody b2 s1 (trace ++ evs) = none
  cases i with
  | i32Const v =>
    simp only [execOp] at htrap
    cases htrap
  | i32Bin op =>
    have hun := i32Bin_trap_unlifted op ck t htrap
    rcases st1 with _ | ⟨r, _ | ⟨l, st0⟩⟩
    · simp [lower] at hlq
    · simp [lower] at hlq
    · simp [lower, hun] at hlq
  | i32Cmp op =>
    simp only [execOp] at htrap
    split at htrap <;> simp at htrap
  | load op index arg =>
    obtain ⟨x, rest, mem, hsk, hm, hread⟩ := execOp_load_trap op index arg ck t htrap
    rcases st1 with _ | ⟨a, st0⟩
    · simp [lower] at hlq
    obtain ⟨body', hb2⟩ := lower_head_load op index arg post a st0 l1 k1 b2 final finalLocs m hlq
    have hsk' : (a :: st0).map s1.values = .i32 x :: rest := by rw [← hstk1]; exact hsk
    have hax : s1.values a = .i32 x := by
      simp only [List.map_cons, List.cons.injEq] at hsk'
      exact hsk'.1
    have hm1 : s1.store.memory? index = some mem := by rw [← hst1]; exact hm
    have harg : ({ offset := arg.offset } : MemArg) = arg := by cases arg; rfl
    rw [hb2]
    exact runBody_load_trap k1 index a arg.offset op body' s1 (trace ++ evs) x mem hax hm1
      (by rw [harg]; exact hread)
  | store op index arg =>
    obtain ⟨operand, x, rest, mem, hsk, hm, hwrite⟩ := execOp_store_trap op index arg ck t htrap
    rcases st1 with _ | ⟨v, _ | ⟨a, st0⟩⟩
    · simp [lower] at hlq
    · simp [lower] at hlq
    obtain ⟨body', hb2⟩ := lower_head_store op index arg post v a st0 l1 k1 b2 final finalLocs m hlq
    have hsk' : (v :: a :: st0).map s1.values = operand :: .i32 x :: rest := by
      rw [← hstk1]; exact hsk
    have hvv : s1.values v = operand := by
      simp only [List.map_cons, List.cons.injEq] at hsk'
      exact hsk'.1
    have hax : s1.values a = .i32 x := by
      simp only [List.map_cons, List.cons.injEq] at hsk'
      exact hsk'.2.1
    have hm1 : s1.store.memory? index = some mem := by rw [← hst1]; exact hm
    have harg : ({ offset := arg.offset } : MemArg) = arg := by cases arg; rfl
    rw [hb2]
    exact runBody_store_trap index a v arg.offset op.width body' s1 (trace ++ evs) x operand mem
      hax hvv hm1 (by rw [harg]; exact hwrite)
  | globalGet index =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | globalSet index =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | localGet k =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | localSet k =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | localTee k =>
    simp only [execOp] at htrap
    repeat (split at htrap)
    all_goals simp_all
  | _ => simp [lower] at hlq

end V4Straight
end TptWasm
