# Wasm-to-IR Lowering

**Status:** M6 — MVP surface implemented: structured control flow, locals,
globals, linear memory, tables, and calls

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
  -> lower globals, memory + active data segments, and table + element segment
  -> parse function-body immediates
  -> allocate typed SSA-ready result IDs
  -> lower the instruction subset, building a multi-block CFG
  -> check the function result stack
  -> verify the complete IR module
  -> return VerifiedIrModule
```

`lower_and_verify` is the preferred compiler entry point because it does not
expose an unverified intermediate module.

## Current module-state gate

The lowerer rejects a module containing:

- imports, so a `call` always resolves to a defined function;
- start functions;
- passive data segments, and a non-zero data-segment memory index;
- more than one active element segment, or a non-zero table index;
- multiple memories, a 64-bit memory, multiple tables, or a non-`funcref` table;
- an element or data segment whose offset is not a constant `i32`;
- a global initializer that is not a constant of the global's own type;
- a `block` type the IR cannot represent.

Exports are deliberately *not* rejected. They are module metadata rather than
code, and the runtime resolves them against the module's own function table, so
the IR does not need to carry them.

Everything else is carried into the IR: the linear memory with its active data
segments in module order, the table with its initial function indices, the module
function types, and the global types with their constant initial values.

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
| `block` | `Branch` on a new block | The label's parameters become the target block's parameters. |
| `loop` | `Branch` back to the header | The header is the branch target and the exit is a distinct block, so a back edge and a fall-out are different edges. |
| `if`/`else` | `CondBranch` | Each arm ends with a branch; the merge block takes the label's parameters. |
| `br` | `Branch` | Carries the label's values into the target's parameters. |
| `br_if` | `CondBranch` | The condition and the label's values are consumed on both the taken and the fall-through path. |
| `br_table` | a chain of `CondBranch` blocks | The selector is spilled to a synthesized local and re-read per comparison; the last label is the default reached by falling off the chain. |
| `drop` | `Drop` | Removes one value from the lowering stack. |
| `select` | `Select` | Requires an i32 condition and two equal-typed numeric arms. |
| `local.get`, `local.set`, `local.tee` | `LocalGet`, `LocalSet`, `LocalTee` | A local is one function-scoped slot with a stable value ID; `set` has no result and `tee` returns the stored value. |
| `call` | `Call` | Direct calls to defined functions carry typed argument/result IDs; imported calls are rejected. |
| `call_indirect` | `CallIndirect` | Names a type rather than a function, so the signature is checked against the table entry at run time. |
| `i32.const` | `ConstI32` | Allocates an `i32` result ID. |
| `i64.const` | `ConstI64` | Allocates an `i64` result ID. |
| `f32.const` | `ConstF32` | Preserves raw IEEE 754 bits. |
| `f64.const` | `ConstF64` | Preserves raw IEEE 754 bits. |
| all fourteen `i32`/`i64`/`f32`/`f64` loads | `Load` | Explicit width and sign/zero-extension per operation, with a static offset. |
| all nine stores | `Store` | Explicit width and operand type per operation. |
| `memory.size`, `memory.grow` | `MemorySize`, `MemoryGrow` | `grow` yields the previous size or -1. |
| `global.get`, `global.set` | `GlobalGet`, `GlobalSet` | Checked against the global's declared type and mutability. |
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
parameter metadata, unreachable blocks, a use not dominated by its definition,
and a branch edge whose value count does not match the target's parameters.

A Wasm local keeps one stable value ID for the whole function rather than being
SSA-renamed, because it is a storage slot that `local.set` re-assigns. Two places
depend on that: the lowerer spills a `br_table` selector into a synthesized local
so each comparison block can re-read it, and the verifier lets a local written
inside a branch be read after the merge.

## Differential testing

`tpt-wasm-ir` unit tests use Micro only as a dev-dependency. Each supported
fixture is:

1. decoded structurally by constructing the same validated module;
2. lowered and verified as IR;
3. executed by a small test-only IR evaluator;
4. executed by the Micro interpreter; and
5. compared for identical return values or trap identity.

The fixtures cover constants, integer arithmetic, shifts, rotations, comparisons,
bitwise AND/OR/XOR, integer unary bit-count, integer conversion, raw-bit
reinterpretation, non-trapping float conversion, trapping float-to-integer
conversion, float comparisons and arithmetic, local get/set/tee, defined direct
calls, `call_indirect` through an active element segment, `drop`, `select`,
`nop`, structured control flow (`block`, `loop`, `if`/`else`, `br`, `br_if`,
`br_table` including the back edge and out-of-range default), the linear memory
with all fourteen loads and nine stores plus `memory.size`/`memory.grow`,
`global.get`/`global.set`, active data segments, explicit return, unreachable
code, unsupported module state, and malformed hand-built IR.

## Next lowering work

Items 1 through 5 of the original plan are done: multi-block CFG construction
with block parameters and dominance verification, defined direct calls,
`call_indirect`, the linear memory, globals, and tables. What remains, in
dependency order:

1. reference instructions (`ref.null`, `ref.func`, `ref.is_null`) and an IR
   `funcref` value;
2. imported-function resolution, which unblocks the baseline's host boundary;
3. explicit host-effect instructions, so a compiled module can call a granted
   capability;
4. passive and multiple element/data segments, and non-`funcref` tables.

Each addition requires an IR instruction form, verifier rule, lowering test,
Micro differential test, and scope documentation before its tracker item can be
checked.