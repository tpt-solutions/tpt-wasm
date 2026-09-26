# Baseline Code Generation

**Status:** M7 — portable baseline slice for the straight-line MVP instruction set
and structured control flow represented by the IR; native backends pending

`tpt-wasm-codegen` currently provides a deterministic, portable lowering artifact
for numeric functions. It is independent of Micro and does not claim to produce
executable native code yet.

## Pipeline

```text
VerifiedIrModule
  -> lower_function
  -> BaselineFunction (blocks of dense value slots + BaselineOp)
  -> independent baseline execution
```

`lower_function` accepts multi-block functions. Blocks keep the IR's order and
are addressed by index; each block's parameters are slots that an incoming edge
binds. An edge carries the slots of the operands for its target, mirroring the
IR's block-argument phis.

`lower_function` rejects unknown or mistyped values, malformed control flow
(a branch to a missing block, a duplicate block id, an edge whose arity does not
match its target, a missing entry block), and unsupported IR instructions,
instead of dropping behavior. The baseline executor walks the block graph and
supports return, trap, unreachable, unconditional, and two-way branches, and is
covered by Micro differential tests including a loop that requires following a
back edge.

## Memory and globals

`BaselineModule` owns the functions plus the memory and globals they share. State
is shared across calls the way it is in Wasm: a callee observes the caller's
stores, and a `global.set` inside a callee is visible to the caller.

The memory is a `SharedMemory` handle rather than an owned value, because a
module that *imports* a memory has to reach the same one the exporting instance
holds, not a copy of it — a copy would pass every type check and then silently
lose every write. `memory_handle` publishes the handle and `set_memory` installs
one, which is how `tpt-wasm-runtime` wires a cross-instance import on this
backend. The handle is locked for the duration of a call rather than per access,
so the module sees its own stores and no observer can see a half-applied one.
Globals are shared the same way, as a `SharedGlobals` handle: a compiled module
that kept its own `Vec` of globals would diverge from the store's copy, and a
`global.set` in a compiled function would be invisible to any reader outside the
module. Tables are not shared this way yet.

`execute` and `execute_with` have no module, so a function that touches memory or
globals reports that it needs one rather than reading uninitialized data. Use
`BaselineModule::lower` followed by `call`.

## The host boundary

A compiled module reaches outside itself through exactly one instruction,
`CallHost`, and only through an installed `HostBoundary`. That is deliberate: a
host effect is an explicit, grantable step, so a compiled module cannot acquire a
capability the embedder did not hand it. Without a boundary, a `CallHost` is
refused rather than resolved by guesswork.

The host's return is not trusted. Its arity and types are re-checked against the
import's declared signature, because the host is outside the verified module.

A host failure is reported as `BaselineError::Host`, not as a Wasm trap. The two
are different things: a trap is the module's own fault, while a host failure is
the embedder refusing a capability. The interpreter already reports them
differently, so the baseline keeps them apart rather than flattening one into the
other. `tpt-wasm-runtime` maps `BaselineError::Host` back onto the same
`RuntimeError::HostFunction` the interpreter produces, name and message
included, so an embedder sees one error shape from both backends.

The runtime routes the baseline's host calls through `Instance::invoke_host_call`,
the same function the interpreter's `Step::HostCall` resumes through. Sharing
that path is what makes host-effect equivalence a property of the code rather
than a coincidence of two tests.

Loads read little-endian bytes and apply the opcode's extension: a float load is a
pure bit reinterpretation, so a NaN payload survives a round trip through memory,
and an out-of-bounds access traps with `MemoryOutOfBounds` rather than reading
adjacent memory. `memory.grow` returns the previous page count, or -1 when the
request would exceed the declared maximum.

## Calls

A `call` names a function by module index, so the functions of a module are
lowered together with `lower_module` and executed through
`BaselineFunction::execute_with`, which takes the function table. `execute` has
no table, so a function containing a call reports that the target is missing
rather than guessing.

Each callee allocates its own slot frame, so recursion and re-entrancy behave as
they do in Wasm. A callee's arguments and results are type-checked against the
slots, and a result of the wrong type is rejected rather than stored.

A callee is run by recursing, so the call depth is bounded. `ExecState` counts
the live frames — the entry call counts as one, exactly as the interpreter counts
the frame it pushes — and a call past `max_call_depth` traps with
`Trap::CallDepthExceeded` instead of being made. Without that bound a runaway
recursion exhausts the *host* stack and aborts the process, which is a crash
where WebAssembly requires a trap and a denial of service for anything embedding
the engine. The limit defaults to `ResourceLimits::default().max_call_depth` and
the runtime overrides it with the same number it gave the store, so the two
backends run out of depth on the same call.

