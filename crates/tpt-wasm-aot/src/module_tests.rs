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
use tpt_wasm_jit::{HostCalls, LinearMemory};
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
fn a_sealed_module_artifact_opens_and_runs_as_the_unsealed_one_does() {
    // The seal is transparent: `open` returns the artifact exactly as sealed, so the same
    // parser accepts it and the loaded code gives the same answers.
    let key = b"0123456789abcdef0123456789abcdef";
    let bytes = compile_module_artifact(&lowered(dispatch_module()), 1000, true)
        .expect("compiles")
        .to_bytes();
    let sealed = crate::seal::seal(&bytes, key).expect("sealed");
    let opened = crate::seal::open(&sealed, key).expect("opens under its key");
    assert_eq!(opened, bytes.as_slice());
    let artifact = ModuleArtifact::from_bytes(opened).expect("parses");
    assert_eq!(artifact.to_bytes(), bytes);
}

#[test]
fn a_flipped_byte_in_a_sealed_module_artifact_is_refused_before_it_is_parsed() {
    let key = b"0123456789abcdef0123456789abcdef";
    let bytes = compile_module_artifact(&lowered(dispatch_module()), 1000, true)
        .expect("compiles")
        .to_bytes();
    let sealed = crate::seal::seal(&bytes, key).expect("sealed");
    for position in (0..sealed.len()).step_by(11) {
        let mut damaged = sealed.clone();
        damaged[position] ^= 0x40;
        assert!(
            crate::seal::open(&damaged, key).is_err(),
            "a flipped byte at {position} of the sealed artifact is refused"
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
    assert_eq!(artifact.result(), Some(ValueType::F64));
    let restored = Artifact::from_bytes(&artifact.to_bytes()).expect("reads back");
    assert_eq!(restored, artifact);
    let loaded = restored.load().expect("loads on this host");
    let raw = loaded
        .call_raw(&[2.5f64.to_bits() as i64], None)
        .expect("runs");
    assert_eq!(f64::from_bits(raw), -2.5);
}

#[test]
fn a_module_with_several_results_keeps_them_through_the_artifact() {
    // f0(a, b) = (a + b, a - b). f1(a) calls f0 with (a, 1) and returns both of its results.
    let i32s = ValueType::I32;
    let module = Module {
        types: vec![
            FunctionType {
                params: ResultType(vec![i32s, i32s]),
                results: ResultType(vec![i32s, i32s]),
            },
            FunctionType {
                params: ResultType(vec![i32s]),
                results: ResultType(vec![i32s, i32s]),
            },
        ],
        functions: vec![
            Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![
                    0x20, 0x00, 0x20, 0x01, 0x6A, 0x20, 0x00, 0x20, 0x01, 0x6B, 0x0B,
                ],
            },
            Function {
                type_index: 1,
                locals: Vec::new(),
                // local.get 0; i32.const 1; call 0; end
                body: vec![0x20, 0x00, 0x41, 0x01, 0x10, 0x00, 0x0B],
            },
        ],
        exports: vec![
            Export {
                name: "pair".into(),
                desc: ExportDesc::Function(0),
            },
            Export {
                name: "shifted".into(),
                desc: ExportDesc::Function(1),
            },
        ],
        ..Module::default()
    };
    let ir = lowered(module);
    let artifact = compile_module_artifact(&ir, 1000, true).expect("compiles");
    let bytes = artifact.to_bytes();
    let restored = ModuleArtifact::from_bytes(&bytes).expect("reads back");
    assert_eq!(
        restored.to_bytes(),
        bytes,
        "the artifact is stable across a round trip"
    );
    let loaded = restored.load().expect("loads on this host");
    assert!(
        loaded.is_compiled(0) && loaded.is_compiled(1),
        "both functions are native"
    );
    let pair = loaded
        .call_with_host_results(0, &[7, 3], None, &mut [], None)
        .expect("runs");
    assert_eq!(pair, vec![10, 4]);
    let shifted = loaded
        .call_with_host_results(1, &[7], None, &mut [], None)
        .expect("runs");
    assert_eq!(shifted, vec![8, 6]);
}

/// A module with one mutable `i32` global and `bump(x)`, which adds `x` to the global and returns
/// the new value. The global's state must survive from one call to the next, both in memory and
/// from the artifact's bytes.
fn global_module() -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            // global.get 0; local.get 0; i32.add; global.set 0; global.get 0
            body: vec![0x23, 0x00, 0x20, 0x00, 0x6a, 0x24, 0x00, 0x23, 0x00, 0x0b],
        }],
        globals: vec![tpt_wasm_format::Global {
            global_type: tpt_wasm_types::GlobalType {
                value_type: ValueType::I32,
                mutable: true,
            },
            init: tpt_wasm_format::ConstExpr(vec![0x41, 0x00, 0x0b]),
        }],
        exports: vec![Export {
            name: "bump".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

#[test]
fn a_global_keeps_its_value_across_calls_from_the_artifact_as_in_memory() {
    let ir = lowered(global_module());
    let artifact = compile_module_artifact(&ir, 1000, false).expect("compiles");
    let bytes = artifact.to_bytes();
    let loaded = ModuleArtifact::from_bytes(&bytes)
        .expect("reads back")
        .load()
        .expect("loads on this host");
    let in_memory = compile_module(&ir, 1000, false);

    let mut stored_globals = vec![0u64];
    let mut memory_globals = vec![0u64];
    for x in [5i64, 2, -7] {
        let stored = loaded
            .call_with_globals(0, &[x], None, &mut stored_globals)
            .expect("runs from its bytes");
        let direct = in_memory
            .call_with_globals(0, &[x], None, &mut memory_globals)
            .expect("runs in memory");
        assert_eq!(stored, direct, "bump({x})");
        assert_eq!(stored_globals, memory_globals, "the global after bump({x})");
    }
    assert_eq!(stored_globals[0] as u32 as i32, 0, "5 + 2 - 7 leaves zero");
}

/// A module with one memory of at most 4 pages and `grow(n)`, which grows the memory by `n` pages
/// and returns the old size, or -1 when it cannot.
fn grow_module() -> Module {
    Module {
        types: vec![FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            // local.get 0; memory.grow 0
            body: vec![0x20, 0x00, 0x40, 0x00, 0x0b],
        }],
        memories: vec![tpt_wasm_format::Memory {
            memory_type: tpt_wasm_types::MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(4),
                },
                memory64: false,
            },
        }],
        exports: vec![Export {
            name: "grow".into(),
            desc: ExportDesc::Function(0),
        }],
        ..Module::default()
    }
}

