# TPT-Wasm IR

**Status:** M6 — typed straight-line subset implemented
**Crate:** `tpt-wasm-ir`

## Position in pipeline

```text
Binary
  -> decode
Module
  -> validate
ValidatedModule
  -> lower + verify
VerifiedIrModule
  +-> baseline compiler
  +-> optimizing compiler
  +-> formal-model projection
```

## Implemented representation

The current IR is deliberately conservative and independently implemented from
the Micro interpreter:

- `IrModule` owns lowered function definitions.
- `IrFunction` records its Wasm signature, parameter and local value IDs, a
  typed value table, an entry block, and basic blocks.
- `ValueId` names each SSA-ready result. Floating-point constants retain raw
  bits, so NaN payloads are not changed during lowering.
- `BasicBlock` owns typed instructions and exactly one terminator.
- The current verifier accepts one terminal block per function. This matches
  the current straight-line lowering slice and rejects multi-block input until
  branch verification is implemented.
- `VerifiedIrModule` keeps its payload private. Consumers can inspect it or
  consume it into a mutable compiler input, but cannot mutate a certified module
  in place.

The implemented instruction set is:

- `i32`, `i64`, `f32`, and `f64` constants;
- `drop` and `local.get`, `local.set`, and `local.tee`;
- direct calls to defined functions;
- `select`;
- `i32`/`i64` add, subtract, multiply, divide/remainder, shifts, rotations, comparisons, AND, OR, and XOR;
- `i32`/`i64` `clz`, `ctz`, and `popcnt`;
- `i32.wrap_i64`, `i64.extend_i32_s`, and `i64.extend_i32_u`;
- non-trapping `f32`/`f64` integer conversions and `f32.demote_f64`/`f64.promote_f32`;
- trapping `i32`/`i64` conversions from `f32`/`f64` (finite/range-checked, `InvalidConversion` trap);
- `i32.reinterpret_f32`, `i64.reinterpret_f64`, `f32.reinterpret_i32`, and `f64.reinterpret_i64`;
- `f32`/`f64` comparisons (`eq`, `ne`, `lt`, `gt`, `le`, `ge`);
- `f32`/`f64` `abs`, `neg`, `ceil`, `floor`, `trunc`, `nearest`, and `sqrt`;
- `f32`/`f64` `min`, `max`, and `copysign`;
- `f32`/`f64` add, subtract, multiply, and divide;
- return, trap, and unreachable terminators.

`nop` is accepted and erased because it has no semantic effect.

## Type and structural verification

`verify_module` checks the properties needed by the current slice:

- value IDs are unique and declared in the function value table;
- parameter arity and parameter types match the Wasm signature;
- parameters and locals have unique definitions;
- every use is dominated by a definition in the single supported block;
- SSA result IDs are defined at most once;
- constants and arithmetic operands have the required Wasm types;
- direct-call targets exist and argument/result signatures match;
- return arity, value types, and definitions match the function signature;
- the entry block exists and the function has exactly one supported block.

`lower_and_verify` composes lowering and verification and returns the
certificate-backed `VerifiedIrModule`.

## Scope boundary

This is not yet a complete MVP compiler IR. Structured control flow,
multi-block local dataflow, globals, memory, tables, imported or indirect calls, references,
remaining numeric operations and explicit host boundaries are
not yet represented by the instruction set. Lowering rejects those constructs
explicitly; it never discards or approximates their behavior.

See [lowering.md](lowering.md) for the stage-by-stage contract and
[refinement.md](../verification/refinement.md) for the executable V4 projection
boundary.

## Independence

Production IR code does not depend on `tpt-wasm-micro`. Micro is present only
as a dev-dependency for differential tests. The IR receives structural module
data through `ValidatedModule`; it never imports interpreter state.

## Correctness relationship

```text
Core Wasm ~= TPT-Wasm IR ~= Optimized IR ~= Machine IR ~= Machine Code
```

Each `~=` boundary requires a proof or executable differential test. The IR is
a compilation representation and never becomes the definition of Wasm
semantics.