`ResourceLimits::max_execution_steps` bounds the *work* rather than the depth, and
is enforced the same way on both backends. `ExecState` holds one `StepBudget`
shared by every frame of an execution, not one per frame — a fresh allowance per
call would let a recursive function run forever — and `charge` is called once per
instruction, *before* the instruction runs, from the single place that dispatches
ops so no new variant can skip it. Charging first is what makes the two backends
exhaust the same budget at the same instruction rather than one apart. `None` is
the default and means unbounded, which is a deliberate choice rather than an
omission: the limit is the embedder's to set, and a module that does not terminate
runs until the host stops it.

## Engine integration

`EngineMode::Baseline` compiles a module through `validate` → IR → baseline at
instantiation and executes the compiled form. The compiled module is owned by
the `Instance`, so its memory, table, and globals persist across calls exactly
as the store-backed path does.

A module the baseline cannot represent is refused at instantiation, not at call
time, so a baseline instance is never left half-usable. Imported *functions* lower
to `CallHost` and run through the host boundary, which the runtime installs once
the imports have resolved. The remaining gaps are cross-instance imports, and
imported tables, memories, and globals; a module using one of those is rejected in
baseline mode and still runs under Micro.

Exports are metadata rather than code. The runtime resolves them against the
module's own function table, so the IR does not carry them and lowering no
longer rejects a module for having some.

`EngineMode::Optimizing` remains unimplemented and is still refused at engine
construction.

## Active data segments

`IrMemory::segments` carries the module's active data segments as
`(offset, bytes)` pairs, in module order. `BaselineModule::new` applies them to
the freshly allocated memory before any function can run, so the ordering rule
the specification requires is structural rather than incidental: a later segment
overwrites an earlier one at the same address. Growth appends zeroed pages and
leaves the segment's bytes untouched. An *imported* memory's segments are held
rather than applied at construction — the memory does not exist yet — and are
written by `set_memory` the moment it arrives, which is still before any function
can run and after the exporting instance's own segments, as required.

A segment that would not fit the declared memory is rejected as a lowering
error. The validator already bounds-checks it, so reaching that arm means the two
disagree — the same reasoning used for element segments.

## Tables and indirect calls

`IrModule` carries the MVP table declaration together with the function indices
from its single active element segment, so the baseline never re-reads the Wasm
module. `BaselineModule::new` seeds a `Vec<Option<u32>>` from that declaration:
`None` is a null reference, and any slot the segment does not reach stays null.

`call_indirect` names a *type*, not a function, so the signature is checked
against whatever entry the table holds and the trap fires when they disagree —
the same order of operations Micro uses. Two failure modes are deliberately
distinct: an index past the end of the table is `TableOutOfBounds`, while an
index inside it that was never written is `NullReference`.

Resolution order matters in the lowering. The table index sits *above* the
arguments on the operand stack, so it is popped first, then the arguments
topmost-first, and only then are the results allocated and pushed — a result of
the same type as the index would otherwise be popped as the index.

## Control flow

The executor keeps a `current` block index and loops until a block ends in
`Return`, `Trap`, or `Unreachable`. `Branch` and `CondBranch` set the next block
after copying the edge's operands into the target's parameter slots. A block
with no terminator is reported as a lowering bug rather than silently accepted.

There is deliberately no step limit, so a non-terminating program loops forever
just as it does under Micro. A fuel bound would make the baseline and the golden
machine disagree on exactly those programs, which differential testing must not
allow.

## Implemented operation set

The baseline slice covers the full straight-line numeric sets for `i32`, `i64`,
`f32`, and `f64`:

- `i32.const` and `i64.const`
- all fifteen `i32` binary operations: add, sub, mul, div_s, div_u, rem_s, rem_u,
  shl, shr_s, shr_u, rotl, rotr, and, or, and xor
- all fifteen `i64` binary operations, with the same operation set
- `i32.eqz` and `i64.eqz`
- all ten `i32` comparisons and all ten `i64` comparisons: eq, ne, lt_s, lt_u,
  gt_s, gt_u, le_s, le_u, ge_s, and ge_u
- `f32.const` and `f64.const`
- all seven `f32` binary operations: add, sub, mul, div, min, max, and copysign
- all seven `f64` binary operations, with the same operation set
- all seven `f32` unary operations: abs, neg, ceil, floor, trunc, nearest, and sqrt
- all seven `f64` unary operations, with the same operation set
- all six `f32` comparisons and all six `f64` comparisons: eq, ne, lt, gt, le, ge
- `drop` and `select`
- `local.get`, `local.set`, and `local.tee`
- `clz`, `ctz`, and `popcnt` at both integer widths
- `i32.wrap_i64`, `i64.extend_i32_s`, and `i64.extend_i32_u`
- all four reinterpret operations: `i32.reinterpret_f32`, `i64.reinterpret_f64`,
  `f32.reinterpret_i32`, and `f64.reinterpret_i64`
