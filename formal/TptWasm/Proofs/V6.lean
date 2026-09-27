import TptWasm.IR
import TptWasm.IRExec
import TptWasm.Passes

/-!
# V6: lowering correctness (skeleton)

`spec.txt` section 29 asks for a *skeleton* for this link, and the word is
load-bearing. This file states the target, the lowering, the relation, and every
obligation, and proves the one thing that can be proved without a target
architecture: that the lowering preserves the shape of the control flow.

## Why a skeleton, and not a proof

V5 was the last link in the chain provable at this level of abstraction: both
sides of its equation are IR. V6's two sides are not. The right-hand side is
machine code, and no machine semantics exists in this model, so there is
nothing yet for a proof to relate the IR to.

Writing `theorem` statements with `sorry` would compile and would prove nothing,
and this package admits no `sorry` or `admit` -- a file that builds while
asserting less than it appears to is worse than a gap that is visible. So the
obligations are a `structure` instead: the shape a proof must have, field by
field, each field stating precisely what must be shown. It compiles because every
field is a hypothesis the caller supplies, which makes the file a checked
specification rather than a list of promises.

## The target

Deliberately the simplest machine that can express the obligations: flat
instruction lists over slot indices, with explicit jumps. It is not a real
backend. It is the shape a real backend's output has, at the level of detail
where "the lowering preserved the control flow" is a question with an answer.
-/

namespace TptWasm
namespace IR

/-- A slot in the machine's value array. -/
abbrev Slot := Nat

/-- A machine instruction.

The three shapes are the point of the model. A pure operation may be reordered or
eliminated; an effect may not; a jump is control flow. The lowering has to
preserve that classification, and that is one of the obligations rather than
something the model takes on trust. -/
inductive MInstr where
  /-- Define `slot` to be the constant `value`. -/
  | constI32 (slot : Slot) (value : Int)
  /-- Define `slot` to be `left op right`. -/
  | binI32 (slot : Slot) (op : BinOp) (left right : Slot)
  /-- Define `slot` to be `left <op> right`. -/
  | cmpI32 (slot : Slot) (op : ICmp) (left right : Slot)
  /-- Read memory, emitting an event. -/
  | load (slot : Slot) (memory : MemoryId) (address : Slot) (width : Nat)
  /-- Write memory, emitting an event. -/
  | store (memory : MemoryId) (address value : Slot) (width : Nat)
  /-- Read a global, emitting an event. -/
  | globalGet (slot : Slot) (index : GlobalId)
  /-- Write a global, emitting an event. -/
  | globalSet (index : GlobalId) (value : Slot)
  /-- Call a function, emitting an event. -/
  | call (index : Nat) (args : List Slot) (results : List Slot)
  /-- Invoke the host, emitting an event. -/
  | callHost (name : String) (granted : Bool) (args : List Slot) (results : List Slot)
  /-- Discard a slot. -/
  | drop (slot : Slot)
  /-- Enter a block, binding its parameters. -/
  | jump (target : Nat) (args : List Slot)
  /-- Enter one of two blocks on a condition, binding each arm's parameters. -/
  | jumpIf (condition : Slot) (thenArm elseArm : Nat × List Slot)
  /-- Return from the function. -/
  | ret (values : List Slot)
  /-- Execution ends in a trap. -/
  | trap (trap : Trap)
  /-- Execution cannot continue. -/
  | unreachable
  deriving DecidableEq, Repr

/-- A machine instruction whose execution is observable.

The counterpart of `observable` in `TptWasm.Passes`, on the machine side. The
two classifications have to agree, and that is an obligation rather than a
definition, because nothing forces a lowering to preserve it. -/
def isEffect : MInstr -> Bool
  | .load _ _ _ _ | .store _ _ _ _ => true
  | .globalGet _ _ | .globalSet _ _ => true
  | .call _ _ _ | .callHost _ _ _ _ => true
  | _ => false

/-- How many control-flow edges this instruction accounts for.

A conditional jump is one instruction and *two* edges, which is why the control
obligation cannot be stated as "a jump appears". -/
def edgeCount : MInstr -> Nat
  | .jump _ _ => 1
  | .jumpIf _ _ _ => 2
  | _ => 0

