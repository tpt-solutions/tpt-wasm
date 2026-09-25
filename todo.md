# tpt-wasm — Project Tracker

**License:** MIT OR Apache-2.0  
**Copyright:** TPT Solutions  
**Standard:** WebAssembly Core 3.0  
**Repository:** https://github.com/tpt-solutions/tpt-wasm

---

## M0 — Foundation

- [x] Initialize git repository and push to `github.com/tpt-solutions/tpt-wasm`
- [x] Create Cargo workspace (`Cargo.toml`, resolver = "2", all 17 crates as members)
- [x] Scaffold all 18 crate stubs with correct dependency edges (no cycles)
- [x] Add `LICENSE-MIT` and `LICENSE-APACHE` at repo root
- [x] Add SPDX headers (`MIT OR Apache-2.0`, copyright TPT Solutions) to every source file
- [x] Create `COPYRIGHT` and `NOTICE` files
- [x] Write `README.md` with project identity statement (not a Wasmtime fork/alternative)
- [x] Write `CONTRIBUTING.md` (issues-first model, independent implementation policy, no PRs)
- [x] Write `SECURITY.md` with security contact
- [x] Write `GOVERNANCE.md`
- [x] Create full `docs/` directory structure (architecture, semantics, host, verification, compiler, standards, licensing)
- [x] Write `docs/architecture/invariants.md` (all 8 architectural invariants)
- [x] Create `feature-registry.toml` (machine-readable Wasm feature/proposal matrix)
- [x] Write `docs/standards/wasm-core.md`, `wasi.md`, `component-model.md`, `proposals.md`
- [x] Write `docs/licensing/provenance.md` (record of all normative sources)
- [x] Set up `.github/workflows/ci.yml` (fmt, check, test, clippy, deny)
- [x] Configure `.cargo/deny.toml` (license allowlist: MIT/BSD/Apache/ISC; advisory audit)
- [ ] Create GitHub issue labels (architecture, wasm-core, validation, interpreter, host, capability, formal, compiler, ir, jit, aot, wasi, component, security, fuzzing, performance, licensing, provenance, research)
- [ ] Create GitHub milestones M0–M10
- [x] Verify: `cargo check --workspace` passes clean

---

## M1 — WebAssembly Types + Binary

- [x] Implement `Value` enum (I32, I64, F32, F64, V128, Ref) in `tpt-wasm-types`
- [x] Implement `ValueType`, `ResultType`, `FunctionType`, `GlobalType`, `MemoryType`, `TableType`, `TagType`, `ReferenceType`
- [x] Implement `RefValue` and `ReferenceType` variants
- [x] Implement `Module` struct in `tpt-wasm-format` (types, imports, functions, tables, memories, globals, exports, elements, data)
- [x] Implement all module section types: `Import`, `Export`, `Function`, `Table`, `Memory`, `Global`, `Element`, `DataSegment`
- [x] Implement binary decoder in `tpt-wasm-decode` (all MVP sections, LEB128, indices, limits, names, custom sections)
- [x] Implement binary encoder (bytes ↔ Module round-trip)
- [x] Implement `Validator` in `tpt-wasm-validate` producing `ValidatedModule` + `ValidationCertificate`
- [x] Ensure decode and validate remain strictly separate stages
- [x] Add unit tests for decode/encode round-trips
- [x] Add malformed binary rejection tests
- [x] Add validation error tests (type mismatches, index out of range, etc.)
- [x] Add an isolated cargo-fuzz decoder target and deterministic mutation corpus test
- [ ] Run the official Wasm MVP binary-format spec tests
- [x] Fuzz decoder with random byte inputs (cargo-fuzz or libfuzzer)

> M1 currently covers the WebAssembly MVP structural format and static validation. The decoder has an isolated cargo-fuzz target and deterministic no-panic mutation coverage; the official MVP spec suite and a live fuzz campaign remain pending. Post-MVP proposals remain explicitly unsupported.

---

## M2 — Micro Interpreter (Golden Machine)