- all ten non-trapping float conversions, and all eight trapping float
  truncations

Each width has its own `I32BinaryOp` / `I64BinaryOp` / `F32BinaryOp` /
`F64BinaryOp` enumeration, and all binary operations at a width share one
`BaselineOp::*Binary` form, so the slot layout is identical regardless of the
operation.

The backend reproduces Wasm's exact rules rather than Rust's defaults:

- arithmetic wraps instead of panicking;
- shift and rotation counts are masked to 5 bits at `i32` width and 6 bits at
  `i64` width;
- signed and unsigned shifts, division, and remainder are distinct operations;
- division and remainder by zero trap with `IntegerDivisionByZero`; and
- signed division of the minimum value by `-1` traps with `IntegerOverflow`
  (`i32`/`i64`), while the matching remainder yields `0`.

Comparisons and `eqz` always produce an `i32` result, matching Wasm, even when
their operands are `i64` or a float type.

## Floating-point semantics

Floating-point values are carried as raw bit patterns end to end, so NaN payloads
and signed zeros survive lowering, execution, and comparison unchanged. The
backend reproduces Micro's IEEE 754 behavior exactly:

- arithmetic results that are NaN are canonicalized to the quiet NaN
  (`0x7fc0_0000` for `f32`, `0x7ff8_0000_0000_0000` for `f64`), because Wasm
  leaves NaN payloads non-deterministic and this backend fixes on one choice in
  order to stay bit-comparable with the golden machine;
- `abs`, `neg`, and `copysign` are defined bitwise and therefore *preserve* the
  NaN payload instead of canonicalizing it;
- `min` propagates NaN, resolves `min(-0, +0)` to `-0`, and `max` resolves
  `max(-0, +0)` to `+0`;
- `nearest` rounds halfway cases to the nearest even integer rather than away
  from zero; and
- `ceil`, `floor`, `trunc`, and `sqrt` canonicalize a NaN result but leave
  infinities and signed zeros intact.

Comparisons use the IEEE unordered rule directly: any comparison involving NaN
is false, except `ne`, which is true.

## Locals, dataflow, and conversions

Locals are pre-allocated slots, zero-initialized at function entry exactly as
Wasm requires. `local.get` copies a slot, `local.set` writes one, and `local.tee`
writes the local and yields the same value as an SSA result. `drop` ends a
value's live range without disturbing the slot, and `select` picks an arm from
its `i32` condition; both arms are type-checked to agree at lowering time.

Reinterpretation moves raw bits and never inspects or changes them, so a NaN
payload or a signed zero survives a float↔integer round trip unchanged — unlike
a numeric conversion, which would normalize it.

Trapping float truncations truncate toward zero and then range-check the result.
NaN, either infinity, an out-of-range magnitude, and a negative value for an
unsigned destination all raise `InvalidConversion`; none of them panic. The
unsigned bounds are half-open, so the largest representable `u32`/`u64` is
accepted while the next value up traps.

## Not yet lowered

`externref`/reference-typed values and host effects are still rejected with
`UnsupportedInstruction` rather than approximated, as are the post-MVP proposals
(`multi-memory`, `memory64`, passive data segments, passive and declarative
element segments, exception handling). A native `CompiledFunction` is reserved
for the later executable-memory boundary and is not produced by this portable
slice.

## Differential testing

Tests drive the full production pipeline — Wasm body → `validate` →
`lower_and_verify` → `lower_function` — and then execute the resulting
`BaselineFunction` against the Micro interpreter for the same body, asserting
identical results and identical traps. Coverage spans wrapping arithmetic, masked
shift and rotation counts at both widths, every division and remainder trap
identity, both `eqz` forms, and every signed and unsigned comparison across sign
boundaries.

Float coverage drives the same pipeline with `f32.const`/`f64.const` fixtures
written as raw little-endian bit patterns, so NaN payloads and signed zeros reach
both backends unmodified. It exercises every `f32` and `f64` binary, unary, and
comparison opcode over ordinary values, ±0, a NaN payload, and infinities, and
asserts the Wasm-specific identities directly: canonical NaN propagation through
arithmetic, NaN payload preservation through `abs`/`neg`/`copysign`, the signed
zero rules for `min` and `max`, ties-to-even `nearest`, and the unordered NaN
comparison rule.

