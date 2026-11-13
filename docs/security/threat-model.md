# Threat model

This document covers the subsystems `spec.txt` section 43 names as security boundaries:
decoder, validator, interpreter, host, resource system, compiler, and executable memory.
For each threat it says which mechanism addresses it and which test or tool checks that
mechanism. Where nothing checks a claim, the entry says so. A threat with no mechanism is
listed under **Open**.

Evidence is named by test or file, so each claim can be re-run. Fuzzing evidence is a count
of runs without a failure. It is evidence, not a proof of absence.

## Decoder (`tpt-wasm-decode`)

| Threat | Mechanism | Evidence |
|---|---|---|
| Malformed binaries | Every read is bounds-checked; EOF and bad LEB128 are errors, not panics | Malformed-binary rejection tests (M1); `decode` fuzz target: 14.4M runs in 5 minutes, no failure |
| Oversized allocations | Vectors grow one decoded element at a time, so a declared length cannot allocate more than the input supplies. Local counts are summed in `u64` and capped at 2^32-1 | `decoder.rs` `vector` and the local-count check; fuzzing as above |
| Integer overflow | Offsets use `checked_add` (`decoder.rs`); LEB128 widths are checked against the target type | Decoder tests; fuzzing as above |
| Recursion | Body decoding and validation are iterative. The validator keeps an explicit control stack | `tpt-wasm-micro/src/instr.rs` `decode_body`; `tpt-wasm-validate/src/body.rs` |
| Nesting exhaustion | No recursion on nesting, so depth costs heap, not host stack. A module nesting 100,000 blocks runs on every engine in about a second | `deeply_nested_blocks_do_not_exhaust_the_host_stack` (runtime) |
| Oversized memory declaration | Declared memories above the resource limit are refused by every backend | `an_oversized_memory_is_refused_everywhere_and_not_allocated` (oracle). Found by the `differential` fuzzer, kept as `fuzz/artifacts/differential/oom-*` |

## Validator (`tpt-wasm-validate`, `tpt-wasm-ir` verifier)

| Threat | Mechanism | Evidence |
|---|---|---|
| Type confusion | Full type checking of every instruction before execution; the IR verifier checks types again after lowering | Validation error tests (M1); IR verifier tests (M6) |
| Out-of-range branch labels | Label lookup is checked (`label_types`, `checked_sub`) | Validator tests |
| Lowering that breaks typing | The IR verifier runs after lowering and after optimization, and the contract checker compares optimized and original traces | `tpt-wasm-opt/src/contract.rs`; the optimizer tests |

## Interpreter (`tpt-wasm-micro`)

| Threat | Mechanism | Evidence |
|---|---|---|
| Memory out-of-bounds | Every load and store is range-checked and traps | Core spec suite on Micro (16,075 assertions); memory tests |
| Table out-of-bounds | Indirect calls check the index against the table and trap | Core spec suite; `an_indirect_call_out_of_range_traps_like_micro` |
| Stack exhaustion | Operand stack depth is limited by `max_stack_depth`; call depth by `max_call_depth`. Both trap with `StackOverflow` or `CallDepthExceeded` | `machine.rs` (`StackOverflow` at push); call-depth tests |
| Runaway execution | Instruction budget (`max_execution_steps`) charged per step | `both_backends_enforce_the_instruction_budget` |
| Reference errors | Null and out-of-range references trap; `ref.func` to a missing function is refused | `a_ref_func_to_a_missing_function_is_refused`; reference tests on both backends |

## Host (`tpt-wasm-host`, `tpt-wasm-capability`, `tpt-wasm-resource`, `tpt-wasm-wasi`)

