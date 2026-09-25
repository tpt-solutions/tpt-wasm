# Baseline Code Generation

**Status:** M7 — portable baseline slice implemented; native backends pending

`tpt-wasm-codegen` currently provides a deterministic, portable lowering artifact
for straight-line `i32` functions. It is independent of Micro and does not claim to
produce executable native code yet.

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

The baseline slice covers the full straight-line `i32` and `i64` numeric sets:

- `i32.const` and `i64.const`
- all fifteen `i32` binary operations: add, sub, mul, div_s, div_u, rem_s, rem_u,
  shl, shr_s, shr_u, rotl, rotr, and, or, and xor
- all fifteen `i64` binary operations, with the same operation set
- `i32.eqz` and `i64.eqz`
- all ten `i32` comparisons and all ten `i64` comparisons: eq, ne, lt_s, lt_u,
  gt_s, gt_u, le_s, le_u, ge_s, and ge_u

Each width has its own `I32BinaryOp` / `I64BinaryOp` enumeration, and all binary
operations at a width share one `BaselineOp::*Binary` form, so the slot layout is
identical regardless of the operation.

The backend reproduces Wasm's exact rules rather than Rust's defaults:

- arithmetic wraps instead of panicking;
- shift and rotation counts are masked to 5 bits at `i32` width and 6 bits at
  `i64` width;
- signed and unsigned shifts, division, and remainder are distinct operations;
- division and remainder by zero trap with `IntegerDivisionByZero`; and
- signed division of the minimum value by `-1` traps with `IntegerOverflow`
  (`i32`/`i64`), while the matching remainder yields `0`.

Comparisons and `eqz` always produce an `i32` result, matching Wasm, even when
their operands are `i64`.

## Differential testing

Tests drive the full production pipeline — Wasm body → `validate` →
`lower_and_verify` → `lower_function` — and then execute the resulting
`BaselineFunction` against the Micro interpreter for the same body, asserting
identical results and identical traps. Coverage spans wrapping arithmetic, masked
shift and rotation counts at both widths, every division and remainder trap
identity, both `eqz` forms, and every signed and unsigned comparison across sign
boundaries.

Native x86-64/AArch64 code generation, executable-memory integration, and
`EngineMode::Baseline` remain pending. The native `CompiledFunction` type is
reserved for that later boundary and is not produced by this portable slice.
