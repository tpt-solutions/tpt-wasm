# tpt-wasm — Project Tracker

**License:** MIT OR Apache-2.0  
**Copyright:** TPT Solutions  
**Standard:** WebAssembly Core 3.0  
**Repository:** https://github.com/tpt-solutions/tpt-wasm

---

## M0 — Foundation

- [x] Initialize git repository and push to `github.com/tpt-solutions/tpt-wasm`
- [x] Create Cargo workspace (`Cargo.toml`, resolver = "2", all 17 crates as members)
- [x] Scaffold all 17 crate stubs with correct dependency edges (no cycles)
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

- [ ] Implement `Value` enum (I32, I64, F32, F64, V128, Ref) in `tpt-wasm-types`
- [ ] Implement `ValueType`, `ResultType`, `FunctionType`, `GlobalType`, `MemoryType`, `TableType`, `TagType`, `ReferenceType`
- [ ] Implement `RefValue` and `ReferenceType` variants
- [ ] Implement `Module` struct in `tpt-wasm-format` (types, imports, functions, tables, memories, globals, exports, elements, data)
- [ ] Implement all module section types: `Import`, `Export`, `Function`, `Table`, `Memory`, `Global`, `Element`, `DataSegment`
- [ ] Implement binary decoder in `tpt-wasm-decode` (all sections, LEB128, indices, limits, names, custom sections)
- [ ] Implement binary encoder (bytes ↔ Module round-trip)
- [ ] Implement `Validator` in `tpt-wasm-validate` producing `ValidatedModule` + `ValidationCertificate`
- [ ] Ensure decode and validate remain strictly separate stages
- [ ] Add unit tests for decode/encode round-trips
- [ ] Add malformed binary rejection tests
- [ ] Add validation error tests (type mismatches, index out of range, etc.)
- [ ] Fuzz decoder with random byte inputs (cargo-fuzz or libfuzzer)
- [ ] Verify: all Wasm MVP binary format spec tests pass

---

## M2 — Micro Interpreter (Golden Machine)

- [ ] Implement `Store` (memories, tables, globals, functions, instances) in `tpt-wasm-micro`
- [ ] Implement `Machine` (store, operand_stack, frames)
- [ ] Implement `Frame` and `ControlFrame` types separately
- [ ] Implement `Instr` enum — all MVP instructions, clarity over performance, no optimization
- [ ] Implement `step(&mut Machine) -> Step` (Continue, Return, Trap, HostCall)
- [ ] Implement `Trap` enum — no Rust panics for Wasm traps (Unreachable, DivByZero, OOB, etc.)
- [ ] Implement all memory operations with bounds checking
- [ ] Implement all table operations with bounds checking
- [ ] Implement control flow (block, loop, if/else, br, br_if, br_table, return)
- [ ] Implement direct and indirect function calls
- [ ] Implement locals and globals access (get/set/tee)
- [ ] Implement all integer arithmetic (i32, i64 — add, sub, mul, div, rem, and, or, xor, shl, shr, rotl, rotr, clz, ctz, popcnt)
- [ ] Implement all float arithmetic (f32, f64 — IEEE 754 compliant)
- [ ] Implement all conversion/truncation/reinterpret instructions
- [ ] Implement memory.grow and memory.size
- [ ] Implement `Limits` struct and enforcement (max_memory_pages, max_table_elements, max_stack_depth, max_call_depth, max_execution_steps)
- [ ] Implement deterministic execution mode (no wall-clock, seeded RNG)
- [ ] Confirm `unsafe` count = 0 in `tpt-wasm-micro`
- [ ] Pass WebAssembly Core spec test suite (MVP) through `tests/spec/`
- [ ] Pass validation correctness tests
- [ ] Pass malformed binary rejection tests
- [ ] Set up differential testing vs. official Wasm reference interpreter
- [ ] Fuzz interpreter with random valid Wasm modules
- [ ] Confirm deterministic execution: identical output on repeated runs
- [ ] Confirm no unexplained panics on fuzz corpus

---

## M3 — Runtime

- [ ] Implement `Store` with explicit `Instance` management in `tpt-wasm-runtime`
- [ ] Implement `Linker` (resolve imports, link multiple instances)
- [ ] Implement host function registration API
- [ ] Implement resource table and resource lifetime tracking
- [ ] Implement `Limits` enforcement at instantiation
- [ ] Implement `Engine` and `Config` structs (engine_mode, features, limits, deterministic, debugging, profiling)
- [ ] Implement `EngineMode` enum (Micro, Baseline, Optimizing)
- [ ] Implement public embedding API: `Engine::new`, `Module::from_file`, `Store::new`, `engine.instantiate`
- [ ] Implement `get_function` / `call` on instance
- [ ] Wire `EngineMode::Micro` to the interpreter backend
- [ ] Write integration tests using the public API end-to-end
- [ ] Verify API is independent of compiler internals, formal system, and TPT domain crates

