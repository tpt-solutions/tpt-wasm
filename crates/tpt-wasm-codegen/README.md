# tpt-wasm-codegen

The baseline code generator for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

The baseline favours **correctness, startup time, predictable output, and easy debugging**
over peak performance. Peak performance is the optimizing pipeline's job
(`tpt-wasm-opt`).

```
Wasm → TPT IR → simple lowering → baseline form → execution
```

> Status: M7. The baseline lowers the straight-line MVP instruction set and the structured
> control flow that the IR represents, and runs it through a host boundary. **Native
> backends are pending.** `TargetArch` names the planned targets (`X86_64`, `Aarch64`,
> `Riscv64`), but no backend emits machine code yet. Do not rely on a native target.

## Main items

| Item | Role |
|---|---|
| `lower_module(&IrModule)` | Lower every function of a module to a `BaselineFunction` |
| `lower_function(&IrFunction)` | Lower one function |
| `BaselineFunction`, `BaselineBlock`, `BaselineModule` | The lowered form |
| `BaselineMemory`, `BaselineTable` | Linear memory and tables a lowered module runs against |
| `BaselineFunction::execute` / `execute_with` | Run a lowered function, with or without the module's function table |
| `HostBoundary` (trait) | The embedder's implementation of host calls |
| `HostCallFailure` | A failed host call |
| `CodegenError`, `BaselineError` | Why lowering or execution failed |

Execution limits are part of the baseline's contract:

- `set_max_call_depth` bounds the call stack, so runaway recursion becomes a trap instead of
  exhausting the host stack.
- `set_max_execution_steps` bounds the instruction budget, and `call_counted` reports how many
  steps a call used.

Shared memories, tables, and globals (`SharedMemory`, `SharedTable`, `SharedGlobal`) let
several instances use the same state.

## Example

```rust
use tpt_wasm_codegen::lower_module;
use tpt_wasm_ir::IrModule;

fn lower(ir: &IrModule) {
    let functions = lower_module(ir).expect("the IR is in the supported subset");
    println!("lowered {} functions", functions.len());
}
```

## Testing

```sh
cargo test -p tpt-wasm-codegen
```

The baseline is checked against the Micro Interpreter by the core spec suite and by the
differential harness, which compares both backends on generated programs.

## Dependencies

- `tpt-wasm-types`
- `tpt-wasm-ir`

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
