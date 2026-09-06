# tpt-wasm

**TPT-Wasm** is an independent Rust implementation of WebAssembly built around a minimal
executable semantic core, capability-oriented host, formal verification layer, and native compiler.

> This is not a Wasmtime alternative. It is not a Wasmtime fork.  
> It is a specification-first WebAssembly execution system.

## Architecture

```
                       WEBASSEMBLY CORE STANDARD
                                  │
                    ┌─────────────▼─────────────┐
                    │      TPT-WASM SEMANTICS    │
                    │  types · validation ·      │
                    │  instantiation · execution │
                    └──────────────┬─────────────┘
                                   │
             ┌─────────────────────┼──────────────────┐
             │                     │                  │
             ▼                     ▼                  ▼
      ┌────────────┐      ┌──────────────┐   ┌──────────────┐
      │   MICRO    │      │    FORMAL    │   │      IR      │
      │INTERPRETER │      │    MODEL     │   │   COMPILER   │
      └────────────┘      └──────────────┘   └──────────────┘
             │                     │                  │
             └─────────────────────┼──────────────────┘
                                   │
                        ┌──────────▼──────────┐
                        │    TPT-WASM HOST    │
                        │  capabilities ·     │
                        │  resources ·        │
                        │  determinism · WASI │
                        └──────────┬──────────┘
                                   │
                             tpt-system
                                   │
                      physics · FEM · robotics · ...
```

### The four layers

| Layer | Crate | Question |
|---|---|---|
| Micro Interpreter | `tpt-wasm-micro` | What does WebAssembly *mean*? |
| TPT Host | `tpt-wasm-host` | What may a Wasm program *interact with*? |
| Formal Verification | `tpt-wasm-semantics`, `tpt-wasm-verify` | Can we *prove* correct behavior? |
| Full Compiler | `tpt-wasm-ir`, `tpt-wasm-codegen`, `tpt-wasm-jit`, `tpt-wasm-aot` | Can we execute the same semantics *efficiently*? |

### The central invariant

> There is one meaning of a WebAssembly program, expressed formally and embodied by the
> Micro Interpreter. Every optimized implementation must preserve that meaning.

## Status

**Architecture / greenfield implementation.** Targeting WebAssembly Core 3.0.

See [todo.md](todo.md) for the full milestone tracker (M0–M10).

## Workspace crates

| Crate | Purpose |
|---|---|
| `tpt-wasm-types` | Semantic value and type definitions |
| `tpt-wasm-format` | Module structure representation |
| `tpt-wasm-decode` | Binary format decoder |
| `tpt-wasm-validate` | Validation pass → `ValidatedModule` |
| `tpt-wasm-micro` | Micro Interpreter (Golden Machine, zero unsafe) |
| `tpt-wasm-runtime` | Store, Instance, Linker, Engine |
| `tpt-wasm-host` | TPT capability host |
| `tpt-wasm-capability` | Capability types and trait |
| `tpt-wasm-resource` | Resource table and opaque handles |
| `tpt-wasm-semantics` | Formal abstract machine |
| `tpt-wasm-verify` | Verification infrastructure |
| `tpt-wasm-ir` | TPT intermediate representation |
| `tpt-wasm-codegen` | Baseline code generation |
| `tpt-wasm-jit` | JIT compiler |
| `tpt-wasm-aot` | AOT compiler |
| `tpt-wasm` | Public embedding API |
| `tpt-wasm-cli` | Command-line interface |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

Copyright (c) 2026 TPT Solutions

## Standards

- Normative: [WebAssembly Core Specification](https://webassembly.github.io/spec/)
- Standard version: WebAssembly Core 3.0
- See [docs/standards/](docs/standards/) for the full standards matrix

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).  
Project discussion and issue tracking: GitHub Issues.
