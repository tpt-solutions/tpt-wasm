// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The embedder's host stack against the call-depth limit. A configuration that declares a stack
//! too small for its depth is refused at engine creation, and one that fits is accepted.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};

fn configure(mode: EngineMode, stack: Option<usize>) -> Result<Engine, RuntimeError> {
    Engine::new(Config {
        engine_mode: mode,
        host_stack_bytes: stack,
        ..Config::default()
    })
}

#[test]
fn a_recursive_tier_on_a_stack_too_small_for_its_depth_is_refused() {
    let one_mebibyte = 1 << 20;
    for mode in [EngineMode::Baseline, EngineMode::Optimizing] {
        match configure(mode, Some(one_mebibyte)) {
            Err(RuntimeError::Refused(message)) => assert!(
                message.contains("host stack of 1048576 bytes"),
                "{mode:?}: the refusal names the stack: {message}"
            ),
            other => panic!("{mode:?}: expected a refusal, got {:?}", other.err()),
        }
    }
}

#[test]
fn a_stack_that_holds_the_depth_is_accepted() {
    let eight_mebibytes = 8 << 20;
    assert!(configure(EngineMode::Baseline, Some(eight_mebibytes)).is_ok());
}

#[test]
fn the_interpreter_keeps_its_frames_on_the_heap_so_any_stack_is_accepted() {
    assert!(configure(EngineMode::Micro, Some(64 * 1024)).is_ok());
}

#[test]
fn without_a_declared_stack_nothing_is_checked() {
    assert!(configure(EngineMode::Baseline, None).is_ok());
}
