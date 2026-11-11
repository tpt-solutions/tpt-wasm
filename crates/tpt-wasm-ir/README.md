# tpt-wasm-ir

The TPT intermediate representation for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

The IR is deliberately independent of the Micro Interpreter. It is the form that the
optimizer, the baseline compiler, and the verification layer all work on.

```
Wasm → ValidatedWasm → TPT-Wasm IR → { baseline, optimizing, verification }
```

Lowering is conservative. It accepts the instruction subset this IR version represents and
**rejects** every other construct with `LoweringError`, rather than producing IR that might
not mean the same thing.

> Pre-1.0. Applications normally reach the IR through the runtime's `Baseline` and
> `Optimizing` engine modes.

## Main types

| Type | Role |
|---|---|
| `IrModule` | A lowered module: functions, imports, globals, memories, tables, and data segments |
| `IrFunction`, `BasicBlock`, `BlockId` | A function as a control-flow graph of basic blocks |
| `IrInstr` | An instruction; each result is a value |
| `Terminator` | How a block ends: branch, return, trap, and so on |
| `IrImport`, `IrImportSource` | Imports, and where they come from |
| `IrGlobal`, `IrMemory`, `IrTable`, `IrDataSegment` | Module-level entities |
| `LoweringError` | Why a construct was refused |
| `VerificationError` | Why a lowered module failed IR verification |

## Main functions

- `lower_module(&ValidatedModule) -> Result<IrModule, LoweringError>` — lower a validated
  module to IR.
- `lower_and_verify` — lower, then run IR verification, and return a `VerifiedIrModule`
  holding the module and its certificate.
- `verify_module` — check an existing `IrModule`.
- `instruction_results`, `const_expr_i32` — helpers used by the other crates.

## Example

```rust
use tpt_wasm_ir::lower_module;
use tpt_wasm_validate::ValidatedModule;

fn lower(validated: &ValidatedModule) {
    let ir = lower_module(validated).expect("a module in the supported subset");
    println!("{} functions", ir.functions.len());
}
```

## Design rules

- **A local is storage, not an SSA name.** A local can be assigned again, so a value's
  definition does not fix its contents for the whole function.
- **Instruction order is observable.** Traps are trapping events; the position of a trap is
  part of the behaviour, and no pass may move it.
- **A dead result is not a dead instruction.** An unused `i32.div_s` still traps, and a call
  to an imported function still reaches the host.

The optimization passes in `tpt-wasm-opt` are written against these rules.

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-format`
- `tpt-wasm-validate`

## Testing

```sh
cargo test -p tpt-wasm-ir
```

The lowering is exercised by the core spec suite through the baseline backend, and by the
differential harness, which compares the backends on generated programs.

## Safety

No `unsafe` code.

## Minimum supported Rust version

Rust 1.80.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
Copyright (c) 2026 TPT Solutions. See the
[repository](https://github.com/tpt-solutions/tpt-wasm) for the license texts.
