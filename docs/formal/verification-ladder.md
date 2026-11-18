# The verification ladder

`spec.txt` section 29 fixes the order formal work takes:

```text
V0  Type invariants
 ↓
V1  Interpreter correspondence
 ↓
V2  Memory safety
 ↓
V3  Host capability safety
 ↓
V4  IR refinement
 ↓
V5  Compiler transformation correctness
 ↓
V6  Machine-code refinement
```

and section 29 says why: "this makes formal verification a continuous
architectural property rather than a giant final project." Each rung is a claim
that is either proved or is not, and the point of the ordering is that a rung is
only worth attempting once the one below it holds.

Section 37 gives the compiler its shape: `Core Wasm ≈ TPT-Wasm IR ≈ Optimized IR
≈ Machine IR ≈ Machine Code`, with each `≈` a refinement, so that "the compiler
never becomes the definition of Wasm semantics."

## What "refinement" means here

`TptWasm.Observation` defines it: two computations are refinements of each other
when they leave the same **observation** -- the same ordered event trace, and the
same returned values or the same trap.

Three consequences are worth stating because they are what make the definition
usable rather than merely strict.

**A trap is an event, not a verdict.** Section 67 says "an optimization that
changes *when* a trap occurs may be incorrect even if the eventual return value
looks identical", so the model records the trap *in the trace*, in order, with its
identity. A store that happened before a trap is observable; one that did not
happen is not.

**Arithmetic is not observable.** No `Event` constructor corresponds to computing
a value. That is what gives an optimizer room to work, and it is why the
inertness theorems below are the load-bearing ones.

**Refinement is equality of observation, not up-to-equality.** Two programs that
differ in arithmetic, in evaluation order, or in block count are refinements of
each other. This is the same claim the Rust contract checker in
`tpt_wasm_opt::contract` enforces at run time, which is what makes the formal and
executable checks worth having together: the checker finds violations on real
modules, the theory says what one would mean.

## Where each rung stands

| Rung | Status | Notes |
| --- | --- | --- |
| V0 type invariants | proved | `TptWasm.Invariants` |
| V1 interpreter correspondence | proved | `TptWasm.Proofs.V1` |
| V2 memory safety | proved | `TptWasm.Proofs.V2` |
| V3 host capability safety | proved | `TptWasm.Proofs.V3` |
| V4 IR refinement | **partial**: straight-line subset and one `if`/`else` diamond proved | see below |
| V5 transformation correctness | **partial** | six theorems proved |
| V6 machine-code refinement | **not started** | see below |

### V4: what exists and what does not

`TptWasm.IR` models the IR (SSA values, blocks with parameters, terminators whose
arms bundle a target with the values binding it) and `TptWasm.IRExec` gives its
execution. That is the vocabulary V4 and V5 are stated in, and it is what a later
refinement theorem would quantify over.

Two refinement results are proved, each for a shape narrower than the full language:

* **Straight-line bodies** (`TptWasm.Proofs.V4Straight`, `simulation`): a Wasm body of `i32`
  constants, operators, comparisons, memory accesses, globals and locals, and its lowering to one
  IR block, end in the same operand values, store and events.
* **One `if`/`else` diamond** (`TptWasm.Diamond`, `TptWasm.Proofs.V4Diamond`,
  `diamond_simulation`): `pre ; if ... else ... end ; post`, where `pre` leaves one `i32`
  condition, the arms are stack-neutral, and the locals pass through block parameters. The
  lowering is four IR blocks. The theorem says the lowered function returns the events and
  values the Wasm run returns. `TptWasm.V4DiamondExample` applies it to one concrete diamond and
  discharges every hypothesis, so the statement is shown to be inhabited.

Both are checked with `lake build`, with no `sorry`, and depend only on the standard axioms.

The Wasm model now has the structured `if` as `OpStep` rules (`ifThen`, `ifElse`, `elseSkip`, over a
flat layout with `ifInstr` and `elseInstr` markers), and `TptWasm.Proofs.IfControl` proves that each
rule lands the program counter where that layout says. `TptWasm.Proofs.MachineDiamond` proves that
the machine (the interpreter, run over the program counter) computes what the diamond's `runShape`
computes on a flat body, so the rules and the list semantics agree on the diamond. `TptWasm.Proofs.Triangle`
states the machine, the list semantics and the block graph together for one run: the machine's end
state carries the block graph's returned values, with the same events. The lowering is stated over
the flat body the machine runs (`TptWasm.Proofs.FlatLower`: `lowerFlat`, with `triangleFlat`), which
reads a flat diamond back into its shape and lowers that shape. The two sides still meet through the
list semantics (`runShape`), not directly. Block, loop and branch instructions are not in the model.

Nested diamonds are also proved (`TptWasm.Nested`): a statement is straight-line code or an
`if`/`else` whose arms are statement lists, to any depth. The machine runs the flat layout as the
list semantics does (`machine_seq`); the lowering gives each statement a continuation block, so an
`if` needs no join block (`Lower`); the block graph reaches the continuation in a known number of
steps (`GotoN`), and the simulation is by induction over the statement structure (`Simulate`).
`nested_triangle` states the machine and the IR agree with the Wasm run, and `Nested.Example`
applies it to a program with an `if` nested in an arm. Loops, branches, calls, and traps inside
statements are not yet in the model.

