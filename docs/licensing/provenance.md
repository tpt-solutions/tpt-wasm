# Source Provenance

This document records the origin of every significant piece of content in tpt-wasm.
It is updated whenever a new normative source, test suite, external comparison,
dependency, or third-party material is introduced.

The goal: years from now, answer "where did every significant piece of this code come from?"
without reconstructing history.

---

## Normative Standards

| Standard | Source | How Used |
|---|---|---|
| WebAssembly Core 3.0 | https://webassembly.github.io/spec/ | Normative implementation specification |
| WebAssembly Binary Format | https://webassembly.github.io/spec/core/binary/ | Binary decoder specification |
| WebAssembly Validation | https://webassembly.github.io/spec/core/valid/ | Validator specification |
| WebAssembly Execution | https://webassembly.github.io/spec/core/exec/ | Interpreter specification |

---

## Test Suites

| Suite | Source | How Used |
|---|---|---|
| WebAssembly spec test suite | https://github.com/WebAssembly/testsuite | Interoperability / conformance testing only. Not distributed. |

---

## External Runtime Comparisons

The following runtimes may be used for differential testing and behavioral comparison.
Their source code is **not** incorporated into tpt-wasm.

| Runtime | How Used |
|---|---|
| Official Wasm reference interpreter | Differential testing oracle |
| Wasmtime | Behavioral comparison, interoperability testing |
| Wasmer | Behavioral comparison |
| WasmEdge | Behavioral comparison |
| WAMR | Behavioral comparison |

---

## Dependencies

See `Cargo.lock` and the output of `cargo deny list` for the current dependency list.
Dependencies are tracked by: crate name, version, license, copyright, source, reason.

| Crate | Version | License | Reason |
|---|---|---|---|
| `sha2` | 0.11.1 | MIT OR Apache-2.0 | SHA-256 for the module hash and the execution-artifact digest in `tpt-wasm-runtime` (`provenance.rs`). Chosen over a hand-written hash, so the digest rests on a widely reviewed implementation. Transitive: `digest`, `crypto-common`, `block-buffer`, `cpufeatures`, `const-oid`, `hybrid-array`, `typenum`; all MIT or Apache-2.0 per `cargo deny list`. |
| `tpt-wasm-capability` | workspace | MIT OR Apache-2.0 | Internal. The runtime uses its `CapabilityId` to list a module's function imports in `ExecutionMetadata`. |

---

## Generated Artifacts

None currently.

---

## Third-Party Source

None incorporated. See NOTICE.

---

## Independent Implementation Statement

TPT-Wasm is an original implementation written by TPT Solutions, based on the public
WebAssembly specifications listed above. No implementation code from external
WebAssembly runtimes has been incorporated.
