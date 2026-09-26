# tpt-wasm — Project Tracker

**License:** MIT OR Apache-2.0  
**Copyright:** TPT Solutions  
**Standard:** WebAssembly Core 3.0  
**Repository:** https://github.com/tpt-solutions/tpt-wasm

**Last verified against the working tree:** `cargo check --workspace --all-targets` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean, and `cargo test --workspace` green at 253 passing tests (252 unit/integration tests plus one doc-test). Items are only marked `[x]` when the behavior is implemented and covered by a test; an item that is partially delivered stays `[ ]` with a sub-bullet recording exactly which part is done and why the rest is not.

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
- [x] Match the spec's `br_if` stack effect: the condition and the label's values are consumed on both the taken and the fall-through path
  - The validator previously pushed the popped label values back, which made it reject valid modules whose fall-through repopulated the block's result.
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
- [x] Model the MVP memory instruction set and globals in the Rust abstract machine
  - Fourteen loads, nine stores, `memory.size`, and `memory.grow`, with little-endian access, static offsets, and sign/zero-extending narrow loads. Every access is guarded by one bounds predicate, a refused access traps, and a refused store writes nothing. `StoreOperation::value_type` keeps operand type independent of width, so `f64.store` pops an `f64` rather than an `i64`. `MemoryState` and `ModelError::MemoryOutOfBounds` were already present but unused; the instructions are what give V2 something to be about.
- [x] Install Lean 4 toolchain; scaffold `formal/` directory (definitions, proofs, invariants, tests)
  - elan with the toolchain pinned in `formal/lean-toolchain` (Lean 4.34.1). `formal/` is a Lake project (`tpt-wasm-formal`) with no external dependencies, so `lake build` needs only the pinned toolchain.
