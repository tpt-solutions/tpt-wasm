// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Source locations from line tables that a real toolchain wrote, not hand-built ones.
//!
//! The fixtures under `tests/fixtures/dwarf` were compiled from `add.c` with clang 22 for
//! `wasm32` at `-O0 -g`, once with DWARF 4 and once with DWARF 5. `add` is the function on lines
//! 1 to 4 and `twice` the one on lines 6 to 8, which calls `add` on line 7.

use tpt_wasm_runtime::{Config, DebugSession, Engine, EngineMode};
use tpt_wasm_types::Value;

fn micro() -> Engine {
    Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("micro is accepted")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/fixtures/dwarf/{name}")).expect("the fixture is present")
}

fn line(session: &DebugSession<'_>, frame: usize) -> Option<u64> {
    session
        .source_location(frame)
        .expect("the line table reads")
        .map(|location| {
            assert!(
                location.file.ends_with("add.c"),
                "the file is add.c, not {}",
                location.file
            );
            location.line
        })
}

fn check_version(name: &str) {
    let engine = micro();
    let mut instance = engine
        .instantiate_bytes(&fixture(name))
        .expect("the fixture instantiates");
    let mut session = instance
        .debug("twice", vec![Value::I32(3)])
        .expect("a session");

    assert_eq!(
        line(&session, 0),
        Some(6),
        "{name}: the first instruction of twice is on its opening line"
    );

    // Step until the call to `add` has pushed its frame.
    let mut steps = 0;
    while session.stack().len() < 2 {
        session.step().expect("steps");
        steps += 1;
        assert!(steps < 100, "{name}: never entered add");
    }
    let inside_add = line(&session, 0).expect("add has line information");
    assert!(
        (1..=3).contains(&inside_add),
        "{name}: add runs on lines 1 to 3, got {inside_add}"
    );
    assert_eq!(
        line(&session, 1),
        Some(7),
        "{name}: the caller is on the line of the call"
    );
}

#[test]
fn dwarf_4_from_clang_maps_each_frame_to_its_line() {
    check_version("add-v4.wasm");
}

#[test]
fn dwarf_5_from_clang_maps_each_frame_to_its_line() {
    check_version("add-v5.wasm");
}
