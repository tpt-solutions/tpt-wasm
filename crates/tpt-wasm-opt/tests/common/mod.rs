// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Shared helpers for the translation-validation tests against the Lean model.
//!
//! Each test generates bodies, runs them on the Micro engine for their result, writes Lean
//! cases that evaluate the same bodies in the model, and asks the Lean checker for one verdict
//! per case. This module holds what those tests have in common.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

use tpt_wasm_format::{Export, ExportDesc, Function, Module};
use tpt_wasm_runtime::{Config, Engine};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

/// SplitMix64, with a fixed sequence for a fixed seed.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Append `i32.const value` with its signed LEB128 immediate.
pub fn push_i32_constant(bytes: &mut Vec<u8>, value: i32) {
    bytes.push(0x41);
    push_signed_leb(bytes, i64::from(value));
}

/// Append `i64.const value` with its signed LEB128 immediate.
pub fn push_i64_constant(bytes: &mut Vec<u8>, value: i64) {
    bytes.push(0x42);
    push_signed_leb(bytes, value);
}

fn push_signed_leb(bytes: &mut Vec<u8>, value: i64) {
    let mut remaining = value;
    loop {
        let byte = remaining as u8 & 0x7f;
        remaining >>= 7;
        let sign = byte & 0x40 != 0;
        if (remaining == 0 && !sign) || (remaining == -1 && sign) {
            bytes.push(byte);
            return;
        }
        bytes.push(byte | 0x80);
    }
}

/// The module that runs `body` as its exported `run` function, which returns one `i32`.
pub fn module_of(body: &[u8]) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            body: body.to_vec(),
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

/// The `i32` the Micro engine returns for a body that leaves one `i32` on the stack.
pub fn micro_result(body: &[u8]) -> i32 {
    let engine = Engine::new(Config::default()).expect("the engine is constructible");
    let mut instance = engine
        .instantiate(module_of(body))
        .expect("a generated body validates");
    match instance
        .call("run", Vec::new())
        .expect("the body returns")
        .as_slice()
    {
        [Value::I32(value)] => *value,
        other => panic!("the body returned {other:?}, not one i32"),
    }
}

/// Whether the translation tests are switched on. They need Lean, so they are opt-in.
pub fn enabled(name: &str) -> bool {
    if std::env::var_os("TPT_TRANSLATION_VALIDATION").is_none() {
        eprintln!("{name} skipped: set TPT_TRANSLATION_VALIDATION to run it");
        return false;
    }
    true
}

/// Whether the mutant mode is on: the expectations are corrupted, and the test passes only if
/// the checker rejects them.
pub fn mutant() -> bool {
    std::env::var_os("TPT_TRANSLATION_MUTANT").is_some()
}

/// Write `source` as a Lean file and run the checker on it, returning one verdict line per
/// `#eval` in the file, in order. Fails if the checker itself fails.
pub fn run_lean_checker(file_name: &str, source: &str) -> Vec<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let formal = root.join("formal");
    let out_dir = root.join("target").join("translation-validation");
    std::fs::create_dir_all(&out_dir).expect("create the output directory");
    let file = out_dir.join(file_name);
    std::fs::write(&file, source).expect("write the Lean cases");

    let output = Command::new("lake")
        .arg("env")
        .arg("lean")
        .arg(&file)
        .current_dir(&formal)
        .output()
        .expect("run lake env lean");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the Lean checker failed:\n{stdout}\n{stderr}"
    );
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}