- [x] Implement `Store` (memories, tables, globals, functions, instances) in `tpt-wasm-micro`
- [x] Implement `Machine` (store, operand_stack, frames)
- [x] Implement `Frame` and `ControlFrame` types separately
- [x] Implement `Instr` enum — all MVP instructions, clarity over performance, no optimization
- [x] Implement `step(&mut Machine) -> Step` (Continue, Return, Trap, HostCall)
- [x] Implement `Trap` enum — no Rust panics for Wasm traps (Unreachable, DivByZero, OOB, etc.)
- [x] Implement all memory operations with bounds checking
- [x] Implement table storage and indirect-call bounds checks (MVP table semantics)
- [x] Implement control flow (block, loop, if/else, br, br_if, br_table, return)
- [x] Implement direct and indirect function calls
- [x] Implement locals and globals access (get/set/tee)
- [x] Implement all integer arithmetic (i32, i64 — add, sub, mul, div, rem, and, or, xor, shl, shr, rotl, rotr, clz, ctz, popcnt)
- [x] Implement all float arithmetic (f32, f64 — IEEE 754 compliant)
- [x] Implement all conversion/truncation/reinterpret instructions
- [x] Implement memory.grow and memory.size
- [x] Implement `Limits` struct and enforcement (max_memory_pages, max_table_elements, max_stack_depth, max_call_depth, max_execution_steps)
- [x] Implement deterministic execution policy and seeded effect stream (no wall-clock or ambient RNG)
- [x] Confirm `unsafe` count = 0 in `tpt-wasm-micro`
- [ ] Pass WebAssembly Core spec test suite (MVP) through `tests/spec/`
- [x] Pass validation correctness tests
- [x] Pass malformed binary rejection tests
- [ ] Set up differential testing vs. official Wasm reference interpreter
- [ ] Fuzz interpreter with random valid Wasm modules
- [x] Confirm deterministic execution: identical output on repeated runs
- [ ] Confirm no unexplained panics on fuzz corpus

---

## M3 — Runtime

- [x] Implement `Store` with explicit `Instance` management in `tpt-wasm-runtime`
- [x] Implement `Linker` (resolve imports, link multiple instances)
- [x] Implement executable host function registration and automatic call resumption
- [x] Implement runtime store resources and lifetime tracking
- [x] Implement `Limits` enforcement at instantiation
- [x] Implement `Engine` and `Config` structs (engine_mode, features, limits, deterministic, debugging, profiling)
- [x] Implement `EngineMode` enum (Micro, Baseline, Optimizing)
- [x] Implement public embedding API: `Engine::new`, byte/file instantiation, store access, and exported function calls
- [x] Implement `get_function` / `call` on instance
- [x] Wire `EngineMode::Micro` to the interpreter backend
- [x] Write integration tests using the public API end-to-end
- [x] Verify API is independent of compiler internals, formal system, and TPT domain crates

> M3 supports the MVP Micro backend, function imports, explicit instances, start functions, exported calls, configured resource limits, and a shared-store linker for Wasm functions, globals, tables, and memories. Cross-engine store aliases and incompatible limits/types are rejected; WASI and the official spec suite remain pending.

---

## M4 — TPT Host

- [x] Implement `CapabilityId`, `CapabilityDescriptor`, `CapabilityGrant` in `tpt-wasm-capability`
- [x] Implement `Capability` trait (`id()`, `invoke(operation, args) -> Result`)
- [x] Implement descriptor-backed `CapabilitySet` and authorization-enforcing `Host` in `tpt-wasm-host`
- [x] Implement monotonic opaque `ResourceId(u64)` and lifetime-checked `ResourceTable` in `tpt-wasm-resource`
- [x] Implement `ExecutionMode` enum (Deterministic, HostDependent, RecordReplay)
- [x] Implement deterministic host services: virtual clock, seeded randomness, controlled filesystem snapshot, network policy, and scheduler
- [x] Implement record/replay infrastructure skeleton
- [x] Implement `Effect` enum and ordered successful-effect records
- [x] Create tpt-system adapter stub (placeholder; no real tpt-system dependency yet)
- [x] Implement WASI adapter skeleton (maps WASI imports → TPT capability system)
- [x] Confirm `unsafe` count = 0 in `tpt-wasm-host`, `tpt-wasm-capability`, `tpt-wasm-resource`
- [x] Write capability security tests (no ambient authority, insufficient grants, revocation)
- [x] Write resource lifetime tests
- [x] Write malicious module host isolation tests
- [x] Verify: no ambient authority invariant holds in capability and host tests

> M4 now enforces explicit descriptor registration, operation permissions, grant/revocation, monotonic resource handles, ordered effect records, record/replay, an end-to-end runtime host-function bridge, and the dedicated WASI adapter facade. Ungranted module calls are denied before capability execution, and separate host instances remain isolated. Complete deterministic effect providers and the full WASI ABI remain pending.

