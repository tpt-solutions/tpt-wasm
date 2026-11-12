// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Module artifacts: a module compiled with its calls and table must run from its stored bytes as
//! it runs in memory, and a damaged or inconsistent artifact must be refused.
#![cfg(any(
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

use super::*;
use tpt_wasm_format::{
    Element, ElementInit, ElementMode, Export, ExportDesc, Function, Module, Table,
};
use tpt_wasm_types::{FunctionType, Limits, RefType, ResultType, TableType};

/// `dispatch(x, y, sel)` calls the table's slot `sel` with `(x, y)`. The table has four slots: `add`
/// and `mul`, a null, and `neg`, whose type does not match the call.
fn dispatch_module() -> Module {
    let i32_type = |params: usize| FunctionType {
        params: ResultType(vec![ValueType::I32; params]),
        results: ResultType(vec![ValueType::I32]),
    };
    Module {
        types: vec![i32_type(2), i32_type(1), i32_type(3)],
        functions: vec![
            Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b], // add
            },
            Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x20, 0x00, 0x20, 0x01, 0x6c, 0x0b], // mul
            },
            Function {
                type_index: 1,
                locals: Vec::new(),
                body: vec![0x41, 0x00, 0x20, 0x00, 0x6b, 0x0b], // neg: 0 - x
            },
            Function {
                type_index: 2,
                locals: Vec::new(),
                // x, y, sel; call_indirect (type 0) (table 0)
                body: vec![0x20, 0x00, 0x20, 0x01, 0x20, 0x02, 0x11, 0x00, 0x00, 0x0b],
            },
        ],
        tables: vec![Table {
            table_type: TableType {
                element_type: RefType::FuncRef,
                limits: Limits { min: 4, max: None },
            },
            init: None,
        }],
        elements: vec![Element {
            element_type: RefType::FuncRef,
            mode: ElementMode::Active {
                table_index: 0,
                offset: tpt_wasm_format::ConstExpr(vec![0x41, 0x00, 0x0b]),
            },
            init: ElementInit::Expressions(vec![
                tpt_wasm_format::ConstExpr(vec![0xd2, 0x00, 0x0b]), // ref.func 0
                tpt_wasm_format::ConstExpr(vec![0xd2, 0x01, 0x0b]), // ref.func 1
                tpt_wasm_format::ConstExpr(vec![0xd0, 0x70, 0x0b]), // ref.null funcref
                tpt_wasm_format::ConstExpr(vec![0xd2, 0x02, 0x0b]), // ref.func 2
            ]),
        }],
        exports: vec![Export {
            name: "dispatch".into(),
            desc: ExportDesc::Function(3),
        }],
        ..Module::default()
    }
}

/// The module lowered to IR, as the runtime lowers it.
fn lowered(module: Module) -> IrModule {
    let validated = tpt_wasm_validate::validate(module).expect("the module validates");
    tpt_wasm_ir::lower_and_verify(&validated)
        .expect("the module lowers")
        .module()
        .clone()
}

/// The calls the dispatch test makes: each selector, with a few operand pairs.
fn dispatch_calls() -> Vec<[i64; 3]> {
    let mut calls = Vec::new();
    for sel in [-1i64, 0, 1, 2, 3, 4, 100] {
        for (x, y) in [(7, 3), (-4, 5), (0, 0)] {
            calls.push([x, y, sel]);
        }
    }
    calls
}

#[test]
fn a_module_with_indirect_calls_runs_from_its_bytes_as_it_does_in_memory() {
    let ir = lowered(dispatch_module());
    let artifact = compile_module_artifact(&ir, 1000, true).expect("compiles");
    assert!(
        artifact.indirect(),
        "the table is private, so its calls are compiled"
    );
    let bytes = artifact.to_bytes();
    let restored = ModuleArtifact::from_bytes(&bytes).expect("reads back");
    assert_eq!(restored, artifact);
    let loaded = restored.load().expect("loads on this host");
    let in_memory = compile_module(&ir, 1000, true);
    let mut outcomes = Vec::new();
    for call in dispatch_calls() {
        let stored = loaded.call(3, &call, None);
        assert_eq!(stored, in_memory.call(3, &call, None), "call {call:?}");
        outcomes.push(stored);
    }
    assert!(outcomes.iter().any(Result::is_ok), "a call succeeds");
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome == &Err(JitError::Trap(tpt_wasm_types::Trap::NullReference))),
        "a call through the null slot traps"
    );
}

#[test]
fn every_truncation_of_a_module_artifact_is_refused() {
    let bytes = compile_module_artifact(&lowered(dispatch_module()), 1000, true)
        .expect("compiles")
        .to_bytes();
    for length in 0..bytes.len() {
        assert!(
            ModuleArtifact::from_bytes(&bytes[..length]).is_err(),
            "a prefix of {length} bytes is refused"
        );
    }
}

#[test]
fn a_flipped_byte_in_a_module_artifact_is_refused() {
    let bytes = compile_module_artifact(&lowered(dispatch_module()), 1000, true)
        .expect("compiles")
        .to_bytes();
    for position in (0..bytes.len()).step_by(7) {
        let mut damaged = bytes.clone();
        damaged[position] ^= 0x40;
        assert!(
            ModuleArtifact::from_bytes(&damaged).is_err(),
            "a flipped byte at {position} is refused"
        );
    }
}

#[test]
fn a_module_artifact_for_another_target_is_refused_at_load() {
    let mut artifact =
        compile_module_artifact(&lowered(dispatch_module()), 1000, true).expect("compiles");
    artifact.target = "elsewhere".into();
    assert!(matches!(
        artifact.load(),
        Err(AotError::TargetMismatch { .. })
    ));
}

#[test]
fn a_table_naming_a_function_that_is_not_compiled_is_refused_for_indirect_calls() {
    let mut artifact =
        compile_module_artifact(&lowered(dispatch_module()), 1000, true).expect("compiles");
    // Slot 0 still names function 0, but the artifact no longer carries its code.
    artifact.parts.functions[0] = None;
    assert!(
        matches!(artifact.load(), Err(AotError::Load(JitError::Invalid(_)))),
        "an indirect call needs every slot to name a compiled function"
    );
}

#[test]
fn a_float_function_keeps_its_types_through_the_artifact() {
    // (f64) -> f64: f64.neg.
    let module = Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::F64]),
            results: ResultType(vec![ValueType::F64]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            body: vec![0x20, 0x00, 0x9a, 0x0b],
        }],
        exports: vec![Export {
            name: "neg".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    };
    let ir = lowered(module);
    let artifact = compile_artifact(&ir.functions[0]).expect("compiles");
    assert_eq!(artifact.params(), &[ValueType::F64]);
    assert_eq!(artifact.result(), ValueType::F64);
    let restored = Artifact::from_bytes(&artifact.to_bytes()).expect("reads back");
    assert_eq!(restored, artifact);
    let loaded = restored.load().expect("loads on this host");
    let raw = loaded
        .call_raw(&[2.5f64.to_bits() as i64], None)
        .expect("runs");
    assert_eq!(f64::from_bits(raw), -2.5);
}
