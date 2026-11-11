# tpt-wasm-opt

Optimizing compiler passes over the TPT IR, for [TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

The Micro Interpreter is the golden machine. An optimization is correct only if the optimized
module is indistinguishable from the unoptimized one in every observable respect: values,
control flow, traps, memory effects, global effects, table effects, host effects, and the
order of all of them. The contract is checked in the `contract` module.

The pipeline takes an `IrModule` and returns a **new** `IrModule`. It never mutates its input,
so a caller can keep the original and compare the two. That comparison is how the contract
is tested.

> Pre-1.0. Applications reach these passes through the runtime's `Optimizing` engine mode.

## Optimization levels

`OptimizationLevel` selects the passes:

| Level | Passes |
|---|---|
| `None` | No passes. The module is returned unchanged, keeping the unoptimized path available for differential comparison. |
| `Fast` (default) | Constant folding and propagation, control-flow simplification, dead-code elimination, local and global value numbering (CSE), load/store optimization, bounds-check elimination, and instruction scheduling. |
| `Full` | Everything in `Fast`, plus register allocation. Register allocation runs last, after every pass that removes instructions, because it needs the final liveness. |

`optimize(&IrModule, level)` runs the pipeline to a fixed point and returns an
`OptimizationOutcome` with a report of which passes changed the module. Each pass can also be
called on its own from its module.

## Main modules

| Module | Contents |
|---|---|
| `analysis` | Control-flow graph, dominance, liveness, and removability checks |
| `const_fold`, `const_prop` | Constant folding and propagation (`fold_function`) |
| `graph` | Control-flow simplification |
| `dce` | Dead-code elimination (`eliminate`) |
| `lvn`, `memory` | Local and global value numbering, and memory-access optimization |
| `numeric` | Numeric rules shared by the folding passes |
| `sched` | Instruction scheduling |
| `regalloc` | Register allocation (`Full` only) |
| `contract` | The observable-behaviour contract that every pass must keep |

`COMPILER_VERSION` identifies the optimizer. It is part of the compilation cache key, so a
change to the passes invalidates cached output.

## Rules every pass follows

- A local is storage, not an SSA name. Re-assigning a local kills what the optimizer knew
  about it.
- A dead result is not a dead instruction. `may_trap`, `has_side_effects`, and
  `is_removable` decide what can go.
- Instruction order is observable. A pass may not move a faulting access past a store that
  could change its result, or hoist it above a store that could fault first.

## Testing

```sh
cargo test -p tpt-wasm-opt
```

Each pass is tested on its own, and the whole pipeline is compared against the unoptimized
module through the differential harness.

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