| Threat | Mechanism | Evidence |
|---|---|---|
| Capability escalation | A capability is invoked only with a matching grant on the exact operation. Grants are explicit | `ungranted_and_undergranted_operations_cannot_invoke`; `malicious_module_cannot_use_ungranted_capability_and_hosts_are_isolated` |
| Confused deputy | Each invocation names the capability and operation; no ambient identity is consulted | Host tests above |
| Ambient authority | No filesystem, clock, random, or stdio authority unless granted. The WASI adapter installs only explicit bindings. The Preview 1 core takes every stream, clock seed and argument as an explicit grant | `wasi_adapter_installs_only_explicit_bindings`; `unbound_preview1_import_is_not_installed`; the Preview 1 tests |
| Resource lifetime | Handles are opaque and monotonic; a removed handle is invalid; a wrong-type removal keeps the resource | `handles_are_opaque_monotonic_and_invalidated_on_remove`; `wrong_type_removal_preserves_the_resource` |
| Revocation | Revoking a capability removes authority for later calls | `matching_grant_invokes_and_revocation_removes_authority` |
| Reentrancy | Calls on one engine are serialized by a call gate. A host function receives only its arguments and the caller's memory, not an engine handle, so it cannot call back into the engine | `call_gate` in `tpt-wasm-runtime`; while a debug session is open, other calls and instantiation are refused (`while_a_session_is_open_no_other_instance_can_call_or_instantiate`). No dedicated test for host-side reentrancy, because the structure rules it out rather than a check |
| Host-side memory access | A host function's view of memory is a bounds-checked `GuestMemory`; an out-of-range access is reported, not a panic | `host_function_reaches_the_callers_memory_on_both_backends` (an out-of-range write is a host failure) |

## Resource system (`tpt-wasm-runtime` limits)

| Threat | Mechanism | Evidence |
|---|---|---|
| Unbounded memory | `max_memory_pages` caps growth; `memory.grow` past the cap returns -1 | `memory_growth_per_call_agrees_on_both_backends` |
| Unbounded tables | `max_table_elements` caps table size; growth past it is refused | `table_allocation_bounds_and_reference_types_are_checked` (`tpt-wasm-micro` store) |
| Unbounded instances | `max_instances` caps the store; the instance past the cap is refused | `the_instance_limit_refuses_the_instance_past_it` (`tpt-wasm-micro` store) |
| Unbounded time | `max_execution_steps` (see Interpreter) | Budget tests |

## Compiler (`tpt-wasm-opt`, `tpt-wasm-codegen`, `tpt-wasm-ir`)

| Threat | Mechanism | Evidence |
|---|---|---|
| Incorrect bounds elimination | A bounds check is removed only when the access provably fits the memory | `bounds_checks_are_removed_only_when_the_access_fits_the_memory`; the contract checker |
| Control-flow corruption | The IR verifier checks dominance, edge arity and types after every pass; the contract checker compares traces | IR verifier tests; `contract.rs`; `differential` fuzzer: 9.46M runs after the optimizer fixes |
| Register allocation bugs | Allocation runs last and is covered by the same differential tests | Optimizer differential tests; `differential` fuzzer |
| Calling-convention errors | The baseline's internal convention is checked by the same call tests as Micro | `baseline_matches_micro_for_a_direct_call`; recursion-depth tests |
| Super-linear compile time | Fixed in this round: the CFG pass, dominance fixpoint, branch-type check and contract walk were quadratic or worse on nested blocks. A 100,000-block nest compiles and runs in about a second on the optimizing engine | `deeply_nested_blocks_do_not_exhaust_the_host_stack` |

## Executable memory (`tpt-wasm-jit`, `tpt-wasm-aot`)

`tpt-wasm-jit` generates x86-64 machine code for a subset of the IR (see its crate
documentation). `tpt-wasm-aot` stores that code as an artifact and loads it back. The
executable-memory boundary is the one place in the workspace where `unsafe` code is allowed to run
generated code.

