// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The verifier's work budget. A module whose verification would exceed the budget is refused by the
//! compiled tiers, with the budget named, and the interpreter, which does not verify, still runs it.

use tpt_wasm_runtime::{Config, Engine, EngineMode, RuntimeError};
use tpt_wasm_types::Value;

fn leb(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn section(id: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(leb(content.len() as u32));
    out.extend_from_slice(content);
    out
}

/// `f() -> i32`, which nests `depth` blocks that each carry an i32, with `i32.const 0` in the
/// innermost one. The module is valid, and its control flow is `depth` levels deep, which is the
/// shape that makes dominance work grow.
fn nested_blocks(depth: usize) -> Vec<u8> {
    let mut body = vec![0x00]; // no local declarations
    for _ in 0..depth {
        body.extend_from_slice(&[0x02, 0x7f]); // block (result i32)
    }
    body.extend_from_slice(&[0x41, 0x00]); // i32.const 0
    body.extend(std::iter::repeat_n(0x0b, depth)); // end of each block
    body.push(0x0b); // end of the function

    let mut code = leb(1);
    code.extend(leb(body.len() as u32));
    code.extend(body);

    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend(section(1, &[1, 0x60, 0, 1, 0x7f])); // () -> i32
    module.extend(section(3, &[1, 0])); // one function of type 0
    module.extend(section(7, &[1, 1, b'f', 0x00, 0])); // export "f"
    module.extend(section(10, &code));
    module
}

fn engine(mode: EngineMode, work: u64) -> Engine {
    Engine::new(Config {
        engine_mode: mode,
        max_verification_work: work,
        ..Config::default()
    })
    .expect("engine")
}

#[test]
fn a_module_over_the_budget_is_refused_by_the_compiled_tier_with_the_budget_named() {
    let module = nested_blocks(2000);
    let error = engine(EngineMode::Baseline, 1_000)
        .instantiate_bytes(&module)
        .expect_err("the verifier's budget is smaller than the module's work");
    match error {
        RuntimeError::Refused(message) => assert!(
            message.contains("WorkBudgetExceeded") && message.contains("1000"),
            "the refusal names the budget: {message}"
        ),
        other => panic!("expected a refusal naming the budget, got {other:?}"),
    }
}

#[test]
fn the_default_budget_accepts_the_same_module_and_it_runs() {
    let module = nested_blocks(2000);
    let mut instance = engine(
        EngineMode::Baseline,
        Config::default().max_verification_work,
    )
    .instantiate_bytes(&module)
    .expect("within the default budget");
    assert_eq!(
        instance.call("f", Vec::new()).expect("runs"),
        vec![Value::I32(0)]
    );
}

#[test]
fn the_interpreter_does_not_verify_so_the_budget_does_not_apply_to_it() {
    let module = nested_blocks(2000);
    let mut instance = engine(EngineMode::Micro, 1_000)
        .instantiate_bytes(&module)
        .expect("the interpreter does not run the verifier");
    assert_eq!(
        instance.call("f", Vec::new()).expect("runs"),
        vec![Value::I32(0)]
    );
}