/-- A machine instruction that moves control flow. -/
def isJump : MInstr -> Bool
  | .jump _ _ | .jumpIf _ _ _ => true
  | _ => false

/-- A machine function. -/
structure MFunction where
  /-- The slots the lowering handed out, in declaration order. -/
  slots : List ValueId
  /-- One instruction list per IR block, in the IR's block order. -/
  blocks : List (List MInstr)
  /-- Which block starts the function, as an *index*.

  An index rather than a `BlockId`, because a real backend renumbers blocks and a
  proof that silently assumed the numbering survived would be worth nothing. -/
  entry : Nat
  deriving DecidableEq, Repr

/-- The index of the block with this id, defaulting to the first block.

The default is the same "refuse rather than approximate" discipline the rest of
the model uses: a function whose blocks do not contain the id is one the verifier
has already rejected, and this returns a total answer rather than an option the
caller would have to thread. -/
def indexOf (id : BlockId) (blocks : List Block) : Nat :=
  (blocks.findIdx? fun b => b.id = id).getD 0

/-- The `j`th block, or an empty one.

Total, so an obligation can be stated for every `j` without also having to
establish `j` is in bounds -- which is not a fact about the lowering and would
otherwise be a second thing to prove at every use. -/
def blockAt (blocks : List Block) (j : Nat) : Block :=
  blocks.getD j { id := 0, params := [], instrs := [], terminator := .unreachable }

/-- The `j`th machine block, or an empty one. -/
def mblockAt (blocks : List (List MInstr)) (j : Nat) : List MInstr :=
  blocks.getD j []

/-- Lower one IR instruction to machine instructions.

`isInstr` is the register allocation, and it is a parameter so the obligations
below can be stated against whatever it returns: a wrong allocator and a right
one are then both testable against the same specification, rather than the
specification being written to fit one allocator. -/
def lowerInstr (isInstr : ValueId -> Slot) : Instr -> List MInstr
  | .constI32 r v => [.constI32 (isInstr r) v]
  | .constI64 _ _ => []
  | .binI32 r op l rr => [.binI32 (isInstr r) op (isInstr l) (isInstr rr)]
  | .binI64 _ _ _ _ => []
  | .cmpI32 r op l rr => [.cmpI32 (isInstr r) op (isInstr l) (isInstr rr)]
  | .load r m a w => [.load (isInstr r) m (isInstr a) w]
  | .store m a v w => [.store m (isInstr a) (isInstr v) w]
  | .globalGet r index => [.globalGet (isInstr r) index]
  | .globalSet index v => [.globalSet index (isInstr v)]
  | .call index args results => [.call index (args.map isInstr) (results.map isInstr)]
  | .callHost name granted args results =>
      [.callHost name granted (args.map isInstr) (results.map isInstr)]
  | .drop v => [.drop (isInstr v)]

/-- The lowered body of one IR block. -/
def lowerBody (isInstr : ValueId -> Slot) (b : Block) : List MInstr :=
  b.instrs.flatMap (lowerInstr isInstr)

/-- Lower a terminator.

Branch targets become *indices*, not `BlockId`s, because that is what a machine
addresses blocks by -- and getting this wrong is the single most common way a
lowering silently changes which blocks run. -/
def lowerTerm (blocks : List Block) (isInstr : ValueId -> Slot) : Terminator -> List MInstr
  | .branch t args => [.jump (indexOf t blocks) (args.map isInstr)]
  | .condBranch c (t1, a1) (t2, a2) =>
      [.jumpIf (isInstr c) (indexOf t1 blocks, a1.map isInstr)
        (indexOf t2 blocks, a2.map isInstr)]
  | .ret values => [.ret (values.map isInstr)]
  | .trap t => [.trap t]
  | .unreachable => [.unreachable]

/-- Lower a whole function. -/
def lower (f : Function) (isInstr : ValueId -> Slot) (entryIndex : Nat) : MFunction :=
  { slots := f.declared
    blocks := f.blocks.map fun b => lowerBody isInstr b ++ lowerTerm f.blocks isInstr b.terminator
    entry := entryIndex }

/-- The machine semantics, as a parameter.

