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
| V4 IR refinement | **model + vocabulary only** | see below |
| V5 transformation correctness | **partial** | six theorems proved |
| V6 machine-code refinement | **not started** | see below |

### V4: what exists and what does not

`TptWasm.IR` models the IR (SSA values, blocks with parameters, terminators whose
arms bundle a target with the values binding it) and `TptWasm.IRExec` gives its
execution. That is the vocabulary V4 and V5 are stated in, and it is what a later
refinement theorem would quantify over.

The refinement theorem itself is **not proved**. The missing piece is the erasure
from IR back to a Wasm instruction sequence together with the theorem that the two
observe identically. The reason it is not written yet is specific rather than
general: `TptWasm.Transition` models Wasm execution over a *list of
instructions* with an operand stack, whereas the IR is in SSA form with block
parameters. Bridging them is a real theorem about the shape of the two, not a
definition, and it needs a typing theory to say which values a block parameter may
legitimately bind. Writing it before that theory would produce a theorem whose
premises were stronger than the claim, which is the failure mode this document
exists to prevent.

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
* **Dead-value elimination.** `TptWasm.Passes.deadBlock` is defined with the
  `extra` argument the liveness analysis must supply, but no correctness theorem
  over it is written.
* **CFG simplification.** Not started. This is the one pass of the three that
  moves control flow, so it needs the whole function rather than a block body, and
  the reachability argument the Rust `graph` pass had to get right (see the
  `thread_empty_blocks` and `merge_blocks` comments there for the two
  arity-substitution bugs the spec suite found).
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