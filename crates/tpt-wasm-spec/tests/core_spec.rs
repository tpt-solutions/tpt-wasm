// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Runs the vendored WebAssembly Core spec test suite through the real
//! decode/validate/execute pipeline.
//!
//! `.wast` text is turned into bytes by the upstream `wast` crate; everything
//! downstream -- decode, validate, instantiate, call, and comparing results --
//! is tpt-wasm's own code. See `crates/tpt-wasm-spec/src/core.rs` for the
//! executor and why that division keeps the decoder/interpreter the thing
//! actually under test.
//!
//! Each suite's directive counts are asserted exactly, the same way
//! `binary_format.rs` asserts assertion counts: a change in the vendored file,
//! or a change in which directives this harness runs versus skips, changes
//! coverage, and an unexplained change in coverage should fail the build
//! rather than pass silently. The counts are asserted on both execution
//! backends, so a backend that quietly dropped a directive fails too.
//! `assert_invalid` and `assert_malformed` in these
//! files are all written in the text format and are counted as skipped (see
//! `core.rs`); this suite does not (yet) re-derive the decode/validate outcome
//! for a text-format module the way `binary_format.rs` does for the binary
//! form.
//!
//! `core_spec_suite` runs the directives on `Micro` and
//! `core_spec_suite_on_baseline` runs the identical directives on the compiled
//! backend, so every module goes through `validate` -> `lower_and_verify` ->
//! `BaselineModule::lower` -> the block-graph executor and is held to
//! upstream's own expected values and traps. Everything between the decoded
//! bytes and the compared result is the same code on both sides, so a
//! disagreement is a disagreement about execution.
//!
//! Not every Core suite file is vendored yet, and the reason is now measured
//! rather than remembered: every remaining candidate was downloaded and run
//! against this harness, and **every one of them is blocked on at least one
//! post-MVP proposal**. There is no remaining MVP gap in the Core suite -- what is
//! left is proposal work, tracked per-proposal in `feature-registry.toml`. The
//! blocker behind each file, as the error a single failing directive reports:
//! - `multi_value` -- a type-index block type (`(block (result i32 i32) ...)`).
//!   Only the empty and single-result forms decode today. Six files:
//!   `block.wast`, `br.wast`, `fac.wast`, `func.wast`, `if.wast`, `loop.wast`,
//!   each failing on its first module with `InvalidBlockType`. This is the
//!   largest remaining cluster by file count.
//! - `reference_types` (multi-table and `externref`) -- `table.wast`,
//!   `call_indirect.wast`, `exports.wast`, `select.wast` need more than one
//!   table (`MVP modules may contain at most one table`); `table.wast`,
//!   `elem.wast`, and `br_table.wast` also need a non-`funcref` table or a
//!   heap type this decoder does not recognize.
//! - `bulk_memory` -- `data.wast` and `token.wast` need passive data segments
//!   (`passive data segments`); `elem.wast` needs non-active element segments.
//! - `non_trapping_float_to_int` -- `conversions.wast` needs the saturating
//!   `0xfc`-prefixed truncations (`InvalidInstruction(252)`).
//! - `extended_const` -- `global.wast` needs arithmetic inside a global
//!   initializer (`InvalidOpcode(108)`), and `spectest`'s globals.
//! - `exceptions` -- `imports.wast` needs the tag section
//!   (`InvalidSectionId(13)`); `unwind.wast` is the proposal outright.
//! - `typed_references` -- `local_init.wast` needs `(ref null extern)`.
//! - `names.wast` contains identifiers with bidirectional-control Unicode
//!   characters that the `wast` crate's lexer refuses outright, before this
//!   harness ever sees them.
//!
//! `spectest`'s *functions* are installed (see `core.rs`), which is why
//! `start.wast` and `func_ptrs.wast` are in the table below. Its globals, table,
//! and memory are deliberately not installed: every file that needs them also
//! needs a proposal, so installing them would buy nothing while letting a
//! fabricated global stand in for a real one.

use std::collections::HashMap;

use tpt_wasm_runtime::EngineMode;
use tpt_wasm_spec::{run_core_suite_with, CoreOutcome};

/// Each suite, with the exact count of every directive kind it contains.
/// A directive kind absent from a suite's map is expected to occur zero times.
struct Suite {
    name: &'static str,
    source: &'static str,
    directives: &'static [(&'static str, usize)],
}