| Threat | Mechanism | Evidence |
|---|---|---|
| Writable and executable pages at the same time | Write-then-execute: a page is mapped read-write, the code is copied in, then the page is changed to read-execute. No page is both writable and executable | `exec.rs` (`ExecutableCode::new`); the protection sequence is in the module's documentation |
| Patching installed code | The mapping is read-execute once installed and is never written again | `exec.rs`; the `Send`/`Sync` impls rely on this and say so |
| Unsafe code outside the boundary | `#![deny(unsafe_code)]` at the crate root; the only `allow`s are on the `exec` module, which maps and calls executable memory, and on the host trampoline, the one function through which compiled code calls a host import | `crates/tpt-wasm-jit/src/lib.rs` |
| Generated code that does not match the IR | Encoder is safe code, tested on every target: golden encodings for the prologue and a constant return, register and ABI tests, and a back-edge test. Generated code is run against Micro on generated programs, including loops, conditional branches, `select`, `local.tee`, shifts past the operand width, and comparisons | `tests/differential.rs` (straight-line: 600 functions) and `tests/control_flow.rs` (control flow: 2,880 runs), on Windows x64 and, under WSL, Linux System V. All pass; a guard requires varied results |
| Code that the encoder cannot handle is compiled anyway | Anything outside the subset is refused with `Unsupported`, so no approximation is generated. Division is compiled with its traps, and each trap is reported in a status half of the 64-bit return value. `i32` and `i64` arithmetic, comparison, conversion and memory access are compiled; operations outside the subset (floats, calls, memory growth) are refused | `an_f32_value_is_refused_on_every_target` (an `f32` value); `tests/wide.rs` (`i64` arithmetic, memory widths and traps agree with Micro); `tests/division.rs` (every trap and value outcome agrees with Micro, all four operations). A branch to a missing block and a branch with the wrong arity are refused by the encoder, with no test of their own yet |
| Native code runs where the engine's own accounting should apply | The `EngineMode::Jit` native tier is off for an instance with a step limit, profiling or debugging, since native code charges no steps and keeps no profile. A native function reads memory only through the baseline's memory under its lock, and makes no calls to functions outside its module. A call to a host import goes through the same boundary as the baseline's, and charges no steps, which is why the tier is off under a step limit | `tests/jit_tier.rs` (`a_step_limit_keeps_every_call_on_the_baseline`, `a_native_load_sees_what_a_baseline_store_wrote`), run with `--features jit` |
| Compiled float code departs from Wasm's rules (NaN, signed zero, rounding) | Arithmetic uses the hardware's IEEE operations with round-to-nearest-even; comparisons are the ordered ones the specification gives, so an unordered pair is false for every ordering and true only for `!=`; negation and absolute value flip or clear the sign bit, which is exact for every input. `sqrt`, `min`, `max` and `copysign` follow Micro's rules, with `min`/`max` resolving signed zeros and NaN exactly as Micro does. The rounding functions follow Micro's rules as well: on x86-64 they use `roundss`/`roundsd` and need SSE4.1, which the target name records, so a host without it refuses them; AArch64 uses its `frint` instructions; RISC-V has no rounding instruction, so it converts through a 64-bit integer below 2^52 (2^23 for `f32`) and copies the operand's sign onto the result, which makes a zero result `-0` for a negative operand, as Micro does. The conversions are compiled with Micro's rules: an integer to a float rounds to nearest-even, the unsigned 64-bit source is exact on every target, and a width change canonicalizes a NaN. A float-to-integer truncation traps with `InvalidConversion` on a NaN, an infinity, or a truncated value outside the target's range; the range is checked on the truncated value, where a NaN fails both comparisons, so no separate NaN test is needed. On x86-64 the truncation uses `roundsd`, so it needs SSE4.1 as the rounding functions do | `tests/float.rs` (arithmetic, comparisons, `neg`/`abs`, constants and memory against Micro, on x86-64, AArch64 and RISC-V under QEMU), compared bit for bit. A NaN produced by arithmetic is replaced by the canonical NaN on every target, as Micro's deterministic mode does; the hardware's own NaN differs (x86 makes a negative one for `inf - inf`), and this divergence was found by `fuzz/fuzz_targets/jit_module.rs`, not by the hand-written tests; `tests/jit_tier_float.rs` through `EngineMode::Jit` |
| A host call from native code reaches the wrong import, passes or returns the wrong words, lets the host read or write memory it was not lent, or lets a host panic unwind through compiled frames | The compiled code calls one trampoline, with the context, the import index, and the argument and result blocks of its own frame. The trampoline gets the import's arity from the host, and the call goes through the same `invoke_import` the baseline uses, which checks the host's return against the import's declared arity and types. The host is lent exactly the `memory_len` bytes of the call, and its writes are bounds-checked. A panic in the host is caught at the trampoline. A failure sets a status that the compiled code returns on, and the runtime reports the host's own error, not a status | `tests/native_host.rs` (results, a refusing host, a host that writes memory the native code then reads, and host call counts, against Micro and the baseline, on all three targets), and the module fuzzer, which generates host calls, refusals and memory writes and compares every outcome with Micro |
| Native growth of a memory writes past its reservation, or leaves the size the runtime reads disagreeing with the memory | The memory is reserved to its declared maximum, or to the engine's memory limit if that is lower, and only when that maximum is at most 16 MiB. Compiled code checks the new size against the reservation before it stores the size, and a grow past the reservation returns -1, as a grow past the maximum does. The runtime takes the size back only through a setter that refuses a size past the allocation. The reservation changes capacity, not contents, so the bytes a program can read are unchanged | `tests/jit_tier_grow.rs` (growth to the maximum, a grow past it, loads and stores on the grown pages, a trap past the end, a maximum above the cap left to the baseline, and the engine's limit on every engine) |
| A leaf function that uses a callee-saved register returns to its caller with that register changed | On x86-64 a function that makes no call may keep values in rbx and r12 to r15. Each such register it uses is saved to a frame slot on entry and restored before every return, and every trap return goes through the same epilogue. Functions that make calls use no callee-saved registers | `a_leaf_function_saves_and_restores_each_callee_saved_register_it_uses` in the encoder, which checks that rbx and r12 are each saved on entry and restored before the return; the differential and module fuzzing suites check the results of such functions. Nothing reads the caller's registers after a call, so the preservation itself is checked by the encoding, not by behaviour |
| A native call reaches the wrong function, runs past the call-depth limit, or overflows the native stack | Calls go through a table of entry points that is filled only for functions compiled in the same module; a function that calls an uncompiled one is itself left out, by a greatest fixed point, so no call target is ever empty. Each call checks the active-frame count against the limit before it proceeds, the count being Micro's, and a trap in a callee returns through each caller with its status. When the deepest chain of calls could outgrow a 512 KiB native stack budget, the functions that call are not compiled. No code is patched after installation | `tests/calls.rs` (recursion, mutual recursion, a trap two calls deep, the depth limit against Micro, a refused callee, a deep caller with a large frame), and `tests/jit_tier_calls.rs` through `EngineMode::Jit`. The frame size is estimated from the IR, and the test reads each compiled function's prologue back and checks it against that estimate on all three targets; a single estimate is checked, not every frame the encoder could ever emit |
| A native indirect call reaches a function the table did not name, skips a check the baseline makes, or runs against a table another instance changed | The table is read from the IR at compile time, and compiled only when this module defines it and does not export it, so no other instance can write it and the IR holds its contents for good. The index is checked against the length, the slot against null, and the slot's signature against the call's, in Micro's order, each with its own trap status. A slot holding a function not compiled in this module keeps the caller on the baseline. The signature test is an integer comparison of canonical ids, each the first module type equal to the function's type | `tests/jit_tier_indirect.rs` through `EngineMode::Jit` on all three targets: success, out of range, null, and mismatch, each against Micro, and the exported-table variant left to the baseline |
| Calls with the wrong arity | The call checks the argument count before entering the code | `a_call_with_the_wrong_arity_is_refused` |
| Unbounded resources | One mapping per compiled function, page-rounded; the mapping is released when the handle is dropped | `Drop for ExecutableCode`. No test measures leaks yet |
| Calling convention errors | The compiled function takes a pointer to its arguments and a pointer to a context, in the first two integer argument registers of the host ABI (System V: `rdi`, `rsi`; Win64: `rcx`, `rdx`). Both are kept in frame slots, because the code uses every scratch register. The Win64 and System V registers are distinct and tested | `Abi` in `x86.rs`; the golden prologue encoding |
| Calling convention errors on AArch64 and RISC-V | The same two-pointer convention on AAPCS64 (`x0`, `x1`) and LP64 (`a0`, `a1`), with the result in the first return register. Each is run under QEMU against Micro | the aarch64 and riscv64 differential runs (`tests/*.rs` under `qemu-aarch64` and `qemu-riscv64`) |
| Stale instruction cache | AArch64 cleans the data cache and invalidates the instruction cache for the written range, with barriers. RISC-V runs `fence.i`. x86-64 keeps the two coherent. A missing flush would run old bytes | `flush_instruction_cache` in `exec.rs`. Not tested directly: QEMU does not model a stale cache, so the flush is checked by the code and the barrier order, not by a test |

