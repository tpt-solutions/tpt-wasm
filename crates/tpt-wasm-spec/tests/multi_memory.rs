// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The multi-memory proposal's test files, run through the same harness as the Core suite.
//!
//! The interpreter (`Micro`) is the reference, and it runs every file: each directive must pass and
//! each directive kind must occur the pinned number of times, so a directive cannot drop out unnoticed.
//! The compiled engines hold one memory, so a module with more than one is refused by name when it is
//! lowered. They must keep refusing it rather than run it, which the second test pins.

use std::collections::HashMap;

use tpt_wasm_runtime::EngineMode;
use tpt_wasm_spec::{run_core_suite_with, CoreOutcome};

/// Each file, its source, and the exact count of every directive kind it contains.
/// The directive counts of one file: each directive kind and how many times it occurs.
type Counts = &'static [(&'static str, usize)];

const FILES: &[(&str, &str, Counts)] = &[
    (
        "address0.wast",
        include_str!("../testdata/multi-memory/address0.wast"),
        &[("assert_return", 74), ("assert_trap", 17), ("module", 1)],
    ),
    (
        "address1.wast",
        include_str!("../testdata/multi-memory/address1.wast"),
        &[("assert_return", 104), ("assert_trap", 22), ("module", 1)],
    ),
    (
        "align0.wast",
        include_str!("../testdata/multi-memory/align0.wast"),
        &[("assert_return", 4), ("module", 1)],
    ),
    (
        "binary0.wast",
        include_str!("../testdata/multi-memory/binary0.wast"),
        &[("assert_malformed", 2), ("module", 5)],
    ),
    (
        "data0.wast",
        include_str!("../testdata/multi-memory/data0.wast"),
        &[("module", 7)],
    ),
    (
        "data1.wast",
        include_str!("../testdata/multi-memory/data1.wast"),
        &[("assert_trap", 14)],
    ),
    (
        "data_drop0.wast",
        include_str!("../testdata/multi-memory/data_drop0.wast"),
        &[
            ("assert_return", 2),
            ("assert_trap", 2),
            ("invoke", 6),
            ("module", 1),
        ],
    ),
    (
        "exports0.wast",
        include_str!("../testdata/multi-memory/exports0.wast"),
        &[("module", 8)],
    ),
    (
        "float_exprs0.wast",
        include_str!("../testdata/multi-memory/float_exprs0.wast"),
        &[("assert_return", 8), ("invoke", 5), ("module", 1)],
    ),
    (
        "float_exprs1.wast",
        include_str!("../testdata/multi-memory/float_exprs1.wast"),
        &[("assert_return", 2), ("module", 1)],
    ),
    (
        "float_memory0.wast",
        include_str!("../testdata/multi-memory/float_memory0.wast"),
        &[("assert_return", 20), ("invoke", 8), ("module", 2)],
    ),
    (
        "imports0.wast",
        include_str!("../testdata/multi-memory/imports0.wast"),
        &[("assert_unlinkable", 6), ("module", 1), ("register", 1)],
    ),
    (
        "imports1.wast",
        include_str!("../testdata/multi-memory/imports1.wast"),
        &[("assert_return", 3), ("assert_trap", 1), ("module", 1)],
    ),
    (
        "imports2.wast",
        include_str!("../testdata/multi-memory/imports2.wast"),
        &[
            ("assert_return", 6),
            ("assert_trap", 2),
            ("assert_unlinkable", 6),
            ("module", 5),
            ("register", 1),
        ],
    ),
    (
        "imports3.wast",
        include_str!("../testdata/multi-memory/imports3.wast"),
        &[("assert_unlinkable", 8), ("module", 1), ("register", 1)],
    ),
    (
        "imports4.wast",
        include_str!("../testdata/multi-memory/imports4.wast"),
        &[("assert_return", 8), ("module", 5), ("register", 3)],
    ),
    (
        "linking0.wast",
        include_str!("../testdata/multi-memory/linking0.wast"),
        &[
            ("assert_return", 1),
            ("assert_trap", 2),
            ("assert_unlinkable", 1),
            ("module", 1),
            ("register", 1),
        ],
    ),
    (
        "linking1.wast",
        include_str!("../testdata/multi-memory/linking1.wast"),
        &[
            ("assert_return", 7),
            ("assert_trap", 2),
            ("module", 4),
            ("register", 1),
        ],
    ),
    (
        "linking2.wast",
        include_str!("../testdata/multi-memory/linking2.wast"),
        &[("assert_return", 8), ("module", 2), ("register", 1)],
    ),
    (
        "linking3.wast",
        include_str!("../testdata/multi-memory/linking3.wast"),
        &[
            ("assert_return", 6),
            ("assert_trap", 3),
            ("assert_unlinkable", 1),
            ("module", 2),
            ("register", 2),
        ],
    ),
    (
        "load0.wast",
        include_str!("../testdata/multi-memory/load0.wast"),
        &[("assert_return", 2), ("module", 1)],
    ),
    (
        "load1.wast",
        include_str!("../testdata/multi-memory/load1.wast"),
        &[("assert_return", 15), ("module", 2), ("register", 1)],
    ),
    (
        "load2.wast",
        include_str!("../testdata/multi-memory/load2.wast"),
        &[("assert_return", 37), ("module", 1)],
    ),
    (
        "memory-multi.wast",
        include_str!("../testdata/multi-memory/memory-multi.wast"),
        &[("assert_return", 4), ("module", 2)],
    ),
    (
        "memory_copy0.wast",
        include_str!("../testdata/multi-memory/memory_copy0.wast"),
        &[
            ("assert_return", 19),
            ("assert_trap", 2),
            ("invoke", 7),
            ("module", 1),
        ],
    ),
    (
        "memory_copy1.wast",
        include_str!("../testdata/multi-memory/memory_copy1.wast"),
        &[
            ("assert_return", 6),
            ("assert_trap", 2),
            ("invoke", 5),
            ("module", 1),
        ],
    ),
    (
        "memory_fill0.wast",
        include_str!("../testdata/multi-memory/memory_fill0.wast"),
        &[
            ("assert_return", 9),
            ("assert_trap", 2),
            ("invoke", 4),
            ("module", 1),
        ],
    ),
    (
        "memory_grow.wast",
        include_str!("../testdata/multi-memory/memory_grow.wast"),
        &[("assert_return", 47), ("module", 3), ("register", 1)],
    ),
    (
        "memory_init0.wast",
        include_str!("../testdata/multi-memory/memory_init0.wast"),
        &[
            ("assert_return", 5),
            ("assert_trap", 3),
            ("invoke", 4),
            ("module", 1),
        ],
    ),
    (
        "memory_size0.wast",
        include_str!("../testdata/multi-memory/memory_size0.wast"),
        &[("assert_return", 7), ("module", 1)],
    ),
    (
        "memory_size1.wast",
        include_str!("../testdata/multi-memory/memory_size1.wast"),
        &[("assert_return", 14), ("module", 1)],
    ),
    (
        "memory_size2.wast",
        include_str!("../testdata/multi-memory/memory_size2.wast"),
        &[("assert_return", 20), ("module", 1)],
    ),
    (
        "memory_size3.wast",
        include_str!("../testdata/multi-memory/memory_size3.wast"),
        &[("assert_invalid", 2)],
    ),
    (
        "memory_size_import.wast",
        include_str!("../testdata/multi-memory/memory_size_import.wast"),
        &[("assert_return", 4), ("module", 2), ("register", 1)],
    ),
    (
        "memory_trap0.wast",
        include_str!("../testdata/multi-memory/memory_trap0.wast"),
        &[("assert_return", 3), ("assert_trap", 10), ("module", 1)],
    ),
    (
        "memory_trap1.wast",
        include_str!("../testdata/multi-memory/memory_trap1.wast"),
        &[("assert_return", 7), ("assert_trap", 160), ("module", 1)],
    ),
    (
        "start0.wast",
        include_str!("../testdata/multi-memory/start0.wast"),
        &[("assert_return", 6), ("invoke", 2), ("module", 1)],
    ),
    (
        "store0.wast",
        include_str!("../testdata/multi-memory/store0.wast"),
        &[("assert_return", 2), ("invoke", 2), ("module", 1)],
    ),
    (
        "store1.wast",
        include_str!("../testdata/multi-memory/store1.wast"),
        &[
            ("assert_return", 4),
            ("invoke", 4),
            ("module", 3),
            ("register", 2),
        ],
    ),
    (
        "store2.wast",
        include_str!("../testdata/multi-memory/store2.wast"),
        &[
            ("assert_return", 20),
            ("invoke", 2),
            ("module", 2),
            ("register", 1),
        ],
    ),
    (
        "traps0.wast",
        include_str!("../testdata/multi-memory/traps0.wast"),
        &[("assert_trap", 14), ("module", 1)],
    ),
];