const SUITES: &[Suite] = &[
    Suite {
        name: "i32.wast",
        source: include_str!("../testdata/i32.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 364),
            ("assert_trap", 10),
            ("assert_invalid", 83),
            ("assert_malformed", 2),
        ],
    },
    Suite {
        name: "i64.wast",
        source: include_str!("../testdata/i64.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 374),
            ("assert_trap", 10),
            ("assert_invalid", 29),
            ("assert_malformed", 2),
        ],
    },
    Suite {
        name: "f32.wast",
        source: include_str!("../testdata/f32.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 2500),
            ("assert_invalid", 11),
            ("assert_malformed", 2),
        ],
    },
    Suite {
        name: "f64.wast",
        source: include_str!("../testdata/f64.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 2500),
            ("assert_invalid", 11),
            ("assert_malformed", 2),
        ],
    },
    Suite {
        name: "const.wast",
        source: include_str!("../testdata/const.wast"),
        directives: &[
            ("module", 402),
            ("assert_return", 300),
            ("assert_malformed", 76),
        ],
    },
    Suite {
        name: "local_get.wast",
        source: include_str!("../testdata/local_get.wast"),
        directives: &[("module", 1), ("assert_return", 19), ("assert_invalid", 16)],
    },
    Suite {
        name: "local_set.wast",
        source: include_str!("../testdata/local_set.wast"),
        directives: &[("module", 1), ("assert_return", 19), ("assert_invalid", 33)],
    },
    Suite {
        name: "local_tee.wast",
        source: include_str!("../testdata/local_tee.wast"),
        directives: &[("module", 1), ("assert_return", 55), ("assert_invalid", 42)],
    },
    Suite {
        name: "nop.wast",
        source: include_str!("../testdata/nop.wast"),
        directives: &[("module", 1), ("assert_return", 83), ("assert_invalid", 4)],
    },
    Suite {
        name: "unreachable.wast",
        source: include_str!("../testdata/unreachable.wast"),
        directives: &[("module", 1), ("assert_return", 5), ("assert_trap", 58)],
    },
    Suite {
        name: "return.wast",
        source: include_str!("../testdata/return.wast"),
        directives: &[("module", 1), ("assert_return", 63), ("assert_invalid", 20)],
    },
    Suite {
        name: "forward.wast",
        source: include_str!("../testdata/forward.wast"),
        directives: &[("module", 1), ("assert_return", 4)],
    },
    Suite {
        name: "call.wast",
        source: include_str!("../testdata/call.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 69),
            ("assert_invalid", 18),
            ("assert_trap", 1),
            ("assert_exhaustion", 2),
        ],
    },
    Suite {
        name: "br_if.wast",
        source: include_str!("../testdata/br_if.wast"),
        directives: &[("module", 1), ("assert_return", 88), ("assert_invalid", 30)],
    },
    Suite {
        name: "labels.wast",
        source: include_str!("../testdata/labels.wast"),
        directives: &[("module", 1), ("assert_return", 25), ("assert_invalid", 3)],
    },
    Suite {
        name: "stack.wast",
        source: include_str!("../testdata/stack.wast"),
        directives: &[("module", 2), ("assert_return", 5)],
    },
    Suite {
        name: "switch.wast",
        source: include_str!("../testdata/switch.wast"),
        directives: &[("module", 1), ("assert_return", 26), ("assert_invalid", 1)],
    },
    Suite {
        name: "int_exprs.wast",
        source: include_str!("../testdata/int_exprs.wast"),
        directives: &[("module", 19), ("assert_return", 75), ("assert_trap", 14)],
    },
    Suite {
        name: "int_literals.wast",
        source: include_str!("../testdata/int_literals.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 30),
            ("assert_malformed", 20),
        ],
    },
    Suite {
        name: "float_exprs.wast",
        source: include_str!("../testdata/float_exprs.wast"),
        directives: &[("module", 98), ("assert_return", 819), ("invoke", 10)],
    },
    Suite {
        name: "float_literals.wast",
        source: include_str!("../testdata/float_literals.wast"),
        directives: &[
            ("module", 2),
            ("assert_return", 99),
            ("assert_malformed", 78),
        ],
    },
    Suite {
        name: "float_misc.wast",
        source: include_str!("../testdata/float_misc.wast"),
        directives: &[("module", 1), ("assert_return", 470)],
    },
    Suite {
        name: "traps.wast",
        source: include_str!("../testdata/traps.wast"),
        directives: &[("module", 4), ("assert_trap", 32)],
    },
    Suite {
        name: "comments.wast",
        source: include_str!("../testdata/comments.wast"),
        directives: &[("module", 5), ("assert_return", 3)],
    },
    // The memory family. Nothing here is blocked on an unimplemented proposal:
    // the loads, stores, `memory.size`/`memory.grow`, active data segments, and
    // the byte-order and trap rules are all MVP and all implemented on both
    // backends, so these files were simply untested against upstream.
    Suite {
        name: "address.wast",
        source: include_str!("../testdata/address.wast"),
        directives: &[
            ("module", 4),
            ("assert_return", 206),
            ("assert_trap", 49),
            ("assert_invalid", 1),
        ],
    },
    Suite {
        name: "align.wast",
        source: include_str!("../testdata/align.wast"),
        directives: &[
            ("module", 25),
            ("assert_return", 47),
            ("assert_trap", 1),
            ("assert_invalid", 44),
            ("assert_malformed", 48),
        ],
    },
    Suite {
        name: "endianness.wast",
        source: include_str!("../testdata/endianness.wast"),
        directives: &[("module", 1), ("assert_return", 68)],
    },
    Suite {
        name: "load.wast",
        source: include_str!("../testdata/load.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 37),
            ("assert_invalid", 46),
            ("assert_malformed", 13),
        ],
    },
    Suite {
        name: "store.wast",
        source: include_str!("../testdata/store.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 9),
            ("assert_invalid", 51),
            ("assert_malformed", 7),
        ],
    },
    Suite {
        name: "memory.wast",
        source: include_str!("../testdata/memory.wast"),
        directives: &[
            ("module", 11),
            ("assert_return", 53),
            ("assert_invalid", 22),
            ("assert_malformed", 3),
            ("module_definition", 1),
        ],
    },
    Suite {
        name: "memory_size.wast",
        source: include_str!("../testdata/memory_size.wast"),
        directives: &[("module", 4), ("assert_return", 36), ("assert_invalid", 2)],
    },
    Suite {
        name: "memory_trap.wast",
        source: include_str!("../testdata/memory_trap.wast"),
        directives: &[("module", 2), ("assert_return", 10), ("assert_trap", 170)],
    },
    Suite {
        name: "memory_redundancy.wast",
        source: include_str!("../testdata/memory_redundancy.wast"),
        directives: &[("module", 1), ("assert_return", 4), ("invoke", 3)],
    },
    // `memory.grow` plus a memory grown across three instances and re-imported
    // twice, which is what the shared-memory work in the baseline exists for.
    Suite {
        name: "memory_grow.wast",
        source: include_str!("../testdata/memory_grow.wast"),
        directives: &[
            ("module", 8),
            ("assert_return", 80),
            ("assert_trap", 7),
            ("assert_invalid", 9),
            ("register", 2),
        ],
    },
    // The comparison and bitwise float operators, which between them are the
    // bulk of the remaining MVP surface: every `f32`/`f64` relation and every
    // bit-level operator, checked over their full edge-case sets.
    Suite {
        name: "f32_bitwise.wast",
        source: include_str!("../testdata/f32_bitwise.wast"),
        directives: &[("module", 1), ("assert_return", 360), ("assert_invalid", 3)],
    },
    Suite {
        name: "f32_cmp.wast",
        source: include_str!("../testdata/f32_cmp.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 2400),
            ("assert_invalid", 6),
        ],
    },
    Suite {
        name: "f64_bitwise.wast",
        source: include_str!("../testdata/f64_bitwise.wast"),
        directives: &[("module", 1), ("assert_return", 360), ("assert_invalid", 3)],
    },
    Suite {
        name: "f64_cmp.wast",
        source: include_str!("../testdata/f64_cmp.wast"),
        directives: &[
            ("module", 1),
            ("assert_return", 2400),
            ("assert_invalid", 6),
        ],
    },
    // Float loads and stores over the memory, including the NaN payloads that
    // have to survive a round trip through memory and the signalling-NaN
    // canonicalization the specification requires on store.
    Suite {
        name: "float_memory.wast",
        source: include_str!("../testdata/float_memory.wast"),
        directives: &[("module", 6), ("assert_return", 60), ("invoke", 24)],
    },
    // A module that reaches a type it does not define, and so cannot decode.
    Suite {
        name: "type.wast",
        source: include_str!("../testdata/type.wast"),
        directives: &[("module", 1), ("assert_malformed", 2)],
    },
    // Custom sections: their placement, their contents, and that a module with
    // one decodes and runs unchanged.
    Suite {
        name: "custom.wast",
        source: include_str!("../testdata/custom.wast"),
        directives: &[("module", 3), ("assert_malformed", 8)],
    },
    // The `(module ...)` abbreviation of a bare module, which this harness
    // reaches through the `wast` crate's encoder rather than its own parser.
    Suite {
        name: "inline-module.wast",
        source: include_str!("../testdata/inline-module.wast"),
        directives: &[("module", 1)],
    },
    // Instructions that are unreachable but still have to *validate*: the
    // validator's dead-code rules are the whole content of this file, and it
    // has no module and no invocation at all.
    Suite {
        name: "unreached-invalid.wast",
        source: include_str!("../testdata/unreached-invalid.wast"),
        directives: &[("assert_invalid", 121)],
    },
    // The start function: run at instantiation, before any export is callable,
    // and the `spectest` imports plus `(assert_trap (module ... (start ...)))`
    // that make this file the reason the harness installs `spectest` and
    // instantiates a module written inline inside a directive.
    Suite {
        name: "start.wast",
        source: include_str!("../testdata/start.wast"),
        directives: &[
            ("module", 5),
            ("invoke", 4),
            ("assert_return", 6),
            ("assert_trap", 1),
            ("assert_invalid", 3),
            ("assert_malformed", 1),
        ],
    },
    // `ref.func` and `call_indirect` over them, with the `spectest` print
    // imports that a module importing a host function has to link against.
    Suite {
        name: "func_ptrs.wast",
        source: include_str!("../testdata/func_ptrs.wast"),
        directives: &[
            ("module", 3),
            ("invoke", 1),
            ("assert_return", 19),
            ("assert_trap", 6),
            ("assert_invalid", 7),
        ],
    },
];

