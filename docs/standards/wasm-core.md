# WebAssembly Core Standard

**Normative version:** WebAssembly Core 3.0  
**Specification:** https://webassembly.github.io/spec/  
**Repository:** https://github.com/WebAssembly/spec  

---

## Normative sections

| Section | Coverage |
|---|---|
| §1 Introduction | Architecture context |
| §2 Structure | Module structure → `tpt-wasm-format` |
| §3 Validation | Static type checking → `tpt-wasm-validate` |
| §4 Execution | Operational semantics → `tpt-wasm-micro`, `tpt-wasm-semantics` |
| §5 Binary Format | Encoding/decoding → `tpt-wasm-decode` |
| §6 Text Format | WAT parsing → future |
| §A Appendix (Implementation limits) | `ResourceLimits` in `tpt-wasm-types` |
| §B Appendix (Formal properties) | `tpt-wasm-semantics`, `tpt-wasm-verify` |

---

## Proposals tracking

See `feature-registry.toml` for the machine-readable status of each proposal.

| Proposal | Phase | Status |
|---|---|---|
| MVP | Shipped | In progress |
| Reference Types | Phase 4 | Planned (M1+) |
| Bulk Memory | Phase 4 | Planned |
| SIMD | Phase 4 | Planned |
| Multi-value | Phase 4 | Planned |
| Sign-extension ops | Phase 4 | Planned |
| Non-trapping float-to-int | Phase 4 | Planned |
| Threads | Phase 4 | Planned |
| Exception Handling | Phase 4 | Planned (M10) |
| Memory64 | Phase 4 | Planned (M10) |
| Multi-memory | Phase 3 | Planned |
| Tail call | Phase 4 | Planned |
| GC | Phase 4 | Planned |
| Typed Function References | Phase 4 | Planned |

---

## Standards hierarchy

```
W3C WebAssembly Core
        │
        ├── Core semantics          tpt-wasm-micro / tpt-wasm-semantics
        ├── Binary format           tpt-wasm-decode
        ├── Validation              tpt-wasm-validate
        └── Execution               tpt-wasm-micro
                │
                ▼
             tpt-wasm
                │
       ┌────────┼────────┐
       ▼        ▼        ▼
     WASI   Component   TPT Host
```
