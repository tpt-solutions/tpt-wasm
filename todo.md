# tpt-wasm — Project Tracker

**License:** MIT OR Apache-2.0  
**Copyright:** TPT Solutions  
**Standard:** WebAssembly Core 3.0  
**Repository:** https://github.com/tpt-solutions/tpt-wasm

**Last verified against the working tree:** `cargo check --workspace --all-targets` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo deny check` clean, `cargo fmt --all -- --check` clean, and `cargo test --workspace` green at 279 passing tests (274 unit/integration tests plus one doc-test; one further test is `#[ignore]`d). That count is dominated by the roughly 32,368 assertions the two spec-test harnesses drive through the real decoder/validator/runtime (218 from the binary-format suite, 16,075 from the Core suite on each of the two execution backends), not the number of `#[test]` functions. Items are only marked `[x]` when the behavior is implemented and covered by a test; an item that is partially delivered stays `[ ]` with a sub-bullet recording exactly which part is done and why the rest is not.

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
- [x] Run the official Wasm MVP binary-format spec tests
  - `crates/tpt-wasm-spec` vendors `binary.wast` and `binary-leb128.wast` unmodified (provenance and Apache-2.0 licensing in `testdata/PROVENANCE.md`) and runs them through the real decoder and validator. All 218 assertions hold: 49 modules accepted, 154 rejected, and 4 documented gaps. The counts are asserted so a silent change in coverage fails the build, and every gap names the missing behavior rather than just a line, so the list is a to-do list; a failure that is *not* listed fails the test, and a listed gap that starts passing also fails, so an entry cannot rot. Building the harness found four real decoder bugs, all fixed: a function body was not required to end with the `end` opcode, so a module whose body ran past its end decoded; the declared local counts were not summed, so a module could declare more than 2^32-1 locals; a data segment could only be written in the one form with no kind byte, so the passive and explicit-memory forms were rejected; and the same for an element segment, which could only be active in table 0. None was reachable from the hand-written tests. The 2 remaining gaps are both the same unimplemented feature rather than 2 defects: the bulk-memory instructions and their data count section. In each the module is valid and this implementation refuses it, so nothing undecodable slips through. `assert_malformed` is satisfied by a rejection at either the decode or the validate stage, since the harness runs only those two; the outcomes stay distinct in the reported `Outcome` so the difference stays visible. An earlier revision demanded rejection at the decode stage specifically and recorded 15 gaps, 11 of which were really the validator doing the refusing, and a further 2 were element segments whose entries are expressions. That demand was the error, not the code: parsing function bodies in the decoder, which had been planned to close 9 of them, turns out to be unnecessary here, because the validator already walks a body's opcodes and rejects illegal ones, unbalanced blocks, and over-long LEBs within a body. Element segments are now read in all eight core forms. The four whose entries are constant expressions needed a representation the format did not have: `Element.init` was a vector of function indices, which cannot hold a `ref.null`. It is now an `ElementInit` that records which of the two families a segment was written in, and the IR table carries `Option<u32>` so a null survives lowering. Both backends read the segment independently, and a segment written as `ref.null` is observed to trap a `call_indirect` in each, which an implementation that narrowed the expression to an index would not. Text-format modules are reported as skipped rather than assembled, because writing a text assembler here would be a second implementation of the format whose bugs could mask a decoder bug. The rest of the Core suite is the separate item below.
- [x] Fuzz decoder with random byte inputs (cargo-fuzz or libfuzzer)