#[test]
fn core_spec_suite() {
    assert_suite_passes(EngineMode::Micro);
}

/// The same directives, on the compiled backend.
///
/// Every module is lowered to the TPT IR and code-generated into the portable
/// baseline at instantiation, so this drives the whole `validate` -> `lower` ->
/// `verify` -> `BaselineModule::lower` -> block-graph-executor path with upstream's
/// own expected values and traps, rather than with fixtures written next to the
/// code under test. The counts are re-asserted here so a backend that drops a
/// directive fails rather than quietly shrinking its coverage.
#[test]
fn core_spec_suite_on_baseline() {
    assert_suite_passes(EngineMode::Baseline);
}

fn assert_suite_passes(mode: EngineMode) {
    let mut failures: Vec<String> = Vec::new();
    let mut miscounted: Vec<String> = Vec::new();

    for suite in SUITES {
        let cases = run_core_suite_with(suite.source, mode)
            .unwrap_or_else(|error| panic!("{}: could not parse: {error}", suite.name));

        let mut actual_counts: HashMap<&str, usize> = HashMap::new();
        for case in &cases {
            *actual_counts.entry(case.directive).or_insert(0) += 1;
            if let CoreOutcome::Failed(reason) = &case.outcome {
                failures.push(format!("{} line {}: {reason}", suite.name, case.line));
            }
        }

        let expected_total: usize = suite.directives.iter().map(|(_, count)| count).sum();
        if cases.len() != expected_total {
            miscounted.push(format!(
                "{}: expected {expected_total} directive(s) total, found {}",
                suite.name,
                cases.len()
            ));
        }
        for (directive, expected) in suite.directives {
            let actual = actual_counts.get(directive).copied().unwrap_or(0);
            if actual != *expected {
                miscounted.push(format!(
                    "{}: expected {expected} `{directive}` directive(s), found {actual}",
                    suite.name
                ));
            }
        }
        for (directive, actual) in &actual_counts {
            if !suite.directives.iter().any(|(name, _)| name == directive) {
                miscounted.push(format!(
                    "{}: found {actual} `{directive}` directive(s), none expected",
                    suite.name
                ));
            }
        }
    }

    assert!(
        miscounted.is_empty(),
        "{mode:?}: directive counts changed, so coverage changed without a failure:\n{}",
        miscounted.join("\n")
    );
    assert!(
        failures.is_empty(),
        "{mode:?}: {} directive(s) did not run the way upstream requires:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
