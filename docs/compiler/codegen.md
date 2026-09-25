# Baseline Code Generation

**Status:** M7 — portable baseline slice for the straight-line MVP instruction set
represented by the IR; native backends pending

`tpt-wasm-codegen` currently provides a deterministic, portable lowering artifact
for straight-line numeric functions. It is independent of Micro and does not
claim to produce executable native code yet.

## Pipeline

```text
VerifiedIrModule
  -> lower_function
  -> BaselineFunction (dense value slots + BaselineOp)
  -> independent baseline execution
```

`lower_function` rejects multi-block functions, unknown or mistyped values, and
unsupported IR instructions instead of dropping behavior. The current baseline
executor supports return, trap, and unreachable terminators and is covered by
Micro differential tests.

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

Multi-block control flow, memory and table access, and direct or indirect calls
are still rejected with `UnsupportedInstruction` rather than approximated. A
native `CompiledFunction` is reserved for the later executable-memory boundary
and is not produced by this portable slice.

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

Native x86-64/AArch64 code generation, executable-memory integration, and
`EngineMode::Baseline` remain pending.
