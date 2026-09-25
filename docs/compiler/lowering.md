# Wasm-to-IR Lowering

**Status:** M6 — straight-line subset implemented

## Input contract

`lower_module` accepts only `ValidatedModule`. Callers cannot fabricate a
validation certificate outside `tpt-wasm-validate`, and lowering does not
reinterpret untrusted bytes. The input module remains structurally separate
from the Micro interpreter.

## Pipeline

```text
ValidatedModule
  -> reject unsupported module state
  -> resolve each defined function type
  -> parse function-body immediates
  -> allocate typed SSA-ready result IDs
  -> lower the supported instruction subset
  -> check the function result stack
  -> build one terminal basic block
  -> verify the complete IR module
  -> return VerifiedIrModule
```

`lower_and_verify` is the preferred compiler entry point because it does not
expose an unverified intermediate module.

## Current module-state gate

The initial slice rejects modules containing imports, tables, memories,
globals, element segments, data segments, start functions, or any exports.
Custom sections do not affect execution and are not copied into the IR.

These checks are conservative. The lowerer returns a typed
`LoweringError::UnsupportedFeature` rather than dropping state or names that
could affect linking, initialization, or observable behavior.

## Current instruction mapping

| Wasm opcode | IR form | Notes |
|---|---|---|
| `unreachable` | `Terminator::Unreachable` | Trailing validated bytes are unreachable and ignored. |
| `nop` | erased | No semantic effect. |
| `end` | `Terminator::Return` | Requires exactly the declared result values. |
| `return` | `Terminator::Return` | Requires exactly the declared result values. |
| `drop` | `Drop` | Removes one value from the lowering stack. |
| `select` | `Select` | Requires an i32 condition and two equal-typed numeric arms. |
| `local.get`, `local.set`, `local.tee` | `LocalGet`, `LocalSet`, `LocalTee` | Straight-line local slots are stable; `set` has no result and `tee` returns the stored value. |
| `call` | `Call` | Direct calls to defined functions carry typed argument/result IDs; imported calls are rejected. |
| `i32.const` | `ConstI32` | Allocates an `i32` result ID. |
| `i64.const` | `ConstI64` | Allocates an `i64` result ID. |
| `f32.const` | `ConstF32` | Preserves raw IEEE 754 bits. |
| `f64.const` | `ConstF64` | Preserves raw IEEE 754 bits. |
| `i32` eqz/eq/ne/lt_s/lt_u/gt_s/gt_u/le_s/le_u/ge_s/ge_u | `I32Eqz`, `I32Compare` | Comparisons produce `i32`; signed and unsigned ordering are explicit. |
| `i64` eqz/eq/ne/lt_s/lt_u/gt_s/gt_u/le_s/le_u/ge_s/ge_u | `I64Eqz`, `I64Compare` | Comparisons produce `i32`; signed and unsigned ordering are explicit. |
| `i32` clz/ctz/popcnt | `I32Unary` | Count leading/trailing zero bits and set bits. |
| `i64` clz/ctz/popcnt | `I64Unary` | Count leading/trailing zero bits and set bits. |
| `i32.wrap_i64`, `i64.extend_i32_s`, `i64.extend_i32_u` | `IntConvert` | Typed non-trapping integer width and signedness conversions. |
| `i32.reinterpret_f32`, `i64.reinterpret_f64`, `f32.reinterpret_i32`, `f64.reinterpret_i64` | `Reinterpret` | Same-width raw-bit reinterpretation; payload and NaN bits are preserved. |
| `i32` add/sub/mul/div_s/div_u/rem_s/rem_u/shl/shr_s/shr_u/rotl/rotr/and/or/xor | `I32Add`, `I32Sub`, `I32Mul`, `I32DivS`, `I32DivU`, `I32RemS`, `I32RemU`, `I32Shl`, `I32ShrS`, `I32ShrU`, `I32Rotl`, `I32Rotr`, `I32And`, `I32Or`, `I32Xor` | Integer arithmetic, traps, shifts/rotations, and bitwise operations. |
| `i64` add/sub/mul/div_s/div_u/rem_s/rem_u/shl/shr_s/shr_u/rotl/rotr/and/or/xor | `I64Add`, `I64Sub`, `I64Mul`, `I64DivS`, `I64DivU`, `I64RemS`, `I64RemU`, `I64Shl`, `I64ShrS`, `I64ShrU`, `I64Rotl`, `I64Rotr`, `I64And`, `I64Or`, `I64Xor` | Integer arithmetic, traps, shifts/rotations, and bitwise operations. |
| `f32`/`f64` integer conversions and demote/promote | `FloatConvert` | Non-trapping IEEE 754 conversion; NaN outputs use the canonical runtime NaN policy. |
| `i32`/`i64` conversions from `f32`/`f64` | `FloatTrunc` | Truncates toward zero; NaN, infinity, and out-of-range values trap with `InvalidConversion`. |
| `f32` eq/ne/lt/gt/le/ge | `F32Compare` | Comparisons produce `i32`; NaN follows WebAssembly unordered rules. |
| `f64` eq/ne/lt/gt/le/ge | `F64Compare` | Comparisons produce `i32`; NaN follows WebAssembly unordered rules. |
| `f32` abs/neg/ceil/floor/trunc/nearest/sqrt | `F32Unary` | IEEE 754 unary operations; NaN payload/sign handling is preserved where required. |
| `f64` abs/neg/ceil/floor/trunc/nearest/sqrt | `F64Unary` | IEEE 754 unary operations; NaN payload/sign handling is preserved where required. |
| `f32` min/max/copysign | `F32Min`, `F32Max`, `F32Copysign` | IEEE 754 binary operations; NaN and signed-zero behavior follows Micro. |
| `f64` min/max/copysign | `F64Min`, `F64Max`, `F64Copysign` | IEEE 754 binary operations; NaN and signed-zero behavior follows Micro. |
| `f32` add/sub/mul/div | `F32Add`, `F32Sub`, `F32Mul`, `F32Div` | Float arithmetic is lowered, evaluated, and projected into the executable formal model. |
| `f64` add/sub/mul/div | `F64Add`, `F64Sub`, `F64Mul`, `F64Div` | Float arithmetic is lowered, evaluated, and projected into the executable formal model. |