| Out-of-bounds linear memory access in compiled code | Every load and store computes its effective address in 64 bits (so it cannot wrap), checks `address + width <= memory_len` from the context, and traps before touching memory. The bounds hint in the IR is ignored, so an access the optimizer believed safe is still checked | `tests/memory.rs`: 240 programs with random widths, signedness and offsets, addresses near the page end, and each result and the full page compared with Micro. A call without memory traps |
| Memory access through a wrong context | The context is a `#[repr(C)]` struct owned by the call frame; the base pointer is read only after the check, and a call with no memory gets a null base with length zero, so no access reaches it | `Context` in `lib.rs`; `a_call_without_memory_traps_on_any_access` |
| Corrupted artifact | FNV-1a 64 checksum over the whole artifact; truncation at every length, a flipped byte, a wrong magic, and trailing bytes are each refused | `tpt-wasm-aot` tests (`a_flipped_byte_is_a_checksum_mismatch`, `a_truncated_artifact_is_refused_at_every_length`, and others) |
| A cached native module is loaded from a file another user wrote or could modify, or a stale or corrupt entry runs | The cache is opt-in (`Config::native_cache_dir`), on Unix and Windows; other platforms refuse to open it. It opens only a directory the current user owns that no other account can change: on Unix, no one else may write to it; on Windows, no other account may hold a write-type right in its access list. An entry loads only if it is a regular file owned by the current user that no other account can change: on Unix, with no group or other mode bits; on Windows, with no write-type right granted to another account. Unlike Unix, a read-only grant to another account on Windows is accepted, because only a write can inject code; the trusted accounts there are the current user, the creator owner, LocalSystem and the built-in Administrators, and a missing access list or an unrecognised allow entry is refused. An entry that fails the check is ignored, and the module is compiled again, and the new entry replaces the refused one. Entries are named by the module's SHA-256, the call-depth limit and the target, and the artifact checks the same, so a mismatch is refused. Entries are written to a private temporary file and renamed into place. The check is ownership and mode, not a signature: a file the current user owns is trusted as that user's own code | `tests/native_cache.rs` (a compiled module reused by the next engine, a refused entry replaced, a corrupt entry ignored, a directory others can write refused at open), run under Linux and QEMU; `tests/native_cache_windows.rs` (the same cases, with access control entries granted through `icacls`, plus the read-only grant accepted), run on Windows; `tests/native_cache_platform.rs` for the refusal on other platforms |
| A module artifact's table or calls disagree with its code, or its counts reserve more memory than its bytes can hold | Counts are checked against the bytes that remain before anything is reserved. At load, a table slot must name a function the artifact defines, a null slot must carry the null signature id, and an artifact whose indirect calls are compiled must have every slot name a compiled function. A checksum does not authenticate the code, which is the same trust boundary as every artifact: a module artifact is code, so it is loaded only from a store its loader trusts | `tpt-wasm-aot` `module_tests` (a round trip that runs as the in-memory module does, every truncation and a flipped byte refused, a target mismatch, and a table naming an uncompiled function refused for indirect calls) |
| Forged artifact | Not defended. The checksum is not a signature. Loading an artifact runs its code, so artifacts must come from a store the loader trusts | Stated in the crate documentation. Signing is not implemented |
| Artifact run on the wrong target | The target name is recorded and checked before the code is installed | `an_artifact_for_another_target_does_not_load` |
| Encoder bugs found by fuzzing | The `jit` fuzz target compares compiled code with Micro on random stack programs, including division and control flow | `fuzz/fuzz_targets/jit.rs`: 370,666 runs in eight minutes and 132,311 in two, no divergence and no crash |