Not proved: nested control flow, loops, `br`/`br_if`/`br_table`, calls, traps in the diamond,
the store and locals after the run (the diamond theorem exports events and values only), and
the refinement chain that composes V4 with V5 and V6. The lowering is not yet tied to the Rust
lowerer by translation validation for the diamond; the straight-line subset is.

### V5: what is proved

Six theorems in `TptWasm.Proofs.V5`, over a block body and a state whose operands
are already bound:

* `constI32_inert`, `constI64_inert` -- a constant emits no event and leaves the
  store alone.
* `binI32_inert`, `cmpI32_inert` -- so does pure arithmetic. Together with the two
  above this is the whole content of constant folding: a fold replaces an
  arithmetic instruction with the constant it computes, and both are invisible.
* `store_not_inert`, `load_not_inert` -- a memory access *is* observable, and
  takes the write (or read) succeeding as its premise rather than re-deriving V2's
  bounds argument.

The sixth is the one that makes the other five mean something. A contract in
which everything were inert would be satisfied by an optimizer that deleted every
store; the two memory theorems are what rule that out.

### V5: what is not proved

* **The whole-pass folding theorem.** The per-step fact (`foldable` produces an
  inert instruction) is not yet connected to `foldBlock` by an induction, so
  `foldBlock`'s output is not shown to observe the same as its input. The
  remaining step is mechanical; it was not completed here.
* **Dead-value elimination** is proved over a block body by
  `TptWasm.Proofs.V5.deadBlock_sound`: running the pruned body from a state that
  agrees on the live values gives the same trace and a trap at the same point, and
  final states that agree on `extra`. `extra` is the liveness premise the caller
  supplies (the values the terminator and successors read). Writing that proof
  exposed a bug in the model: `deadBlock` had been tested against the uses of the
  instructions *before* each one, not after, and so deleted values that later
  instructions read. The model now runs backwards, as `tpt-wasm-opt::dce` does.
* **CFG simplification.** Partly proved. Unreachable-block removal is sound over
  whole functions (`TptWasm.Proofs.CFG.prune_sound` and `prune_reachable_sound`), and
  empty-block threading preserves behaviour (`TptWasm.Proofs.Threading.thread_behaviour`).
  Threading assumes that an edge into a chain carries no values, which the verifier's
  arity rule guarantees. Block merging is proved over whole functions
  (`TptWasm.Proofs.MergeBlocks.merge_behaviour`), under the pass's own conditions
  (`MergeCtx`): a single predecessor, matching arity, parameters read only in their
  block, and unique identifiers and definitions. Constant-condition folding is proved
  over whole functions (`TptWasm.Proofs.FoldWhole.fold_behaviour`), from a dominance
  invariant over the block graph (`TptWasm.Dominance`) and the verifier's checks, which
  are stated as the `FoldCtx` hypotheses.
* **Refinement of the Rust passes.** Translation validation, not proof:
  `crates/tpt-wasm-opt/tests/translation_validation.rs` checks the CFG simplification
  pass against the Lean model on generated functions in the modelled subset (opt-in,
  with `TPT_TRANSLATION_VALIDATION=1`). It includes loops, agrees on every generated case,
  and a corrupted copy is caught. It covers no locals, memory, or floats, so it is evidence
  on a subset.
* **Whole-function equivalence**, which needs the liveness computation the
  body-level statements take as a premise.
* **Trap position under the fuel-bounded run**, which needs the multi-block run
  rather than a single body.

## Modelled subset

The IR models the instructions that can *observe* something -- memory, globals,
calls, the host boundary -- plus enough arithmetic to have values to move around.
Absent, and listed so the gap is visible rather than assumed closed:

* `f32`/`f64` arithmetic, which needs IEEE 754 bit semantics;
* `select`, the integer conversions, and `br_table`;
* floating-point constants and the sign-extension operations;
* locals. The IR is SSA and models no `local.set`, which is a deliberate
  simplification: a local is storage that a branch may or may not have written,
  and modelling that distinction is a precondition for propagating a constant into
  a local's read -- which the Rust `const_prop` pass got wrong, and which the
  `local` case in `TptWasm.IRExec.intPayload` exists to keep visible.

## How this relates to the Rust checks

The formal claim and the executable check are the same claim at two granularities,
and neither is sufficient alone.

The Rust side runs on real modules. `tpt_wasm_opt::contract::verify_contract` is
called on every compilation, including in `EngineMode::Optimizing`, and the Core
spec suite runs through that mode with its directive counts re-asserted. It found
five real defects during M8, including a miscompile in `const_prop` that no
downstream check would have caught because the resulting IR was well-formed.

The Lean side says what the property is and why a violation matters. It cannot
run on a module, so it cannot find a violation; it establishes that the passes are
doing the right thing, which the Rust side can then check on every input.