---

## M4 — TPT Host

- [ ] Implement `CapabilityId`, `CapabilityDescriptor`, `CapabilityGrant` in `tpt-wasm-capability`
- [ ] Implement `Capability` trait (`id()`, `invoke(operation, args) -> Result`)
- [ ] Implement `CapabilitySet` and `Host` struct in `tpt-wasm-host`
- [ ] Implement `ResourceId(u64)` and `ResourceTable` in `tpt-wasm-resource`
- [ ] Implement `ExecutionMode` enum (Deterministic, HostDependent, RecordReplay)
- [ ] Implement deterministic mode: virtual clock, seeded RNG, controlled filesystem snapshot, prohibited/recorded network, deterministic scheduler
- [ ] Implement record/replay infrastructure skeleton
- [ ] Implement `Effect` enum (Pure, Read, Write, Execute, External) for explicit host effects
- [ ] Create tpt-system adapter stub (placeholder; no real tpt-system dependency yet)
- [ ] Implement WASI adapter skeleton (maps WASI imports → TPT capability system)
- [ ] Confirm `unsafe` count = 0 in `tpt-wasm-host`, `tpt-wasm-capability`, `tpt-wasm-resource`
- [ ] Write capability security tests (no ambient authority, prevent escalation, confused deputy)
- [ ] Write resource lifetime tests
- [ ] Write malicious module host isolation tests
- [ ] Verify: no ambient authority invariant holds in all tests

---

## M5 — Formal Semantics

- [ ] Design `Configuration` abstract machine in `tpt-wasm-semantics` (Store, FrameStack, OperandStack, ControlStack)
- [ ] Define transition relations (C → C') mirroring spec execution semantics
- [ ] Implement V0: type invariants as executable Rust properties
- [ ] Install Lean 4 toolchain; scaffold `formal/` directory (definitions, proofs, invariants, tests)
- [ ] Write Lean 4 definitions for Wasm value types and abstract machine
- [ ] Write Lean 4 transition rules mirroring spec §4 (execution)
- [ ] Establish V1: interpreter correspondence proofs (formal rule ≈ Rust `step()`, select subset)
- [ ] Establish V2: memory safety property (`Valid(Module) ⇒ NoWasmMemoryOutOfBoundsAccess`)
- [ ] Establish V3: host capability safety (`WasmExecution ⇒ OnlyGrantedCapabilitiesInvoked`)
- [ ] Write `docs/verification/model.md`, `invariants.md`, `refinement.md`
- [ ] Add Lean 4 proof check to CI

---

## M6 — TPT IR

- [ ] Design typed IR in `tpt-wasm-ir` (SSA-ready, explicit CFG)
- [ ] Implement IR value types aligned with Wasm types
- [ ] Implement IR instructions (typed values, explicit memory ops, explicit traps, explicit calls, explicit references, explicit host boundaries)
- [ ] Implement `Wasm → TPT IR` lowering pass (`ValidatedModule → IR`)
- [ ] Implement IR type-checking / verification pass
- [ ] Add verification hooks (IR ↔ formal model correspondence)
- [ ] Write lowering tests (compare IR semantics against Micro results)
- [ ] Document IR design in `docs/compiler/ir.md` and `docs/compiler/lowering.md`

---

## M7 — Baseline Compiler

- [ ] Implement baseline code generation in `tpt-wasm-codegen` (correctness and startup time over peak performance)
- [ ] Implement simple lowering pipeline: IR → machine IR → native (x86_64 first)
- [ ] Wire `EngineMode::Baseline` to codegen backend in `Engine`
- [ ] Implement aarch64 backend stub
- [ ] Pass all Wasm spec tests through the baseline compiler
- [ ] Establish Micro equivalence: `Micro(module) == Baseline(module)` for all test programs
- [ ] Set up fuzz differential testing: random Wasm, compare Micro vs. Baseline output
- [ ] Verify trap equivalence (same traps at same points)
- [ ] Verify host-effect equivalence

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

- [ ] Implement `tpt-wasm-wasi` crate (WASI → TPT capability system, not WASI → POSIX)
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