#[test]
fn a_module_that_grows_its_memory_keeps_the_flag_and_grows_as_in_memory() {
    let ir = lowered(grow_module());
    let artifact = compile_module_artifact(&ir, 1000, false).expect("compiles");
    let loaded = ModuleArtifact::from_bytes(&artifact.to_bytes())
        .expect("reads back")
        .load()
        .expect("loads on this host");
    assert!(
        loaded.memory_grows(),
        "the growth flag survives the artifact, so the caller reserves the memory"
    );
    let in_memory = compile_module(&ir, 1000, false);

    // Each case starts from a fresh one-page memory: growing by 1 or 2 succeeds and returns the old
    // size, and growing by 4 would pass the maximum of 4 pages, so it fails with -1.
    for (pages, expected, expected_len) in [(1i64, 1, 2 * 65536), (2, 1, 3 * 65536), (4, -1, 65536)]
    {
        // Reserve the maximum, so compiled code grows into bytes already allocated.
        let mut stored_bytes = vec![0u8; 4 * 65536];
        let mut stored = LinearMemory {
            bytes: &mut stored_bytes,
            len: 65536,
        };
        let mut memory_bytes = vec![0u8; 4 * 65536];
        let mut direct = LinearMemory {
            bytes: &mut memory_bytes,
            len: 65536,
        };
        let from_bytes = loaded
            .call_with_memory(0, &[pages], Some(&mut stored), &mut [])
            .expect("grows from its bytes");
        let from_memory = in_memory
            .call_with_memory(0, &[pages], Some(&mut direct), &mut [])
            .expect("grows in memory");
        assert_eq!(from_bytes, from_memory, "grow({pages})");
        assert_eq!(from_bytes as u32 as i32, expected, "grow({pages}) result");
        assert_eq!(stored.len, direct.len, "the size after grow({pages})");
        assert_eq!(
            stored.len, expected_len,
            "the size after grow({pages}) from one page"
        );
    }
}

/// Supplies one import, `add1`, which adds one to its argument and counts its calls.
struct AddOne {
    calls: u32,
}

impl HostCalls for AddOne {
    fn arity(&self, _import: u32) -> (usize, usize) {
        (1, 1)
    }

    fn call(
        &mut self,
        _import: u32,
        args: &[i64],
        results: &mut [i64],
        _memory: Option<&mut [u8]>,
    ) -> bool {
        self.calls += 1;
        results[0] = args[0] + 1;
        true
    }
}

/// A module that imports `env.add1` and defines `plus_two(x)`, which calls the import twice.
fn import_module() -> Module {
    let i32_to_i32 = FunctionType {
        params: ResultType(vec![ValueType::I32]),
        results: ResultType(vec![ValueType::I32]),
    };
    Module {
        types: vec![i32_to_i32],
        imports: vec![tpt_wasm_format::Import {
            module: "env".into(),
            name: "add1".into(),
            desc: tpt_wasm_format::ImportDesc::Function(0),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: Vec::new(),
            // local.get 0; call 0 (the import); call 0
            body: vec![0x20, 0x00, 0x10, 0x00, 0x10, 0x00, 0x0b],
        }],
        exports: vec![Export {
            name: "plus_two".into(),
            desc: ExportDesc::Function(1),
        }],
        ..Module::default()
    }
}

#[test]
fn a_module_with_an_import_keeps_its_host_calls_through_the_artifact() {
    let ir = lowered(import_module());
    let artifact = compile_module_artifact(&ir, 1000, false).expect("compiles");
    let loaded = ModuleArtifact::from_bytes(&artifact.to_bytes())
        .expect("reads back")
        .load()
        .expect("loads on this host");
    let in_memory = compile_module(&ir, 1000, false);

    let mut stored_host = AddOne { calls: 0 };
    let mut direct_host = AddOne { calls: 0 };
    let from_bytes = loaded
        .call_with_host_results(0, &[40], None, &mut [], Some(&mut stored_host))
        .expect("calls the import from its bytes");
    let from_memory = in_memory
        .call_with_host_results(0, &[40], None, &mut [], Some(&mut direct_host))
        .expect("calls the import in memory");
    assert_eq!(from_bytes, from_memory);
    assert_eq!(from_bytes, vec![42], "40 + 1 + 1");
    assert_eq!(stored_host.calls, 2, "the import is called twice");
    assert_eq!(stored_host.calls, direct_host.calls);
}
