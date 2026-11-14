import TptWasm.CFG

/-!
# Block merging: the model

`tpt-wasm-opt::graph`'s `merge_blocks` appends a block `to` to its only predecessor
`from`. The edge `from -> to(args)` is replaced by `to`'s body and terminator, with
`to`'s parameters replaced by the edge's arguments. This file defines that rewrite.

## What the rewrite changes

* The parameters `ps` of `to` are *renamed*: every operand that names `ps[i]` now names
  `args[i]`. Only operands are renamed, never the results an instruction defines.
* `from`'s body grows by `to`'s renamed body, and `from`'s terminator becomes `to`'s
  renamed terminator. `to` disappears.
* Nothing else changes.

The pass only merges when the conditions in `Proofs.Merge` hold; the definitions here
are total so the rewrite can be stated for any function.
-/

namespace TptWasm
namespace IR

/-- The value a parameter-renaming substitution maps `x` to: `ps[i]` becomes `as[i]`,
and anything not named in `ps` is unchanged. -/
def substVal : List ValueId → List ValueId → ValueId → ValueId
  | p :: ps, a :: as, x => if x = p then a else substVal ps as x
  | _, _, x => x

/-- An instruction with its operands renamed by `σ`. Results are not renamed: they
are definitions, not reads. -/
def renameInstr (σ : ValueId → ValueId) : Instr → Instr
  | .constI32 r v => .constI32 r v
  | .constI64 r v => .constI64 r v
  | .binI32 r op l rr => .binI32 r op (σ l) (σ rr)
  | .binI64 r op l rr => .binI64 r op (σ l) (σ rr)
  | .cmpI32 r op l rr => .cmpI32 r op (σ l) (σ rr)
  | .load r m a w => .load r m (σ a) w
  | .store m a v w => .store m (σ a) (σ v) w
  | .globalGet r g => .globalGet r g
  | .globalSet g v => .globalSet g (σ v)
  | .call i args rs => .call i (args.map σ) rs
  | .callHost n g args rs => .callHost n g (args.map σ) rs
  | .drop v => .drop (σ v)

/-- A terminator with its operands renamed by `σ`. -/
def renameTerm (σ : ValueId → ValueId) : Terminator → Terminator
  | .branch t args => .branch t (args.map σ)
  | .condBranch c (t₁, a₁) (t₂, a₂) => .condBranch (σ c) (t₁, a₁.map σ) (t₂, a₂.map σ)
  | .ret values => .ret (values.map σ)
  | .trap t => .trap t
  | .unreachable => .unreachable

/-- The block `from` becomes after absorbing `to`, which it enters with `args`.

`to`'s parameters are renamed to `args` inside the absorbed body and terminator. -/
def mergeBlock (fb tb : Block) (args : List ValueId) : Block :=
  { fb with
    instrs := fb.instrs ++ tb.instrs.map (renameInstr (substVal tb.params args)),
    terminator := renameTerm (substVal tb.params args) tb.terminator }

/-- The block `b` becomes in the merged function: the block named `from_` absorbs `tb`,
entered with `args`, and every other block is unchanged. -/
def mapBlock (from_ : BlockId) (tb : Block) (args : List ValueId) (b : Block) : Block :=
  if b.id = from_ then mergeBlock b tb args else b

/-- The merged function: `to` is removed, and `from_` absorbs it. Blocks keep their order,
and every block keeps its identifier. -/
def mergeFn (f : Function) (from_ to : BlockId) (tb : Block) (args : List ValueId) : Function :=
  { f with blocks := (f.blocks.filter (fun b => decide (b.id ≠ to))).map (mapBlock from_ tb args) }

end IR
end TptWasm