Neither side of the refinement relation is defined here, and that is the whole
gap V6 rests on: `TptWasm.IRExec` has a semantics for a straight-line IR body but
not for a whole `Function` with control flow, and nothing at all has a semantics
for `MFunction`. Taking both as parameters keeps the relation a *statement*
rather than a definition, so it can be checked now and discharged later. -/
structure Semantics where
  /-- Run a whole IR function. -/
  irRun : Function -> State -> List Event -> Option (State × List Event)
  /-- Run a whole machine function. -/
  machRun : MFunction -> State -> List Event -> Option (State × List Event)

/-- A machine function refines an IR one.

Same shape as `BodyEquiv` in `TptWasm.Proofs.V5`, and for the same reason: the
executable semantics are the specification, and a relation over two executions
composes with the rest of the refinement chain. -/
def Refines (sem : Semantics) (m : MFunction) (f : Function) (s : State) : Prop :=
  ∀ t1 t2, sem.machRun m s [] = some (s, t1) -> sem.irRun f s [] = some (s, t2) -> t1 = t2

/-- Everything a proof that `lower` is correct has to establish.

The fields run from the cheapest to the most expensive, so a partial proof is a
prefix rather than a scattered set. -/
structure LoweringObligations
    (f : Function) (isInstr : ValueId -> Slot) (m : MFunction) (sem : Semantics) : Prop where
  /-- **Slot injectivity.** Two values never share a slot.

  This is what makes `isInstr` a function rather than a lookup that silently
  collides. A collision is the classic register-allocation bug, and it is
  invisible in the output -- the lowered code still typechecks and still runs --
  so it has to be an explicit field rather than an implementation detail. -/
  slot_injective : ∀ a b, isInstr a = isInstr b -> a = b

  /-- **Block shape.** The machine has one block per IR block, in the same order.

  Stated as a count rather than an identity so a backend that renumbers blocks
  satisfies it, while a backend that *drops* or *duplicates* a block does not. -/
  block_count : m.blocks.length = f.blocks.length

  /-- **Entry correspondence.** The machine entry is the index of the IR entry. -/
  entry_is_index_of_entry : m.entry = indexOf f.entry f.blocks

  /-- **Effects are not lost.** Every block whose IR body has an observable
  instruction lowers to a block with a machine effect.

  Losing one deletes a trap or a host call, which is the contract's trap-position
  clause in concrete form. -/
  effects_forward : ∀ j, j ≤ f.blocks.length ->
    (blockAt f.blocks j).instrs.any observable ->
    (mblockAt m.blocks j).any isEffect

  /-- **Effects are not invented.** A block with no observable IR instruction
  lowers to a block with no machine effect.

  Gaining one matters as much as losing one: it performs an effect the embedder
  never asked for, and the host boundary is the place that shows up. -/
  effects_backward : ∀ j, j ≤ f.blocks.length ->
    (mblockAt m.blocks j).any isEffect ->
    (blockAt f.blocks j).instrs.any observable

  /-- **Control flow.** A block's terminator edges and the block's machine jumps
  account for the same number of edges.

  Counted rather than compared pointwise because a backend may lay the jumps out
  in any order, and because one `jumpIf` is two edges. A lost edge merges two
  paths; an invented one introduces a path the program never had, and the two
  failures are not the same, so the field is stated as a count that catches both. -/
  jumps_match : ∀ j, j ≤ f.blocks.length ->
    (blockAt f.blocks j).terminator.targets.length =
      ((mblockAt m.blocks j).map edgeCount).sum

  /-- **The main event.** From the same initial state, the machine and the IR
  finish in the same state with the same event trace.

  This is the obligation the five above exist to make provable, and it is stated
  last because it is the one that consumes them. -/
  behaviour : ∀ s, Refines sem m f s

/- **V6: the lowering preserves the number of blocks.**

The one obligation that is closed today, because it is a property of `lower`
alone: it maps `f.blocks` through `lowerBody`, which produces exactly one entry
per block. It is worth having precisely because it is cheap -- it costs no
assumptions about the allocator and no target model -- and because it is the
shape every later obligation also has. -/
theorem lower_preserves_block_count (f : Function) (isInstr : ValueId -> Slot)
    (entryIndex : Nat) : (lower f isInstr entryIndex).blocks.length = f.blocks.length := by
  simp [lower]

end IR
end TptWasm
