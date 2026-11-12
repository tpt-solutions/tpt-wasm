// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Times the three engines on two kernels: a recursive `fib`, which exercises calls, and a loop,
//! which exercises straight-line integer code. Each engine runs the same module, and the results are
//! checked against each other, so a timing is never reported for a wrong answer.
//!
//! Run with `cargo run --release --example native_bench --features jit`.

use std::time::{Duration, Instant};

use tpt_wasm_format::{Export, ExportDesc, Function, LocalDecl, Module};
use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

const RUNS: usize = 5;

/// The module's one function, `f(n: i32) -> i32`, with the given locals and body.
fn module(locals: Vec<LocalDecl>, body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals,
            body,
        }],
        exports: vec![Export {
            name: "f".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

/// `fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2)`: about 2·fib(n) calls.
fn fib() -> Module {
    module(
        Vec::new(),
        vec![
            0x20, 0x00, 0x41, 0x02, 0x48, 0x04, 0x7f, 0x20, 0x00, 0x05, 0x20, 0x00, 0x41, 0x01,
            0x6b, 0x10, 0x00, 0x20, 0x00, 0x41, 0x02, 0x6b, 0x10, 0x00, 0x6a, 0x0b, 0x0b,
        ],
    )
}

/// `sum(n) = Σ i·i` for `i` below `n`, with `i` and the accumulator as locals 1 and 2.
fn sum_of_squares() -> Module {
    module(
        vec![LocalDecl {
            count: 2,
            value_type: ValueType::I32,
        }],
        vec![
            0x02, 0x40, // block
            0x03, 0x40, // loop
            0x20, 0x01, 0x20, 0x00, 0x4e, // i >= n
            0x0d, 0x01, // br_if 1: leave the block
            0x20, 0x02, // acc
            0x20, 0x01, 0x20, 0x01, 0x6c, 0x6a, // acc + i * i
            0x21, 0x02, // acc = ...
            0x20, 0x01, 0x41, 0x01, 0x6a, 0x21, 0x01, // i = i + 1
            0x0c, 0x00, // br 0: the next iteration
            0x0b, // end loop
            0x0b, // end block
            0x20, 0x02, // acc
            0x0b,
        ],
    )
}

fn time(engine: &Engine, module: &Module, argument: i32) -> (Duration, i32) {
    let mut instance = engine.instantiate(module.clone()).expect("instantiates");
    let mut durations = Vec::new();
    let mut result = 0;
    for _ in 0..RUNS {
        let start = Instant::now();
        let values = instance
            .call("f", vec![Value::I32(argument)])
            .expect("runs");
        durations.push(start.elapsed());
        result = match values.as_slice() {
            [Value::I32(value)] => *value,
            other => panic!("expected one i32, got {other:?}"),
        };
    }
    durations.sort();
    (durations[RUNS / 2], result)
}

fn engine(mode: EngineMode) -> Engine {
    Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine")
}

fn main() {
    let kernels = [
        ("fib(25) recursive", fib(), 25),
        ("sum of squares, n = 1,000,000", sum_of_squares(), 1_000_000),
    ];
    for (name, module, argument) in kernels {
        println!("{name}");
        let mut expected = None;
        for (label, mode) in [
            ("micro", EngineMode::Micro),
            ("baseline", EngineMode::Baseline),
            ("native (jit)", EngineMode::Jit),
        ] {
            let (median, result) = time(&engine(mode), &module, argument);
            match expected {
                None => expected = Some(result),
                Some(value) => assert_eq!(result, value, "{label} disagrees with micro"),
            }
            println!(
                "  {label:<14} {:>10.3} ms   result {result}",
                median.as_secs_f64() * 1000.0
            );
        }
    }
}