- [x] Write Lean 4 definitions for Wasm value types and abstract machine
  - `TptWasm/Basic.lean` (value domain and the width invariant), `Instruction.lean` (traps, the instruction set, two's-complement wraparound), `Store.lean` (memories, tables, globals, functions), and `Config.lean` (frames, control stack, configuration, capability grants).
- [x] Write Lean 4 transition rules mirroring spec 4 (execution)
  - `OpStep` states the instruction semantics as one constructor per rule, `Step` composes instruction fetch with the program-counter advance, and `execOp`/`exec` are the executable interpreter written from the same clauses. Where a check fails no rule applies, so the machine is stuck rather than approximating.
- [x] Establish V1: interpreter correspondence proofs (formal rule = Rust `step()`, select subset)
  - `execOp_sound` shows every derivable rule is computed by the interpreter, and `OpStep.deterministic`/`Step.deterministic` show at most one outcome is derivable. The modeled subset is the Rust instruction set minus the float arithmetic and the trapping float-to-integer conversions; the result is rule-to-interpreter inside the Lean model, not Lean-to-Rust, which is recorded in `docs/verification/refinement.md`.
- [x] Establish V2: memory safety property (`Valid(Module) => NoWasmMemoryOutOfBoundsAccess`)
  - The bounds guard in `MemoryState.readLE?`/`writeLE?` implies the access is in bounds whenever a `load`/`store` rule applies, and an out-of-bounds access has a trap rule available, so a refused store writes nothing. Lifting this to the `Valid(Module)` implication needs the validation theory and is M9 work.
- [x] Establish V3: host capability safety (`WasmExecution => OnlyGrantedCapabilitiesInvoked`)
  - `callHost` carries the grant as a premise, so an ungranted capability has no rule at all, and a step never changes the grants, so execution cannot widen its own authority.
- [x] Write `docs/verification/model.md`, `invariants.md`, and `refinement.md`
- [x] Add Lean 4 proof check to CI
  - A `lean` job runs `lake build` and fails on any `sorry` or `axiom`. The model currently builds with no errors, no warnings, no `sorry`, and no `axiom`.

> M5 has an executable Rust abstract machine with a V0 checker, and the Lean 4 model of the same machine: the value domain, the store and configuration types, the section 4 transition rules, the executable interpreter, and machine-checked V0-V3 proofs. The Rust model now also covers the full MVP memory instruction set and globals. The Lean model is a separate formalization, so V1 establishes rule-to-interpreter agreement rather than equality with `Micro::step`; lifting V2 to the `Valid(Module)` implication and the V4 refinement proofs remain M9 work.

---

## M6 — TPT IR

- [x] Design the typed IR foundation in `tpt-wasm-ir` (SSA-ready values, basic blocks, explicit terminators)
- [x] Implement IR value types aligned with Wasm types
- [ ] Expand the IR instruction set to the full validated MVP (structured control flow, locals/globals, memory, tables, calls, references, remaining numeric operations, explicit host boundaries)
  - Structured control flow representation and dominance verification: done. Multi-block IR with block parameters, `Branch`/`CondBranch` terminators, and dominance-checked uses.
  - Lowering Wasm `block`/`loop`/`if`/`br`/`br_if`/`br_table`/`return` into that CFG: done. A loop label carries its header as the branch target and a distinct exit block, so a back edge and a fall-out are different edges.
  - Multi-block local dataflow: done. A Wasm local is a function-scoped storage slot rather than an SSA register, so `local.get`/`set`/`tee` keep one stable value ID across the whole function. `br_table` spills its selector into a synthesized local and re-reads it in each comparison block, because a later block in the chain is not dominated by a definition in the first. The verifier matches this: a local's initial value is always present, so a local written inside a branch stays readable on every path.
  - The linear memory and globals are now in the IR: `IrModule` carries the memory declaration and the global types with their initial values, and `Load`/`Store`/`MemorySize`/`MemoryGrow`/`GlobalGet`/`GlobalSet` are lowered, verified, and executed.
  - Tables and active element segments are now in the IR too: `IrModule` carries the table declaration with its function indices, and `CallIndirect` names a type rather than a function, so the signature is checked against the table entry at run time.
  - Active data segments now travel with the memory declaration as `IrMemory::segments`, kept in module order so an overlapping later segment wins. They are applied at instantiation, before any function runs. References are done: `ref.null`, `ref.func`, and `ref.is_null` lower to `RefNull`, `RefFunc`, and `RefIsNull` over a `ValueType::Ref` value, and the verifier checks that a `ref.func` names a defined function and that a `ref.is_null` operand is a reference. Making a reference storable widened the validator's type surface, which had been MVP-numeric-only: `funcref`/`externref` are now accepted in function signatures, locals, globals, and block result types, while `v128` stays out. Micro's block-type decoder was widened the same way, so a block may carry a reference result. Explicit host effects are still pending.
- [x] Implement the initial `Wasm → TPT IR` lowering pass for straight-line constants, `drop`, `select`, `nop`, straight-line local access, defined direct calls, integer arithmetic/comparison/division/remainder/bitwise/shift/rotation operations, integer unary bit-count, integer width/signedness conversion, non-trapping f32/f64 conversion, trapping float-to-integer conversion, and raw-bit reinterpretation operations, f32/f64 comparisons/unary/binary operations, and f32/f64 add/sub/mul/div
- [x] Expand lowering to every construct represented by the IR instruction set
  - Every variant of `IrInstr` is now produced by `lower_module`; the lowerer cannot reach a state where the IR has an instruction it has no way to emit. Constructs with no IR form yet (observing a capability's effects, imported tables/memories/globals, and cross-instance imports) are still rejected as unsupported and remain listed as open above.
- [x] Implement IR type-checking / structural verification for the current instruction set
- [ ] Extend IR verification for branches, dominance, memory effects, imported/indirect calls, references, and host effects
  - Branches, block parameters, and dominance: done — multi-block CFG with an iterative immediate-dominator fixpoint, reachable-block enforcement, and per-edge arity/type checks. Each block's visible values are the ones defined in blocks that dominate it, so a value computed before a branch stays usable after it.
  - Indirect calls: done — the table must exist and be a `funcref` table, the named type must be declared, and the argument and result arities and types are checked against it.
  - Memory and global effects: done — a memory instruction in a module that declares no memory is rejected, an unknown global or table index is rejected, `global.set` on an immutable global is rejected, and each load/store width and operand type is checked against the access.
  - References: done — a `ref.func` must name a function the module defines, and a `ref.is_null` operand must be a reference of either kind rather than any value. Each is a hand-built-IR test, because the validator and the lowerer refuse the malformed cases first.
  - Host effects: partially done. `CallHost` is the explicit host-effect instruction, and the verifier checks it against the import's declared signature. What remains is an instruction for observing a capability's effects rather than only invoking it, plus imported tables, memories, globals, and cross-instance imports, none of which have an IR form.
- [x] Add an executable V4 projection hook for the IR/formal-model overlap
  - The hook projects a certified IR function into the executable model and executes it, covering the straight-line numeric, local, and direct-call subset. It is a correspondence hook, not a proof. Control flow, memory, globals, and `call_indirect` are rejected as unsupported rather than projected approximately, because the formal model has no control flow, memory, or table state yet; widening the projection is M9 work.
- [x] Add lowering differential tests against Micro for the current subset
- [x] Document the IR and lowering contract in `docs/compiler/ir.md` and `docs/compiler/lowering.md`
  - Both files now match the code: the status headers, the module-state gate, the pipeline, the instruction-mapping table, the verification list, the differential-testing coverage, and the scope boundary all describe the multi-block, memory, global, and table lowering that exists. `codegen.md` was corrected too — it still listed `EngineMode::Baseline` as pending, and it was missing the control-flow, memory, global, and table differential coverage.

> M6 now provides a typed, certificate-backed IR slice that lowers the whole MVP surface the IR can represent: constants, `drop`, `select`, local get/set/tee, defined direct calls, the full integer set (arithmetic, division/remainder, comparisons, shifts, rotations, bitwise, bit-count, width/signedness conversion), the full float set (comparisons, unary, binary, non-trapping conversion, trapping truncation, raw-bit reinterpretation), multi-block structured control flow, the linear memory with all fourteen loads and nine stores plus `memory.size`/`memory.grow`, globals, tables with active element segments, `call_indirect`, active data segments, the reference instructions, and imported-function resolution through an explicit `CallHost`. Verification covers dominance, block-parameter arity, call and table typing, memory/global effects, the reference rules, and the host-call signature, with Micro differential coverage and an executable formal-model projection for the supported subset. It is deliberately not marked as a complete MVP compiler IR: there is no instruction for observing a capability's effects, imported tables/memories/globals and cross-instance imports have no form, and the reference-types proposal is only partly in — there is no `table.get`/`table.set`, no `externref` table, and no passive or declarative element segment. Those remain listed above.

---

## M7 — Baseline Compiler

- [ ] Implement baseline code generation in `tpt-wasm-codegen` (correctness and startup time over peak performance)
  - The portable baseline slice below is complete; this umbrella item stays open for the native backends, which need an executable-memory boundary.
- [x] Implement the initial portable baseline lowering/execution slice (`i32.const`, `i32.add`, and terminators)
- [x] Extend the portable baseline to multi-block functions: block parameters bound by incoming edges, plus `Branch` and `CondBranch` terminators and a block-graph executor
- [x] Extend the portable baseline to direct calls to defined functions, with a module-level function table and per-callee slot frames so recursion works
- [x] Extend the portable baseline to tables and indirect calls: an active element segment seeds the table, and `call_indirect` resolves the index, traps distinctly on out-of-range versus null, and checks the entry's signature against the named type
- [x] Extend the portable baseline to active data segments: the bytes are written into memory at instantiation in module order, so a later segment overwrites an earlier one at the same address and the contents survive `memory.grow`
- [x] Extend the portable baseline to the linear memory: all fourteen loads, all nine stores, `memory.size`, and `memory.grow`, with checked little-endian access and `MemoryOutOfBounds` traps
- [x] Extend the portable baseline to `global.get`/`global.set` with module-level global slots shared across calls
- [x] Extend the portable baseline to the full straight-line `i32` set: 15 binary ops, `i32.eqz`, and 10 comparisons with Wasm-exact wrapping, masking, and trap rules
- [x] Extend the portable baseline to the symmetric straight-line `i64` set, including width-correct shift masking and division traps
- [x] Extend the portable baseline to the symmetric straight-line `f32` set: 7 binary ops, 7 unary ops, 6 comparisons, with Wasm-exact NaN canonicalization, NaN-payload-preserving `abs`/`neg`/`copysign`, signed-zero `min`/`max`, and ties-to-even `nearest`
- [x] Extend the portable baseline to the symmetric straight-line `f64` set with the same operation set and IEEE 754 identities
- [x] Extend the portable baseline to straight-line locals: `local.get`, `local.set`, and `local.tee` over zero-initialized slots, plus `drop` and `select`
- [x] Extend the portable baseline to the straight-line integer unary set (`clz`, `ctz`, `popcnt`) and the non-trapping width/signedness conversions
- [x] Extend the portable baseline to raw-bit reinterpretation and to the non-trapping float conversions, preserving NaN payloads and signed zeros
- [x] Extend the portable baseline to the trapping float truncations with Wasm-exact `InvalidConversion` boundaries (NaN, infinity, out-of-range, negative-to-unsigned)
- [x] Extend the portable baseline to the reference instructions: `RefNull`, `RefFunc`, and `RefIsNull`, with a `ref.func` to a function the module does not carry refused rather than producing a dangling reference
- [ ] Implement simple lowering pipeline: IR → machine IR → native (x86_64 first)
- [x] Wire `EngineMode::Baseline` to codegen backend in `Engine`
  - The module is compiled through validate to IR to baseline at instantiation and executed by the compiled form. A module the baseline cannot represent is refused at instantiation rather than left half-usable. Function imports now lower to `CallHost` and run through the host boundary; the remaining gaps are cross-instance imports, an exported or started imported function, and imported tables, memories, and globals.
- [ ] Implement aarch64 backend stub
- [ ] Pass all Wasm spec tests through the baseline compiler
- [x] Establish Micro equivalence: `Micro(module) == Baseline(module)` for all test programs
  - Established for every module the baseline can represent, driven through the real validate → IR → baseline pipeline rather than a hand-built IR. The codegen tests assert identical results and traps against Micro, including the reference instructions returning a `funcref` unchanged, so the two backends' value representations are compared directly and not only through a derived `i32`. Agreement also holds across 25,000 generated programs — see the fuzz differential item below. Extending this to the official spec suite is the separate item above, since that suite does not run yet.
- [x] Set up fuzz differential testing: random Wasm, compare Micro vs. Baseline output
  - A seeded generator builds whole programs — 12 instruction families across both widths and all four float/number types, with a mutable global and a page of memory as shared state — and each one is run through the real validate → IR → baseline pipeline and compared against Micro for both result values and the trap variant. 25,000 programs run in the ordinary test suite (~1.5s); 200,000 were swept locally with no divergence. A seed replays deterministically and every failure message carries the seed and the body bytes, so a divergence reproduces without an artifact. Still open: a structure-aware generator covering control flow and `call`/`call_indirect`, a `cargo-fuzz` target for the same comparison, and the reference-interpreter differential named below.
- [x] Verify trap equivalence (same traps at same points)
  - Checked trap-by-trap against Micro for the whole represented surface: integer division and remainder, every `InvalidConversion` boundary in the float truncations, out-of-bounds memory access, `call_indirect` with an out-of-range index, a null entry, and a signature mismatch, a missing memory, and trap propagation out of a callee. Each baseline trap is asserted to be the same variant Micro raises at the same point.
- [x] Verify host-effect equivalence
  - The baseline has a host boundary now, so this is no longer blocked. `CallHost` is the single instruction that crosses it, resolved only through an installed `HostBoundary`; a compiled module with no boundary is refused rather than left half-usable. The runtime routes both backends through the same `Instance::invoke_host_call`, so a host call has one implementation rather than two that happen to agree. A failing host is reported as a `RuntimeError::HostFunction` with the same registered name and message from either backend — verified by asserting the two errors are equal, not merely that both fail. The host's return arity and types are re-checked against the import's declared signature, since the host sits outside the verified module. Still open: observing a capability's *effects* rather than only invoking it, imported tables/memories/globals, and cross-instance imports.

> M7 now has a deterministic portable baseline slice with independent execution and Micro differential coverage for the full straight-line MVP instruction set that the IR represents: constants and every arithmetic, comparison, and bit-count operation at all four widths; `drop`, `select`, and `local.get`/`set`/`tee` over zero-initialized slots; non-trapping integer width and signedness conversions; raw-bit reinterpretation; the non-trapping float conversions; and the trapping float truncations; plus multi-block control flow — `Branch` and `CondBranch` terminators, block parameters bound by incoming edges, and a block-graph executor that follows back edges. IEEE 754 behavior is reproduced exactly, and differential tests drive the real validate → IR → baseline pipeline asserting identical results and traps bit-for-bit, including NaN canonicalization, NaN-payload preservation across reinterpretation, signed-zero `min`/`max`, ties-to-even rounding, every `InvalidConversion` boundary, and a loop that only terminates if the back edge is followed. Direct calls, the full MVP memory instruction set (fourteen loads, nine stores, `memory.size`, `memory.grow`), and `global.get`/`global.set` are now lowered and executed, with module-level memory and global state shared across calls. Tables and active element segments are now lowered and executed as well: an active segment seeds the table, and `call_indirect` resolves the index, distinguishes an out-of-range index from a null entry, and checks the entry's signature against the type the call names. Active data segments are lowered and applied at instantiation as well. `EngineMode::Baseline` is now wired into the runtime: the module is compiled through IR to the portable baseline at instantiation and executed by it, with the compiled instance owning that module's memory, table, and globals so state persists across calls. Baseline and Micro are checked to agree on the same module. The reference instructions are lowered and executed too, with a `funcref` carried through a local, through a block result, and out of a function as a value. A module with function imports compiles and runs, reaching a granted host capability through an explicit `CallHost` and an installed `HostBoundary`, with a host failure reported identically by both backends. Native x86-64/AArch64 code generation and executable-memory integration remain pending.


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
  - Partially done: the `mvp_compiled`, `mvp_differential`, and `mvp_verified` ladder entries were corrected from `false` to `"partial"`, each with a note on what it does and does not yet cover. The per-proposal flags and the generated compat docs are still outstanding, and `mvp_tested` stays `"partial"` because the spec suite does not run.
- [ ] Keep `docs/licensing/provenance.md` updated with every new normative source or test suite added
- [ ] Run `cargo deny check` in CI; fix any license violations immediately
- [ ] Track `unsafe` counts per crate; keep Micro and Host at zero
  - Micro, Host, Capability, and Resource are at zero today: the only match in those crates is the word `unsafe` in a doc comment, not a block. The tracking is not automated yet, and JIT/AOT will introduce the first real `unsafe` at the executable-memory boundary.
- [ ] Maintain differential testing suite — Micro result is always the golden result
- [ ] Maintain GitHub milestones and issue labels as architectural memory
- [ ] Legal review before any public release (do not rely solely on automation for licensing)