Open for this boundary: no test that a freed mapping is unreachable; no defence against a host that changes page permissions underneath the JIT.
The JIT is not yet reachable from `Engine`: an embedder who calls it directly accepts these
risks, and the engine modes do not change until the same differential tests cover the mode.

## Open

- **Host stack per baseline frame.** The depth limit of 1000 needs about 1.0–1.3 MiB of host
  stack in a debug build and about 0.5–0.75 MiB in release. An embedder on a smaller thread has
  no guarantee that the limit is reached before the stack is exhausted. Tracked in `todo.md`.
- **Super-linear worst cases beyond nesting.** The nesting fixes address the shapes the fuzzers
  and this test produce. Other shapes, such as many uses of values defined deep in a chain, may
  still cost O(depth) per use in `dominates`. Not measured for every pass.
- **Compile-time bounds.** Validation and optimization time is not bounded by a resource limit.
  Only execution is. A module can still be expensive to compile even though it is small to
  download.
- **Miri.** `tpt-wasm-codegen` did not finish within the Miri time limit, and `tpt-wasm-opt`
  was not clean within it earlier. Those crates have no Miri result yet. Miri cannot run the
  JIT's generated code, so the JIT's encoder tests are its only Miri-checkable part.
- **Fuzzing duration.** The fuzzing above is mostly 5-minute runs. The module fuzzer, which now generates host calls, refusals and memory writes, ran twice for eight minutes each under WSL (41,365 and 41,320 inputs, different seeds) with no divergence. Longer campaigns of the other targets have not been run.