---

## M5 — Formal Semantics

- [x] Design an executable `Configuration` abstract machine in `tpt-wasm-semantics` (Store, FrameStack, OperandStack, ControlStack) for the modeled subset
- [x] Define executable transition relations (`C → C'`) for the modeled instruction subset
- [x] Implement V0 configuration/type invariants as executable Rust properties
- [ ] Install Lean 4 toolchain; scaffold `formal/` directory (definitions, proofs, invariants, tests)
- [ ] Write Lean 4 definitions for Wasm value types and abstract machine
- [ ] Write Lean 4 transition rules mirroring spec §4 (execution)
- [ ] Establish V1: interpreter correspondence proofs (formal rule ≈ Rust `step()`, select subset)
- [ ] Establish V2: memory safety property (`Valid(Module) ⇒ NoWasmMemoryOutOfBoundsAccess`)
- [ ] Establish V3: host capability safety (`WasmExecution ⇒ OnlyGrantedCapabilitiesInvoked`)
- [x] Write `docs/verification/model.md`, `invariants.md`, and `refinement.md`
- [ ] Add Lean 4 proof check to CI

> M5 has an executable Rust abstract machine and V0 checker for the current instruction subset. Lean tooling, full Core transition coverage, and V1–V3 proofs remain pending.

---

## M6 — TPT IR

- [x] Design the typed IR foundation in `tpt-wasm-ir` (SSA-ready values, basic blocks, explicit terminators)
- [x] Implement IR value types aligned with Wasm types
- [ ] Expand the IR instruction set to the full validated MVP (structured control flow, locals/globals, memory, tables, calls, references, remaining numeric operations, explicit host boundaries)
- [x] Implement the initial `Wasm → TPT IR` lowering pass for straight-line constants, `drop`, `select`, `nop`, straight-line local access, defined direct calls, integer arithmetic/comparison/division/remainder/bitwise/shift/rotation operations, integer unary bit-count, integer width/signedness conversion, non-trapping f32/f64 conversion, trapping float-to-integer conversion, and raw-bit reinterpretation operations, f32/f64 comparisons/unary/binary operations, and f32/f64 add/sub/mul/div
- [ ] Expand lowering to every construct represented by the IR instruction set
- [x] Implement IR type-checking / structural verification for the current instruction set
- [ ] Extend IR verification for branches, dominance, memory effects, imported/indirect calls, references, and host effects
- [x] Add an executable V4 projection hook for the IR/formal-model overlap
- [x] Add lowering differential tests against Micro for the current subset
- [x] Document the IR and lowering contract in `docs/compiler/ir.md` and `docs/compiler/lowering.md`

> M6 now provides a typed, certificate-backed straight-line IR slice including straight-line local get/set/tee, select, defined direct calls, integer arithmetic, division/remainder, comparisons, shifts, rotations, bitwise operations, integer unary bit-count, integer width/signedness conversion, non-trapping f32/f64 conversion, trapping float-to-integer conversion, and raw-bit reinterpretation operations, f32/f64 comparisons/unary/binary operations, f32/f64 add/sub/mul/div, structural verification, Micro differential coverage, and an executable formal-model projection for the complete current IR instruction set. It is intentionally not marked as a complete MVP compiler IR; the unsupported instruction and control-flow work remains listed above.

---

## M7 — Baseline Compiler

- [ ] Implement baseline code generation in `tpt-wasm-codegen` (correctness and startup time over peak performance)
- [x] Implement the initial portable baseline lowering/execution slice (`i32.const`, `i32.add`, and terminators)
- [x] Extend the portable baseline to the full straight-line `i32` set: 15 binary ops, `i32.eqz`, and 10 comparisons with Wasm-exact wrapping, masking, and trap rules
- [x] Extend the portable baseline to the symmetric straight-line `i64` set, including width-correct shift masking and division traps
- [ ] Implement simple lowering pipeline: IR → machine IR → native (x86_64 first)
- [ ] Wire `EngineMode::Baseline` to codegen backend in `Engine`
- [ ] Implement aarch64 backend stub
- [ ] Pass all Wasm spec tests through the baseline compiler
- [ ] Establish Micro equivalence: `Micro(module) == Baseline(module)` for all test programs
- [ ] Set up fuzz differential testing: random Wasm, compare Micro vs. Baseline output
- [ ] Verify trap equivalence (same traps at same points)
- [ ] Verify host-effect equivalence