Dataflow and conversion coverage runs through the same pipeline, declaring
locals in the module so both backends zero-initialize them identically. It
asserts the `local.get`/`set`/`tee` round trip, that an unset local reads as
zero, both `select` arms, that a `drop` does not disturb later results, all
three bit-count operations at both widths, sign- versus zero-extension, that
reinterpretation round-trips a NaN payload and a signed zero, all ten
non-trapping float conversions, and every float truncation trap boundary —
including that `InvalidConversion`, not a panic, is raised for NaN, infinity,
out-of-range magnitudes, and negative-to-unsigned conversions.

Control-flow, memory, global, and table coverage runs through the same pipeline.
It asserts a loop that only terminates if the back edge is followed, `br` carrying
a value, `br_if` on both the taken and the falling-through path, and `if`/`else`
on both edges; that a direct call forwards arguments and results; that an active
element segment seeds the table and `call_indirect` forwards its arguments;
that out-of-range, null, and signature-mismatched table entries each trap
distinctly and like Micro; that a data segment lands at a requested non-zero
offset and that a later segment overwrites an earlier one; that the fourteen
loads and nine stores, `memory.size`, and `memory.grow` agree with Micro and
trap out of bounds identically; that a memory instruction in a module with no
memory traps; that `global.get`/`global.set` read and write the same
module-level slots in both backends; and that `ref.null`, `ref.func`, and
`ref.is_null` agree with Micro, whether the reference is read back from a local,
carried through a block result, returned unchanged, or used to pick a branch.

### Generated programs

The fixtures above are cases someone thought to write. On top of them, a seeded
generator builds whole programs and checks each one the same way. It draws from
twenty-two instruction families — integer, float, and bit-count operations at
both widths, division and remainder with the sign variants distinguished, the
non-trapping and trapping conversions, memory round trips, `block` with a
conditional branch, `if`/`else`, a counted `loop`, a direct `call`, an indirect
`call_indirect`, a `br_table` over two nested blocks, and the reference
instructions both directly and carried out of a block — over a page of memory,
one mutable global, a helper function, and a table seeded with that helper. Each
program goes through the real pipeline and is compared with Micro for both its
value and its trap.

The generator keeps its own shadow of the operand stack so it only emits an
operation whose operands it actually produced; that is what lets it stay type
correct without consulting the validator, and a `pop` that runs past the bottom
is a bug rather than a malformed program. Each structured case restores the
shadow at the block's entry height, and the `else` arm restarts from that height
rather than continuing from the `then` arm, because the two are separate paths.
A seed replays deterministically, and a failure reports the seed and the body
bytes, so a divergence reproduces without saving an artifact. Twenty-five
thousand programs run as an ordinary test; the sweep has been taken to two
hundred fifty thousand locally.

Not yet generated: multi-value block results. Everything else in the represented
surface is covered by the population.

#### What the generator found

Extending the generator to control flow surfaced defects that the hand-written
fixtures had missed, all from one mistaken reading of `br_if`:

- The **validator** dropped the label's values on the fall-through path instead
  of restoring them, so it rejected every valid `br_if` to a label that carries
  a value — and accepted the invalid mirror image. A test had encoded that
  behavior as intended.
- The **IR lowerer** gave the `br_if` fall-through edge no values at all, so a
  value the fall-through still had to produce was silently lost.
- The **`br_if_body` fixture** in this file's own test suite was invalid Wasm
  that only validated because of the validator bug.

The mistake was invisible until the generator started emitting blocks and
branches, because every hand-written fixture branched only to void labels, where
the two readings agree.

Adding `br_table` found two more in the comparison chain the lowerer builds for
it, one of them a plain misreading of the instruction's own type:

- The lowerer popped the **label values before the selector**, but `br_table` is
  `[t* i32] -> [t*]`, so the selector is on top. The two were silently swapped,
  so the branch carried the selector and the dispatch compared the carried
  value. Any selector differing from the value it carried therefore branched to
  the wrong label. A hand-written fixture had used a selector equal to its
  carried value, so the swap was invisible there too.
- Each arm of the chain passed no values on its fall-through edge, and later arms
  named values that did not dominate them. The values are now threaded through as
  each block's parameters.

The generator itself had four more: a `call_indirect` that never pushed the
callee's argument; a counted `loop` that computed its decrement but never stored
it back, so the counter never changed and the loop never terminated; a nested
loop sharing the outer's counter local, leaving it at zero and counting down
through the whole `i32` range; and an `else` arm continuing from the `then` arm's
stack instead of restarting at the block's entry height.

Native x86-64/AArch64 code generation and executable-memory integration remain
pending. `EngineMode::Baseline` is wired: the module is compiled through validate
to IR to baseline at instantiation and executed by the compiled form, and the
compiled instance owns that module's memory, table, and globals so state persists
across calls.