All other opcodes return `LoweringError::UnsupportedInstruction(opcode)`.

## Value and result handling

Function parameters and declared locals receive stable value IDs first.
Instruction results then receive fresh IDs. The lowering stack stores typed
value IDs while consuming Wasm's stack-ordered operands. At a return, the
lowering stack must contain exactly the function result count and types.

The structural verifier repeats these checks over the resulting IR. It also
rejects unknown values, duplicate definitions, use-before-definition, bad
parameter metadata, and unsupported multi-block functions.

## Differential testing

`tpt-wasm-ir` unit tests use Micro only as a dev-dependency. Each supported
fixture is:

1. decoded structurally by constructing the same validated module;
2. lowered and verified as IR;
3. executed by a small test-only IR evaluator;
4. executed by the Micro interpreter; and
5. compared for identical return values or trap identity.

The fixtures cover constants, integer arithmetic, shifts, comparisons, bitwise
AND/OR/XOR, integer unary bit-count, integer conversion, raw-bit reinterpretation,
non-trapping float conversion, trapping float-to-integer conversion, float
comparisons, straight-line local get/set/tee, defined direct calls, `drop`, `nop`,
explicit return, unreachable code, unsupported module state, and malformed
hand-built IR.

## Next lowering work

Future passes must add, in dependency order:

1. block parameters and multi-block local dataflow, plus global operations;
2. branch-aware CFG construction and dominance verification;
3. imported and indirect calls;
4. explicit traps and checked memory operations;
5. tables and references;
6. remaining numeric operations; and
7. explicit host boundaries.

Each addition requires an IR instruction form, verifier rule, lowering test,
Micro differential test, and scope documentation before its tracker item can be
checked.