> M7 now has a deterministic portable baseline slice with independent execution and Micro differential coverage for the full straight-line `i32` and `i64` sets: both constants, all fifteen binary operations per width, both `eqz` forms, all twenty comparisons, and the return/trap/unreachable terminators. Differential tests drive the real validate → IR → baseline pipeline and assert identical results and traps. Float, memory, table, local, and control-flow lowering, native x86-64/AArch64 code generation, executable-memory integration, and `EngineMode::Baseline` remain pending.


---

## M8 — Optimizing Compiler

- [ ] Implement constant folding (with precondition, transformation, semantic invariant, unit test, differential test, fuzz test)
- [ ] Implement constant propagation (same coverage requirement)
- [ ] Implement dead code elimination
- [ ] Implement CFG simplification
- [ ] Implement local value numbering
- [ ] Implement bounds-check elimination
- [ ] Implement load/store optimization
- [ ] Implement common subexpression elimination
- [ ] Implement register allocation
- [ ] Implement instruction scheduling
- [ ] Implement JIT compilation in `tpt-wasm-jit` (unsafe only in executable-memory boundary; each block documents safety invariant)
- [ ] Implement AOT compilation in `tpt-wasm-aot`
- [ ] Implement module compilation cache (keyed by: module hash + engine version + semantics version + compiler version + target arch + feature set)
- [ ] Wire `EngineMode::Optimizing` switch in `Engine`
- [ ] Verify compiler optimization contract: values, control flow, traps, memory effects, global effects, table effects, host effects, observable ordering all preserved
- [ ] Verify host-effect and trap equivalence with Micro on full regression suite

---

## M9 — Formal Compiler Verification

- [ ] Write V4: IR refinement proofs in Lean 4 (Wasm ≈ TPT IR)
- [ ] Write proof for constant folding (`x + 0 = x`)
- [ ] Write proof for dead block elimination
- [ ] Write proof for CFG simplification
- [ ] Write V5: selected optimization transformation proofs
- [ ] Write V6: lowering correctness proof skeleton (IR → machine code)
- [ ] Compose refinement chain: `Wasm ≈ IR ≈ Optimized IR ≈ Machine IR ≈ Machine Code`
- [ ] Document all proof obligations in `docs/verification/refinement.md`

---

## M10 — Production Platform

- [x] Implement `tpt-wasm-wasi` adapter facade (WASI → TPT capability system, not WASI → POSIX; full ABI remains planned)
- [ ] Implement the complete WASI ABI in `tpt-wasm-wasi`
- [ ] Implement Component Model layer (`Component` wraps Core Wasm modules; separate from Micro)
- [ ] Implement WIT interface mapping → TPT Capability boundary
- [ ] Implement debugger support (breakpoints, single-step, stack traces, locals inspection, memory inspection)
- [ ] Implement DWARF / source map support
- [ ] Implement profiling (instruction counts, function timing, host capability timing, memory/allocation stats)
- [ ] Add riscv64 compiler backend
- [ ] Implement `ExecutionMetadata` and `ExecutionArtifact` for scientific reproducibility provenance
- [ ] Implement SBOM generation in CI release pipeline
- [ ] Implement reproducible build verification
- [ ] Production security hardening (all threat models from spec §43: decoder, validator, interpreter, host, resource system, compiler, executable memory)
- [ ] Full release artifact checklist: source archive, binaries, SBOM, license report, third-party notices, feature manifest, build metadata, git commit, version info
- [ ] Miri clean on all safe-Rust crates (no undefined behavior)
- [ ] Cross-compilation CI matrix (x86_64, aarch64, riscv64)
- [ ] Pass full WebAssembly spec test suite including: reference types, bulk memory, SIMD, multi-memory, 64-bit memories, exception handling, threads (where applicable)

---

## Ongoing / Cross-cutting

- [ ] Maintain `feature-registry.toml` as proposals are implemented; generate compat docs from it
- [ ] Keep `docs/licensing/provenance.md` updated with every new normative source or test suite added
- [ ] Run `cargo deny check` in CI; fix any license violations immediately
- [ ] Track `unsafe` counts per crate; keep Micro and Host at zero
- [ ] Maintain differential testing suite — Micro result is always the golden result
- [ ] Maintain GitHub milestones and issue labels as architectural memory
- [ ] Legal review before any public release (do not rely solely on automation for licensing)