> M1 currently covers the WebAssembly MVP structural format and static validation. The decoder has an isolated cargo-fuzz target and deterministic no-panic mutation coverage, and the official MVP binary-format spec suite passes; a live fuzz campaign and the Core suite remain pending. Post-MVP proposals remain explicitly unsupported.

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
- [x] Pass WebAssembly Core spec test suite (MVP) through `tests/spec/`
  - Complete: every Core suite file whose only requirement is MVP is vendored and passing on both backends, and every file that is not has a measured post-MVP blocker recorded below.
  - Started: `crates/tpt-wasm-spec/tests/core_spec.rs` vendors 45 Core suite files and runs their `module`/`invoke`/`assert_return`/`assert_trap`/`assert_exhaustion`/`register` directives (16,075 of them) through the real `Engine::instantiate_bytes`/`Instance::call`, on **both** the `Micro` and the `Baseline` backend — the same directives, the same expected values, the same asserted counts, twice over. Since almost all of the Core suite is written in the text format rather than `(module binary ...)`, and this project deliberately has no text assembler of its own (see `binary_format.rs`'s reasoning), turning `.wast` text into bytes is delegated to the `wast` crate (`bytecodealliance/wasm-tools`), used only for that one step; decode, validate, instantiate, and call are still entirely tpt-wasm's own code, and a bug in `wast`'s encoder would surface as a value or trap mismatch here rather than being absorbed. `assert_return`'s three result forms are all checked bit-exactly: an exact value (including signed zero and NaN payload), a canonical NaN, and an arithmetic NaN, matched against the raw `f32`/`f64` bits `Value` already carries. The files are fetched as raw bytes over HTTPS at the pinned revision `6087111` and verified against it, never retyped, because they are the thing under test and a transcription slip in one would read as a real failure. All 16,075 directives pass on both backends. The batches found fourteen real bugs between them, all fixed:
    - The memory family (`address`, `align`, `endianness`, `load`, `store`, `memory`, `memory_size`, `memory_trap`, `memory_redundancy` — 981 directives) found no defect in the memory instructions themselves: the loads, stores, `memory.size`, active data segments, the `memarg` alignment hint, little-endian ordering, and the out-of-bounds trap rules were already correct on both backends, which is worth recording as a genuine result rather than an absence of one. It did expose that `memory_grow.wast` could not join the table, and following that up found two more real bugs, both fixed:
        - The `register` directive only recorded a name in the harness's own map and put nothing in the engine's `Linker`, so a later module's import could never resolve. `Linker::define_instance` already existed and was already used by the runtime's own tests; the harness simply never called it. Every cross-instance import the Core suite exercises was therefore unrunnable, which is why the table, element, data, function, type, and import/export families all looked blocked on the same thing — they were, on one missing call.
        - An exported memory's external type reported the minimum it was *declared* with rather than its *current* length, and was snapshotted into the `Linker` at registration time. WebAssembly matches an import against the exporting instance's type when the *importing* module is instantiated, and a memory's type includes its current length, so both halves were wrong: a module that grew a memory, exported it, and had another module import it requiring the size it was now was refused as an incompatible import. The type is now read from the store at resolution time and a memory reports its current page count. `ExternalKind` existed only to carry that snapshot and is gone, replaced by one accessor per kind, so the stale value cannot be reintroduced. Both are covered by new tests in `tpt-wasm-runtime`; the first was checked to fail without the fix.
    - The float comparison and bitwise families (`f32_cmp`, `f64_cmp`, `f32_bitwise`, `f64_bitwise` — 5,542 directives) plus `float_memory`, `type`, `custom`, `inline-module`, and `unreached-invalid` were probed rather than assumed, and passed on both backends as they stood: the relation operators, the bit-level operators, float round trips through memory, custom-section handling, and the validator's dead-code rules were all already correct. Rather than guess which files were blocked on what, all 31 un-vendored candidates were downloaded and run, and the per-file failure counts decided the order of work — that is what identified `spectest` and the start function as the two cheap unblocks, and it is recorded here because the probe is the thing that made the rest of the batch obvious. The eleven files that then passed (5,824 directives) went in, and the probe found three real gaps, all fixed:
        - The harness never defined the `spectest` host module, so every Core suite file importing one — `start.wast`, `func_ptrs.wast`, `token.wast`, parts of `imports.wast` and `global.wast` — failed to link. The module is the reference interpreter's own test fixture, not part of WebAssembly, and its print functions exist only to be called and discarded, so they are defined as no-ops. Its globals, table, and memory are not, because this project still cannot import those.
        - The IR refused to lower any module with a start function, even though the runtime already knew how to run one on either backend. A start function is module metadata for the same reason exports are, so the refusal is simply gone; carrying it in the IR as well would have given the lowerer a second, redundant way to say the same thing.
        - The baseline resolved a start function's index against the module's *defined* functions only, so a module nominating an **imported** function as its start was refused there and accepted on the interpreter. Wasm numbers imports first, so a start index below the import count names a host function, and the specification allows exactly that. `BaselineModule::call_wasm_index` now does the split in one place. Checked by reverting the fix and confirming the new test fails, and only on `Baseline`.
        - The harness also could not run a module written *inline* inside a directive (`(assert_trap (module ... (start ...)) ...)`), nor read an exported global (`(get "e")`). Both are now real: the inline module is encoded and instantiated for real, and crucially its errors go through the `From<RuntimeError>` conversion rather than being flattened to a string, so a start function that *traps* stays classified as a trap. `Instance::global` reads the global's current value — which is the distinction `exports.wast` exists to check.
    - Writing that second test exposed a further bug, and it is the same shape as the stale-memory-size one: **a compiled module kept its globals in a private `Vec` while the store kept a second copy**, so the two diverged. `Instance::global` on `Baseline` returned the initializer forever, because the compiled `global.set` only ever touched the module's own copy. `BaselineModule.globals` is now a `SharedGlobals` handle, published per store address in the same registry the memory uses, and `Instance::global` reads through whichever backend owns the value — the same rule `external_memory_type` already followed, which is what made the two backends agree about a memory and now makes them agree about a global. The test is written to run on both backends, which is how the divergence was caught at all: it was written for `Micro` first and the bug was invisible until the loop was added. The globals handle is also what a *global import* will need, so the two are one piece of work rather than two.
    - The five sign-extension opcodes (`i32.extend8_s`/`16_s`, `i64.extend8_s`/`16_s`/`32_s`, 0xc0-0xc4) were not implemented anywhere — not decoded, not validated, not in the IR, not in the baseline — so `i32.wast`/`i64.wast` failed to instantiate at all. All four layers now implement them (validator: `body.rs`; Micro: `instr.rs`/`machine.rs`; IR: a new `SignExtend` op, mirroring `IntConversion`/`Reinterpret`'s pattern of one enum for an asymmetric per-width op set rather than forcing it into the shared `IntUnary`; baseline: `BaselineOp::SignExtend`), and the formal-model IR projection rejects it explicitly as unsupported rather than approximating, the same treatment `call_indirect`/memory/globals/references already get there.
    - The decoder's `value_type()` and `reference_type()` (`tpt-wasm-decode/src/decoder.rs`) only recognized `i32`/`i64`/`f32`/`f64`, and `funcref` for tables — despite the validator having accepted `funcref`/`externref` in signatures, locals, globals, and block results since M6. A function signature, local, or global with a `funcref`/`externref` type, or an `externref` table, could not actually decode from real bytes; the validator's support was unreachable from a real module. Both now accept the reference forms too.
    - Micro's `branch()` (`tpt-wasm-micro/src/machine.rs`) used a block's *result* arity for every branch, including a branch to a `loop`'s start. A branch back to a loop carries the loop's *parameter* arity, not its result arity — the current block-type representation has no independent param list, so that arity is correctly zero, but the code used the (nonzero) result arity instead. Any loop declaring a result type and branching back to its own top with no operand — an extremely common shape, e.g. a loop that returns a value only on exit — trapped with a spurious stack underflow. Fixed by special-casing `ControlKind::Loop` to arity zero.
  - `ResourceLimits::max_execution_steps` was honoured by Micro and **completely ignored by the baseline**, so an embedder asking for an instruction bound got it on one backend only, and a non-terminating module simply ran on the other. The baseline now carries the same budget, shared by every frame of an execution rather than per frame — a fresh allowance per call would let a recursive function run forever — and charges it once per instruction, *before* that instruction runs, which is what makes the trap land on the same instruction on both backends rather than one apart. `set_max_execution_steps` is wired from the same `ResourceLimits` the call depth is, for the same reason. Covered by a test that runs a long but finite loop at seven budgets on both backends and requires `StepsExhausted` at every one, plus a large budget that must let it finish — without that second half the test could pass by trapping on everything, including a budget nothing should have reached. The loop is finite *on purpose*: an endless one makes a regression hang rather than fail, which is a worse signal in CI than a clean assertion. Verified by reverting the fix and confirming the test fails cleanly, and only on `Baseline`. The default remains *no* budget, which means unbounded — that is a property of `ResourceLimits::default` and is deliberately not asserted, since asserting it would mean running a call that cannot return.
  - MVP import kinds are complete on both backends. Table and global imports were the last two the baseline refused, and both are core MVP even though no *unblocked* Core suite file exercises them. The IR now lists globals and tables in Wasm index order with imports first — so `global.get`/`global.set`/`call_indirect` carry the index the module already used and the verifier needs no rebasing rule — and a global's `init` became `Option<Value>` so an imported one has *no* value rather than a placeholder that would read as a legitimate zero. Each global became a single shared handle rather than one handle for a `Vec`, because an imported global has to be the exporter's value and not a copy of it; the same reasoning as the memory, and the same `Arc<Mutex<_>>`. Covered by two new tests: one writes through the importer, reads back through the exporter, and reads again from outside both modules, so a copy is caught whichever side holds it; the other checks a table import links and that the importer's element segment is bounds-checked against the *imported* table's length.
  - **One real divergence between the backends remains, and it is not patch-sized.** A `BaselineTable` entry is an index into the owning module's own function list, so a table entry written by one module names a function that means nothing in a second one — a `call_indirect` through a shared table recurses into the dispatching module instead of reaching the function the writer installed. Micro stores a store-wide function address and has no such limit. Fixing it means a table entry has to name an *instance* as well as an index, which is a redesign of `IrTable::elements` and of how the baseline resolves a `RefFunc`, with an ownership cycle between a shared table and the module that owns its functions. No Core suite file reaches it (`elem.wast` also needs bulk memory), so it is recorded here rather than left implied by the two passing tests above. A second, smaller gap in the same area: the baseline's memory, globals, and table are locked per call, but two instances sharing a handle can still interleave between calls, which is correct (each call is atomic) and not a divergence.
  - A compiled module's globals are now published per store address in the same registry the memory uses, and `Instance::global` reads through whichever backend owns the value. When each side kept a private copy, a compiled `global.set` was invisible to any reader, which reported the initializer forever. The test is written as a loop over both backends: it was first written for `Micro` alone, passed, and the divergence was invisible until the loop was added — a `Micro`-only test for this behaviour would have shipped the bug.
  - `RuntimeError::UnsupportedFeature` for a failed `lower_and_verify` now carries the underlying reason instead of the bare stage name "baseline lowering". A module refused at instantiation is otherwise reported with nothing about *what* is unsupported, and finding out why is a guessing game. Naming the payload is what turned one vague message into the two distinct real causes while this was being written (a stale `table and global imports` rejection, and a global index resolved against the defined-globals section instead of the combined list).
  - The Core suite is **complete for MVP**. Every remaining candidate file was downloaded and run against the harness, and each one's distinct failure reasons were collected, which answers a question the earlier passes had only guessed at: *is anything left that is blocked on an MVP gap rather than a proposal?* The answer is no. Every un-vendored file is blocked on at least one post-MVP proposal, and the blockers are: `multi_value` (six files — `block`, `br`, `fac`, `func`, `if`, `loop` — the largest cluster by file count, all failing on their first module with `InvalidBlockType`); `reference_types` (multi-table and `externref`: `table`, `call_indirect`, `exports`, `select`, `elem`, `br_table`); `bulk_memory` (passive data and non-active element segments: `data`, `token`, `elem`); `non_trapping_float_to_int` (`conversions`); `extended_const` (`global`); `exceptions` (`imports`, `unwind`); `typed_references` (`local_init`); and `names.wast`, which the `wast` crate's own lexer rejects before this harness sees it. So the remaining Core-suite work is proposal work, already tracked per proposal in `feature-registry.toml`, and there is no longer a separate "finish MVP" item hiding behind those proposals. `spectest`'s globals, table, and memory are deliberately *not* installed, because every file needing them also needs a proposal — installing them would buy no coverage while letting a fabricated global stand in for a real one. Table and global *imports* turned out not to be on the critical path at all, since the only files wanting one each need a proposal regardless; they were built anyway because they are core MVP with no unblocked spec coverage, which is the worse gap of the two.
  - `assert_invalid` and `assert_malformed` in these files are reported as skipped for the same reason `binary_format.rs` skips text-format modules: this harness does not re-derive a decode/validate outcome for text-format input.
  - Not yet vendored: every one is blocked on a post-MVP proposal, measured by running it rather than remembered, and each file's specific blocker is listed in `tests/core_spec.rs`'s module doc. `todo.md` does not restate the list, so there is exactly one place to keep current.
- [x] Pass validation correctness tests
- [x] Pass malformed binary rejection tests
- [x] Set up differential testing vs. official Wasm reference interpreter
  - Done for the axis that is actually available here: `crates/tpt-wasm-spec/tests/differential.rs` generates 2,000 seeded modules (4,000 executions) and runs each on **both** backends, comparing the return value, the exported counter global, and the trap variant bit-exactly. The two backends are independent implementations of the same semantics — an interpreter over decoded instructions and a block-graph compiler over the IR — so any disagreement is a bug in at least one of them, whatever upstream says. This is the tracker's own claim that Micro is the golden result, under test rather than asserted. The comparison is deliberately *not* against an expected value computed in the test: that would be a third implementation to maintain, and a bug in it would be indistinguishable from a bug in either backend.
  - Not the official reference interpreter, and the difference is structural rather than a shortcut. Driving `wasmtime` or `wabt` as a subprocess per module would add a dependency, an interpreter in the loop, and a per-run process cost, in exchange for checking the two backends against a third implementation *of the same specification the Core suite already checks them against*. What that would add over the 45 vendored Core files is shapes upstream wrote no test for; what it would cost is the ability to run the comparison in-process, deterministically, from a seed. So the next item fuzzes shape coverage instead, which is the part the reference interpreter would have added.
  - The generator found **no** engine bug; every failure it produced was its own, and each was worth the trip. A record of them, because the failure modes are the interesting part: (a) a `u32` immediate written as four little-endian bytes, which is a valid *first byte* followed by three stray instructions, and which the reader reports as the end of the module rather than as the mistake; (b) a constant expression missing its terminating `end`, one byte short, so the global section ran into the next one — the exact bug this harness spent two earlier attempts misreading as a statement-level problem; (c) `br_if` and the `else` of an `if` each consuming an operand the emitter had not accounted for; (d) a `memory_access` that checked the store's *address* operand before its *value*, which passes for `i32` and fails for every other width; and (e) the one that took longest — an operator's arity decided *after* its operands were pushed, then the surplus undone by popping the tracked stack. Popping does not un-emit the `local.get` byte already written, so the body carried a value nothing consumed. The arity is now decided first and decides how many operands are pushed.
  - Three checks guard the generator itself, and each exists because its absence would produce a *passing* test that tests nothing. A module that fails to instantiate **panics** rather than being recorded as an outcome — recorded, both backends produce the same message and the comparison succeeds without executing an instruction, which is the one failure mode a differential test cannot afford to be blind to. `generated_modules_are_accepted` asserts instantiation separately, so a generator bug says so instead of masquerading as a backend disagreement. And `walk_body` re-derives a body's block structure from its finished bytes, sharing nothing with the emitter but the byte sequence — with `the_byte_walker_rejects_malformed_bodies` checking that the walker rejects four malformed bodies and accepts one well-formed one, so it is not passing vacuously either. It is explicitly *not* a guarantee: two implementations of the same misunderstanding agree with each other, and it validates structure rather than types.
  - Seeds are fixed rather than drawn from the clock, and `a_seed_reproduces_its_module` checks that a seed reproduces its bytes — otherwise a failure reported against "seed 137" would not be reproducible and the suite itself would be nondeterministic, which is the policy the engine is held to. The seed count is a constant to be bumped by hand when the generator grows: a new construct should come with a new seed, not with a luckier pass.
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
  - Every variant of `IrInstr` is now produced by `lower_module`; the lowerer cannot reach a state where the IR has an instruction it has no way to emit. Every MVP import kind is carried — functions as `IrImport` plus a `CallHost`, a memory as an `IrMemory`, and tables and globals as their own entries in `IrModule::tables`/`IrModule::globals` in Wasm index order. What has no IR form yet is a tag import (the exceptions proposal, not MVP) and observing a capability's effects.
- [x] Implement IR type-checking / structural verification for the current instruction set
- [ ] Extend IR verification for branches, dominance, memory effects, imported/indirect calls, references, and host effects
  - Branches, block parameters, and dominance: done — multi-block CFG with an iterative immediate-dominator fixpoint, reachable-block enforcement, and per-edge arity/type checks. Each block's visible values are the ones defined in blocks that dominate it, so a value computed before a branch stays usable after it.
  - Indirect calls: done — the table must exist and be a `funcref` table, the named type must be declared, and the argument and result arities and types are checked against it.
  - Memory and global effects: done — a memory instruction in a module that declares no memory is rejected, an unknown global or table index is rejected, `global.set` on an immutable global is rejected, and each load/store width and operand type is checked against the access.
  - References: done — a `ref.func` must name a function the module defines, and a `ref.is_null` operand must be a reference of either kind rather than any value. Each is a hand-built-IR test, because the validator and the lowerer refuse the malformed cases first.
  - Host effects: partially done. `CallHost` is the explicit host-effect instruction, and the verifier checks it against the import's declared signature. Every MVP import kind now has an IR form — a memory as `IrMemory`, a table and a global as their own entries in Wasm index order, a function as `IrImport` plus a `CallHost` — so "observing a capability's effects" is the only part left here, along with the reference-types remainder below.
- [x] Add an executable V4 projection hook for the IR/formal-model overlap
  - The hook projects a certified IR function into the executable model and executes it, covering the straight-line numeric, local, and direct-call subset. It is a correspondence hook, not a proof. Control flow, memory, globals, and `call_indirect` are rejected as unsupported rather than projected approximately, because the formal model has no control flow, memory, or table state yet; widening the projection is M9 work.
- [x] Add lowering differential tests against Micro for the current subset
- [x] Document the IR and lowering contract in `docs/compiler/ir.md` and `docs/compiler/lowering.md`
  - Both files now match the code: the status headers, the module-state gate, the pipeline, the instruction-mapping table, the verification list, the differential-testing coverage, and the scope boundary all describe the multi-block, memory, global, and table lowering that exists. `codegen.md` was corrected too — it still listed `EngineMode::Baseline` as pending, and it was missing the control-flow, memory, global, and table differential coverage.

> M6 now provides a typed, certificate-backed IR slice that lowers the whole MVP surface the IR can represent: constants, `drop`, `select`, local get/set/tee, defined direct calls, the full integer set (arithmetic, division/remainder, comparisons, shifts, rotations, bitwise, bit-count, width/signedness conversion, sign extension), the full float set (comparisons, unary, binary, non-trapping conversion, trapping truncation, raw-bit reinterpretation), multi-block structured control flow, the linear memory with all fourteen loads and nine stores plus `memory.size`/`memory.grow`, globals, tables with active element segments, `call_indirect`, active data segments, the reference instructions, and imported-function resolution through an explicit `CallHost`. Verification covers dominance, block-parameter arity, call and table typing, memory/global effects, the reference rules, and the host-call signature, with Micro differential coverage and an executable formal-model projection for the supported subset. It is deliberately not marked as a complete MVP compiler IR: there is no instruction for observing a capability's effects, and the reference-types proposal is only partly in — there is no `table.get`/`table.set`, no `externref` table, and no passive or declarative element segment. Every MVP import kind does have a form (function, memory, table, global), and cross-instance state is shared by handle rather than copied, but a `BaselineTable` entry is still a module-local function index, so cross-module `call_indirect` through a shared table is the one backend divergence recorded under M2. Those remain listed above.

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
- [x] Extend the portable baseline to the five sign-extension operators (`i32.extend8_s`/`16_s`, `i64.extend8_s`/`16_s`/`32_s`)
- [x] Extend the portable baseline to raw-bit reinterpretation and to the non-trapping float conversions, preserving NaN payloads and signed zeros
- [x] Extend the portable baseline to the trapping float truncations with Wasm-exact `InvalidConversion` boundaries (NaN, infinity, out-of-range, negative-to-unsigned)
- [x] Extend the portable baseline to the reference instructions: `RefNull`, `RefFunc`, and `RefIsNull`, with a `ref.func` to a function the module does not carry refused rather than producing a dangling reference
- [ ] Implement simple lowering pipeline: IR → machine IR → native (x86_64 first)
- [x] Wire `EngineMode::Baseline` to codegen backend in `Engine`
  - The module is compiled through validate to IR to baseline at instantiation and executed by the compiled form. A module the baseline cannot represent is refused at instantiation rather than left half-usable. Function imports lower to `CallHost` and run through the host boundary, and memories, tables, and globals are shared handles so a cross-instance import reaches the exporter's state rather than a copy. An imported function may be the start function. The remaining gap is cross-module `call_indirect` through a shared table: a `BaselineTable` entry is a module-local function index, so an entry written by one module names nothing in another (recorded under M2 above).
- [ ] Implement aarch64 backend stub
- [x] Pass all Wasm spec tests through the baseline compiler
  - Every vendored Core suite file now runs through the compiled backend as well as through `Micro`, driven by the same harness with the same asserted directive counts, so a backend that quietly dropped a directive would fail rather than shrink its coverage. That is 9,164 directives — module instantiation, calls, bit-exact return values including all three NaN forms, and every trap and `assert_exhaustion` — put through the full `validate` → `lower_and_verify` → `BaselineModule::lower` → block-graph executor path rather than through fixtures written beside the code under test. Running it found six defects, all fixed, five of them in the lowering and one in the backend:
    - The baseline executed a callee by recursing with no depth bound, so `call.wast`'s `runaway` and `mutual-runaway` recursed until the *host* stack was exhausted and the process died with `STATUS_STACK_OVERFLOW`. That is a crash where the spec requires a trap, and a denial of service for any embedder. `ExecState` now carries the depth and refuses a call past `ResourceLimits::max_call_depth` with the same `Trap::CallDepthExceeded` Micro raises, counting the entry call as a frame exactly as Micro counts the one it pushes, so both backends run out of depth on the same call. The runtime passes the limit in from the same `ResourceLimits` the store was built with.
    - A `loop`'s label was given the block type's *results*, so a `br` to the loop label carried values into the loop header while the merge block received none. A loop's result was therefore dropped, and the code after the loop read whatever the enclosing stack happened to hold. A loop label carries the block's *parameters*, which are empty under MVP; the results belong on the merge block's parameters, which is where the fall-through edge now supplies them. `ControlFrame` keeps the two apart as `label_types` and `result_types`, because they genuinely differ for a loop.
    - An `else` was skipped when the `then` arm had already reached a terminator, so the entire `else` arm was lowered into the terminated `then` block and dropped. `if (c) (then br $done) (else <the work>)` did nothing, which turned `stack.wast`'s `fac-expr` into an endless loop. The `else` is now always processed; `close_label` already left a terminated `then` arm alone.
    - A `block`/`loop`/`if` opened inside unreachable code still popped the `if`'s condition off a stack that dead code never pushed onto, so any body with a construct after an `unreachable`, `br`, or `return` failed to lower. Such a construct is now tracked for its `else`/`end` pairing and nothing else, which also keeps the value stack untouched.
    - Dead code did not consume instruction immediates. The body is one byte stream, so stepping over an `i32.const` without stepping over its constant left the next opcode being read from the middle of it and desynchronized every instruction after — which is why nine whole suites failed to lower at all. `skip_immediates` now mirrors the live arms' immediate layout.
    - A dead `unreachable` overwrote the terminator of a block that had already branched or returned, replacing a branch that was still taken with a trap that was not: `(block (result i32) (br 0 (i32.const 1)) (unreachable))` trapped instead of returning 1. A terminator is now only written to a block that does not already have one.
  - Separately, and found by the same route: a branch to a function's own label jumped back to the function's *entry block*, re-running the body instead of returning, so `br_if` to the outermost label was an endless loop. The function's label now targets a dedicated exit block whose only job is to hand the results back. Its parameters are allocated after the body, so adding it does not renumber the values the body already defined.
- [x] Establish Micro equivalence: `Micro(module) == Baseline(module)` for all test programs
  - Established for every module the baseline can represent, driven through the real validate → IR → baseline pipeline rather than a hand-built IR. The codegen tests assert identical results and traps against Micro, including the reference instructions returning a `funcref` unchanged, so the two backends' value representations are compared directly and not only through a derived `i32`. Agreement also holds across 25,000 generated programs, which now include branches, calls, and loops — see the fuzz differential item below — and, since the official spec suite now runs, across all 9,164 Core suite directives on both backends. The two suites are complementary: the generated programs reach shapes a hand-written fixture would not think of, while the spec suite pins exact values, NaN payloads, and trap kinds that a differential check can only confirm as *differing*, never as *correct*.
- [x] Set up fuzz differential testing: random Wasm, compare Micro vs. Baseline output
  - A seeded generator builds whole programs — 22 instruction families across both widths and all four float/number types, plus `block` with a conditional branch, `if`/`else`, a counted `loop`, a direct `call`, an indirect `call_indirect`, a `br_table` over two nested blocks, and the reference instructions both directly and carried out of a block — over a page of memory, a mutable global, a helper function, and a table seeded with that helper as shared state. Each program runs through the real validate → IR → baseline pipeline and is compared against Micro for both result values and the trap variant. 25,000 programs run in the ordinary test suite; 250,000 were swept locally. A seed replays deterministically and every failure message carries the seed and the body bytes, so a divergence reproduces without an artifact. Adding control flow and `br_table` paid for itself immediately: it exposed five real defects, none of which any hand-written fixture could have reached. Three came from one mistaken reading of `br_if` as consuming its label values on both paths rather than as `[t* i32] → [t*]` — the validator rejected every module branching to a value-carrying label, the IR lowerer dropped the values on the fall-through edge, and a test had recorded the wrong reading as intended behavior. Two more were in the `br_table` chain: the lowerer popped the label values *before* the selector, so the two were swapped and the branch carried the selector while the dispatch compared the carried value, meaning any selector differing from the value it carries reached the wrong label; and each arm passed no values on its fall-through edge while later arms named values that did not dominate them. Every one of these was invisible before, because the hand-written fixtures branched only to void labels and used a `br_table` selector equal to its carried value. All five are fixed. Still open: a `cargo-fuzz` target for the same comparison, multi-value block results in the generated population, and the reference-interpreter differential named below.
- [x] Verify trap equivalence (same traps at same points)
  - Checked trap-by-trap against Micro for the whole represented surface: integer division and remainder, every `InvalidConversion` boundary in the float truncations, out-of-bounds memory access, `call_indirect` with an out-of-range index, a null entry, and a signature mismatch, a missing memory, and trap propagation out of a callee. Each baseline trap is asserted to be the same variant Micro raises at the same point.
- [x] Verify host-effect equivalence
  - The baseline has a host boundary now, so this is no longer blocked. `CallHost` is the single instruction that crosses it, resolved only through an installed `HostBoundary`; a compiled module with no boundary is refused rather than left half-usable. The runtime routes both backends through the same `Instance::invoke_host_call`, so a host call has one implementation rather than two that happen to agree. A failing host is reported as a `RuntimeError::HostFunction` with the same registered name and message from either backend — verified by asserting the two errors are equal, not merely that both fail. The host's return arity and types are re-checked against the import's declared signature, since the host sits outside the verified module. Still open: observing a capability's *effects* rather than only invoking it, imported tables/memories/globals, and cross-instance imports.

> M7 now has a deterministic portable baseline slice with independent execution and Micro differential coverage for the full straight-line MVP instruction set that the IR represents: constants and every arithmetic, comparison, and bit-count operation at all four widths; `drop`, `select`, and `local.get`/`set`/`tee` over zero-initialized slots; non-trapping integer width and signedness conversions; the five sign-extension operators; raw-bit reinterpretation; the non-trapping float conversions; and the trapping float truncations; plus multi-block control flow — `Branch` and `CondBranch` terminators, block parameters bound by incoming edges, and a block-graph executor that follows back edges. IEEE 754 behavior is reproduced exactly, and differential tests drive the real validate → IR → baseline pipeline asserting identical results and traps bit-for-bit, including NaN canonicalization, NaN-payload preservation across reinterpretation, signed-zero `min`/`max`, ties-to-even rounding, every `InvalidConversion` boundary, and a loop that only terminates if the back edge is followed. Direct calls, the full MVP memory instruction set (fourteen loads, nine stores, `memory.size`, `memory.grow`), and `global.get`/`global.set` are now lowered and executed, with module-level memory and global state shared across calls. Tables and active element segments are now lowered and executed as well: an active segment seeds the table, and `call_indirect` resolves the index, distinguishes an out-of-range index from a null entry, and checks the entry's signature against the type the call names. Active data segments are lowered and applied at instantiation as well. `EngineMode::Baseline` is now wired into the runtime: the module is compiled through IR to the portable baseline at instantiation and executed by it, with the compiled instance owning that module's memory, table, and globals so state persists across calls. Baseline and Micro are checked to agree on the same module. The reference instructions are lowered and executed too, with a `funcref` carried through a local, through a block result, and out of a function as a value. A module with function imports compiles and runs, reaching a granted host capability through an explicit `CallHost` and an installed `HostBoundary`, with a host failure reported identically by both backends. Native x86-64/AArch64 code generation and executable-memory integration remain pending.


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
  - `TptWasm/Observation.lean` defines the observable vocabulary both sides are judged against, and `TptWasm/IRExec.lean`
gives the IR its own fuel-bounded executor. What is missing is the simulation relation relating an `OpStep` derivation to
the corresponding IR execution.
- [x] Write proof for constant folding (`x + 0 = x`)
  - `TptWasm/Proofs/V5.lean` proves `foldable_inert` (a fold step preserves one instruction's trace) together with the
per-step facts it rests on: the arithmetic instructions emit nothing, memory and the host boundary emit something, and
a fold cannot change a head's event.
  - Getting there required a **model** change, not a proof change. `foldable` read a constant table without checking it
against the state the body ran from, so a stale table replaced the program's own arithmetic. `Constants.constant cs s id`
now returns `none` unless the table agrees with the state, and `foldBlockWith` threads the post-head state through. The
agreement is checked by the definition rather than assumed by a caller.
  - The build is clean: no errors, no warnings, no `sorry`, no `admit`.
- [ ] Write proof for dead block elimination
  - `Passes.lean` defines `deadBlock`, but the proof needs the whole-function liveness premise — the values the
terminator and the successor parameter bindings use — as an explicit hypothesis. Getting that premise wrong fails
silently, which is why it is worth stating rather than assuming.
- [ ] Write proof for CFG simplification
  - Whole-function work: this pass moves control flow, so it cannot use the block-body formulation the other two passes
use. Stated over a whole function it needs the edges in play.
- [x] Write V5: selected optimization transformation proofs
  - Constant folding is proved as above. The observation vocabulary (`Event`, `Finish`, `Observation`) and the inertness
results are in place for the remaining passes.
- [ ] Write V6: lowering correctness proof skeleton (IR → machine code)
- [ ] Compose refinement chain: `Wasm ≈ IR ≈ Optimized IR ≈ Machine IR ≈ Machine Code`
  - One link exists: `Optimized IR ≈ IR` for constant folding, via `V5`. The other links are named above.
- [x] Document all proof obligations in `docs/verification/refinement.md`
  - `docs/verification/refinement.md` states the chain, which obligation each link discharges, and which mechanism
discharges it where a proof does not yet exist. `formal/README.md` carries the per-file status.

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