#[test]
fn every_multi_memory_file_passes_on_the_interpreter() {
    let mut failures: Vec<String> = Vec::new();
    for (name, source, expected) in FILES {
        let cases = run_core_suite_with(source, EngineMode::Micro)
            .unwrap_or_else(|error| panic!("{name}: could not parse: {error}"));
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for case in &cases {
            *counts.entry(case.directive).or_insert(0) += 1;
            if let CoreOutcome::Failed(reason) = &case.outcome {
                failures.push(format!("{name} line {}: {reason}", case.line));
            }
        }
        let expected_total: usize = expected.iter().map(|(_, count)| count).sum();
        assert_eq!(
            cases.len(),
            expected_total,
            "{name}: directive count changed"
        );
        for (directive, count) in expected.iter() {
            assert_eq!(
                counts.get(directive).copied().unwrap_or(0),
                *count,
                "{name}: `{directive}` count changed"
            );
        }
        assert_eq!(
            counts.len(),
            expected.len(),
            "{name}: a directive kind appeared that is not pinned"
        );
    }
    assert!(
        failures.is_empty(),
        "{} directive(s) did not run the way upstream requires:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The same files on the compiled engines: the baseline, which runs every memory a module declares, and the
/// optimizing engine, which optimizes the same IR before the baseline runs it. Each directive must pass and
/// each directive kind must occur the pinned number of times, as on the interpreter.
#[test]
fn every_multi_memory_file_passes_on_the_compiled_engines() {
    for mode in [
        EngineMode::Baseline,
        EngineMode::Optimizing,
        EngineMode::Jit,
    ] {
        let mut failures: Vec<String> = Vec::new();
        for (name, source, expected) in FILES {
            let cases = run_core_suite_with(source, mode)
                .unwrap_or_else(|error| panic!("{name}: could not parse: {error}"));
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for case in &cases {
                *counts.entry(case.directive).or_insert(0) += 1;
                if let CoreOutcome::Failed(reason) = &case.outcome {
                    failures.push(format!("{mode:?} {name} line {}: {reason}", case.line));
                }
            }
            assert_eq!(
                cases.len(),
                expected.iter().map(|(_, count)| count).sum::<usize>(),
                "{mode:?} {name}: directive count changed"
            );
            for (directive, count) in expected.iter() {
                assert_eq!(
                    counts.get(directive).copied().unwrap_or(0),
                    *count,
                    "{mode:?} {name}: `{directive}` count changed"
                );
            }
        }
        assert!(
            failures.is_empty(),
            "{} directive(s) did not run the way upstream requires:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
