# Refinement Boundary

The Rust abstract machine in `tpt-wasm-semantics` is the executable reference
for its modeled instruction subset. It is intentionally independent of the
Micro interpreter and compiler backends.

## V0: configuration invariants

`Configuration::step` returns one of:

```text
Transition::Step(Configuration)
Transition::Return(Vec<Value>)
Transition::Trap(Trap)
```

`tpt_wasm_verify::check_v0` checks structural invariants of the resulting
configuration for the modeled subset.

## V4: executable IR projection

`tpt_wasm_verify::project_function_to_model` accepts a certificate-backed
`VerifiedIrModule` and projects one function into
`tpt_wasm_semantics::FunctionState`.

The current projection covers the complete implemented IR instruction set:
constants, `drop`, `select`, straight-line local `get`/`set`/`tee`, defined direct
calls, integer arithmetic, shifts, rotations, bitwise operations
and integer unary bit-count, integer conversion, raw-bit reinterpretation, non-trapping float conversion, trapping float-to-integer conversion, float comparisons, float unary operations, float binary operations, float
arithmetic, return, and unreachable. Any future IR instruction
must be added here or the projection returns
`IrProjectionError::UnsupportedInstruction`; it is never skipped or approximated.

This hook makes the correspondence data executable and testable. It is not a
refinement proof and does not cover the full IR instruction set.

## Not yet established

The following remain pending:

```text
FormalRule(op) ~= MicroStep(op)   -- across the two languages
Valid(Module) ==> NoWasmOOB       -- needs the validation theory
Wasm ~= TPT IR
TPT IR ~= Optimized IR
Machine IR ~= Machine Code
```

The Lean model proves `FormalRule(op) ~= execOp(op)` for the instruction subset
it represents, which is the Rust model minus its float arithmetic and trapping
conversions. That is a rule-to-interpreter result inside one formalization, not a
proof that the Lean rules equal `Micro::step`; the two are separate
implementations that are kept in step by hand and checked executably on the Rust
side. The IR refinement obligations are unchanged and remain M9 work.
