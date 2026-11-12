// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! `memory.size` is compiled: it reads the memory's length, which cannot change during a native call,
//! so it is the size in pages on every call. `memory.grow` is still refused, since it would change the
//! memory during the call.
#![cfg_attr(
    not(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )),
    allow(dead_code, unused_imports)
)]

use tpt_wasm_format::{Export, ExportDesc, Function, Memory, Module};
use tpt_wasm_jit::{compile, JitError};
use tpt_wasm_types::{FunctionType, Limits, MemoryType, ResultType, ValueType};

const PAGE: usize = 65_536;

/// A module with one page of memory and one `() -> i32` function with the given body.
fn module(body: Vec<u8>) -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            body,
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits { min: 1, max: None },
                memory64: false,
            },
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

fn lower(module: &Module) -> tpt_wasm_ir::IrModule {
    let validated = tpt_wasm_validate::validate(module.clone()).expect("the module validates");
    tpt_wasm_ir::lower_and_verify(&validated)
        .expect("the module lowers")
        .module()
        .clone()
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
#[test]
fn memory_size_is_the_page_count_of_the_memory_the_call_is_given() {
    // memory.size; end
    let ir = lower(&module(vec![0x3F, 0x00, 0x0B]));
    let code = compile(&ir.functions[0]).expect("memory.size is compiled");
    for pages in [1usize, 2, 3, 5] {
        let mut memory = vec![0u8; pages * PAGE];
        let raw = code
            .call_raw(&[], Some(&mut memory))
            .expect("runs without a trap");
        assert_eq!(raw as u32, pages as u32, "a memory of {pages} pages");
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
#[test]
fn memory_grow_is_still_refused() {
    // i32.const 1; memory.grow; end
    let ir = lower(&module(vec![0x41, 0x01, 0x40, 0x00, 0x0B]));
    assert!(
        matches!(compile(&ir.functions[0]), Err(JitError::Unsupported(_))),
        "a grow changes the memory during the call, so it stays on the baseline"
    );
}
