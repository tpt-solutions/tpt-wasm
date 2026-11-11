// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Tests for the Preview 1 core, run through the public engine on both backends.
//!
//! Each test builds a small module that imports some Preview 1 functions and calls them
//! in a fixed order, then checks the bytes the functions left in the module's memory,
//! the value `run` returned, and what the embedder's sinks received.

use std::io::{self, Cursor, Write};
use std::sync::{Arc, Mutex};

use super::{errno, Preview1, Preview1Config};
use tpt_wasm_format::{
    ConstExpr, DataMode, DataSegment, Export, ExportDesc, Function, Import, ImportDesc, Memory,
    Module,
};
use tpt_wasm_runtime::{Config, Engine, EngineMode, Instance, RuntimeError};
use tpt_wasm_types::{FunctionType, Limits, MemoryType, ResultType, Value, ValueType};

const BOTH: [EngineMode; 2] = [EngineMode::Micro, EngineMode::Baseline];

/// A write sink whose contents the test can read after the run.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    fn contents(&self) -> Vec<u8> {
        self.0.lock().expect("not poisoned").clone()
    }
}

impl Write for Sink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("not poisoned")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn signature(params: &[ValueType], results: &[ValueType]) -> FunctionType {
    FunctionType {
        params: ResultType(params.to_vec()),
        results: ResultType(results.to_vec()),
    }
}

fn leb_i32(mut value: i32, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn leb_u32(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// A body that calls each `(function, arguments)` in order. Every call but the last has
/// its result dropped, so the last call's result is what `run` returns.
fn body(calls: &[(u32, Vec<i32>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (position, (function, arguments)) in calls.iter().enumerate() {
        for argument in arguments {
            out.push(0x41);
            leb_i32(*argument, &mut out);
        }
        out.push(0x10);
        leb_u32(*function, &mut out);
        if position + 1 < calls.len() {
            out.push(0x1a);
        }
    }
    out.push(0x0b);
    out
}

/// A body for `clock_time_get(clock, precision, time_at)` calls. The precision is an
/// `i64`, so it is emitted as `i64.const 0`, which `body` cannot express.
fn clock_body(calls: &[(i32, i32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (position, (clock, time_at)) in calls.iter().enumerate() {
        out.push(0x41);
        leb_i32(*clock, &mut out);
        out.push(0x42);
        out.push(0x00);
        out.push(0x41);
        leb_i32(*time_at, &mut out);
        out.push(0x10);
        leb_u32(0, &mut out);
        if position + 1 < calls.len() {
            out.push(0x1a);
        }
    }
    out.push(0x0b);
    out
}

/// A module that imports `imports` from `wasi_snapshot_preview1`, defines `run` with the
/// given signature and body, exports `run` and `mem`, and places `data` in memory.
fn module(
    imports: &[(&str, FunctionType)],
    run: FunctionType,
    run_body: Vec<u8>,
    data: Vec<(u32, Vec<u8>)>,
) -> Module {
    let mut types: Vec<FunctionType> = imports.iter().map(|(_, sig)| sig.clone()).collect();
    types.push(run);
    let run_type = (types.len() - 1) as u32;
    let run_index = imports.len() as u32;
    Module {
        types,
        imports: imports
            .iter()
            .enumerate()
            .map(|(index, (name, _))| Import {
                module: crate::PREVIEW1_MODULE.into(),
                name: (*name).into(),
                desc: ImportDesc::Function(index as u32),
            })
            .collect(),
        functions: vec![Function {
            type_index: run_type,
            locals: Vec::new(),
            body: run_body,
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits { min: 1, max: None },
                memory64: false,
            },
        }],
        data: data
            .into_iter()
            .map(|(at, bytes)| DataSegment {
                mode: DataMode::Active {
                    memory_index: 0,
                    offset: const_i32(at as i32),
                },
                data: bytes,
            })
            .collect(),
        exports: vec![
            Export {
                name: "run".into(),
                desc: ExportDesc::Function(run_index),
            },
            Export {
                name: "mem".into(),
                desc: ExportDesc::Memory(0),
            },
        ],
        ..Module::default()
    }
}

fn const_i32(value: i32) -> ConstExpr {
    let mut bytes = vec![0x41];
    leb_i32(value, &mut bytes);
    bytes.push(0x0b);
    ConstExpr(bytes)
}

/// The Preview 1 signatures the tests need, written out as the ABI gives them.
fn fd_write_signature() -> FunctionType {
    use ValueType::I32;
    signature(&[I32, I32, I32, I32], &[I32])
}

fn two_pointer_signature() -> FunctionType {
    use ValueType::I32;
    signature(&[I32, I32], &[I32])
}

/// Instantiate `module` on `mode` with a fresh configuration, and return the instance
/// together with the handle that reads the configuration's exit code.
fn instantiate(mode: EngineMode, config: Preview1Config, module: Module) -> (Instance, Preview1) {
    let preview1 = Preview1::new(config);
    let mut engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("accepted");
    preview1.define(engine.linker_mut());
    let instance = engine
        .instantiate(module)
        .unwrap_or_else(|error| panic!("{mode:?}: the fixture must instantiate: {error:?}"));
    (instance, preview1)
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four bytes"))
}

#[test]
fn fd_write_sends_granted_stdout_and_reports_the_count() {
    use ValueType::I32;
    for mode in BOTH {
        let stdout = Sink::default();
        let config = Preview1Config {
            stdout: Some(Box::new(stdout.clone())),
            ..Preview1Config::default()
        };
        // An iovec at 0 names the three bytes at 8. Write them to fd 1, then to fd 2.
        let module = module(
            &[("fd_write", fd_write_signature())],
            signature(&[], &[I32]),
            body(&[(0, vec![1, 0, 1, 16]), (0, vec![2, 0, 1, 16])]),
            vec![(0, vec![8, 0, 0, 0, 3, 0, 0, 0]), (8, b"hi\n".to_vec())],
        );
        let (mut instance, _) = instantiate(mode, config, module);
        assert_eq!(
            instance.call("run", Vec::new()).expect("run returns"),
            vec![Value::I32(errno::BADF)],
            "{mode:?}: fd 2 was not granted, so it is BADF"
        );
        assert_eq!(
            stdout.contents(),
            b"hi\n",
            "{mode:?}: the granted stream got the bytes"
        );
        assert_eq!(
            le_u32(&instance.read_memory("mem", 16, 4).expect("reads")),
            3,
            "{mode:?}: the refused write to fd 2 left the count from fd 1 alone"
        );
    }
}

#[test]
fn fd_write_reports_the_bytes_it_wrote_to_the_granted_stream() {
    use ValueType::I32;
    for mode in BOTH {
        let stdout = Sink::default();
        let config = Preview1Config {
            stdout: Some(Box::new(stdout.clone())),
            ..Preview1Config::default()
        };
        let module = module(
            &[("fd_write", fd_write_signature())],
            signature(&[], &[I32]),
            body(&[(0, vec![1, 0, 1, 16])]),
            vec![(0, vec![8, 0, 0, 0, 3, 0, 0, 0]), (8, b"hi\n".to_vec())],
        );
        let (mut instance, _) = instantiate(mode, config, module);
        assert_eq!(
            instance.call("run", Vec::new()).expect("run returns"),
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: a granted write succeeds"
        );
        assert_eq!(
            le_u32(&instance.read_memory("mem", 16, 4).expect("reads")),
            3,
            "{mode:?}: the count is what went out"
        );
    }
}

#[test]
fn args_and_environment_are_laid_out_as_the_abi_describes() {
    use ValueType::I32;
    for mode in BOTH {
        let config = Preview1Config {
            args: vec!["prog".into(), "x".into()],
            env: vec![("K".into(), "v".into())],
            ..Preview1Config::default()
        };
        // argc at 0, argv_buf_size at 4; the argv array at 16, its buffer at 64;
        // envc at 8, environ_buf_size at 12; the environ array at 32, its buffer at 80.
        let module = module(
            &[
                ("args_sizes_get", two_pointer_signature()),
                ("args_get", two_pointer_signature()),
                ("environ_sizes_get", two_pointer_signature()),
                ("environ_get", two_pointer_signature()),
            ],
            signature(&[], &[I32]),
            body(&[
                (0, vec![0, 4]),
                (1, vec![16, 64]),
                (2, vec![8, 12]),
                (3, vec![32, 80]),
            ]),
            Vec::new(),
        );
        let (mut instance, _) = instantiate(mode, config, module);
        assert_eq!(
            instance.call("run", Vec::new()).expect("run returns"),
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: every call succeeds"
        );
        let read = |at, length| instance.read_memory("mem", at, length).expect("reads");

        assert_eq!(le_u32(&read(0, 4)), 2, "{mode:?}: two arguments");
        assert_eq!(
            le_u32(&read(4, 4)),
            7,
            "{mode:?}: \"prog\\0x\\0\" is seven bytes"
        );
        assert_eq!(
            le_u32(&read(16, 4)),
            64,
            "{mode:?}: first argument at the buffer start"
        );
        assert_eq!(
            le_u32(&read(20, 4)),
            69,
            "{mode:?}: second after \"prog\\0\""
        );
        assert_eq!(read(64, 7), b"prog\0x\0", "{mode:?}: the argument strings");

        assert_eq!(le_u32(&read(8, 4)), 1, "{mode:?}: one variable");
        assert_eq!(
            le_u32(&read(12, 4)),
            4,
            "{mode:?}: \"K=v\\0\" is four bytes"
        );
        assert_eq!(le_u32(&read(32, 4)), 80, "{mode:?}: the variable's pointer");
        assert_eq!(read(80, 4), b"K=v\0", "{mode:?}: the variable string");
    }
}

#[test]
fn the_clock_is_virtual_and_advances_by_its_step() {
    use ValueType::{I32, I64};
    for mode in BOTH {
        let config = Preview1Config {
            clock_start_ns: 1_000,
            clock_step_ns: 500,
            ..Preview1Config::default()
        };
        let module = module(
            &[("clock_time_get", signature(&[I32, I64, I32], &[I32]))],
            signature(&[], &[I32]),
            clock_body(&[(1, 128), (0, 136), (9, 144)]),
            Vec::new(),
        );
        let (mut instance, _) = instantiate(mode, config, module);
        assert_eq!(
            instance.call("run", Vec::new()).expect("run returns"),
            vec![Value::I32(errno::INVAL)],
            "{mode:?}: clock 9 does not exist"
        );
        let read_u64 = |at| {
            u64::from_le_bytes(
                instance
                    .read_memory("mem", at, 8)
                    .expect("reads")
                    .try_into()
                    .expect("eight bytes"),
            )
        };
        assert_eq!(
            read_u64(128),
            1_000,
            "{mode:?}: the first reading is the start"
        );
        assert_eq!(
            read_u64(136),
            1_500,
            "{mode:?}: the next reading is one step later"
        );
        assert_eq!(
            read_u64(144),
            0,
            "{mode:?}: an invalid clock writes nothing"
        );
    }
}

/// The bytes `random_get` writes for a seed, read back from the module's memory.
fn random_bytes(mode: EngineMode, seed: u64) -> Vec<u8> {
    use ValueType::I32;
    let config = Preview1Config {
        random_seed: seed,
        ..Preview1Config::default()
    };
    let module = module(
        &[("random_get", two_pointer_signature())],
        signature(&[], &[I32]),
        body(&[(0, vec![256, 16])]),
        Vec::new(),
    );
    let (mut instance, _) = instantiate(mode, config, module);
    instance.call("run", Vec::new()).expect("run returns");
    instance.read_memory("mem", 256, 16).expect("reads")
}

#[test]
fn random_bytes_come_from_the_seed() {
    for mode in BOTH {
        let first = random_bytes(mode, 7);
        assert_eq!(
            first,
            random_bytes(mode, 7),
            "{mode:?}: the same seed gives the same bytes"
        );
        assert_ne!(
            first,
            random_bytes(mode, 8),
            "{mode:?}: another seed gives other bytes"
        );
        assert!(
            first.iter().any(|byte| *byte != 0),
            "{mode:?}: the bytes are not all zero"
        );
    }
}

#[test]
fn a_pointer_outside_the_memory_is_efault_not_a_trap() {
    use ValueType::I32;
    for mode in BOTH {
        let config = Preview1Config {
            stdout: Some(Box::new(Sink::default())),
            ..Preview1Config::default()
        };
        // The iovec pointer is past the one-page memory.
        let module = module(
            &[("fd_write", fd_write_signature())],
            signature(&[], &[I32]),
            body(&[(0, vec![1, 70_000, 1, 16])]),
            Vec::new(),
        );
        let (mut instance, _) = instantiate(mode, config, module);
        assert_eq!(
            instance
                .call("run", Vec::new())
                .expect("the call returns an errno"),
            vec![Value::I32(errno::FAULT)],
            "{mode:?}: a bad pointer is reported as EFAULT"
        );
    }
}

#[test]
fn proc_exit_records_the_code_and_stops_the_module() {
    use ValueType::I32;
    for mode in BOTH {
        let module = module(
            &[("proc_exit", signature(&[I32], &[]))],
            signature(&[], &[]),
            body(&[(0, vec![3])]),
            Vec::new(),
        );
        let (mut instance, preview1) = instantiate(mode, Preview1Config::default(), module);
        let error = instance
            .call("run", Vec::new())
            .expect_err("proc_exit stops the call");
        assert!(
            matches!(&error, RuntimeError::HostFunction { message, .. }
                if message.contains("proc_exit")),
            "{mode:?}: the stop is reported as a host failure, got {error:?}"
        );
        assert_eq!(
            preview1.exit_code(),
            Some(3),
            "{mode:?}: the embedder reads the code"
        );
    }
}

#[test]
fn stdin_is_read_into_the_iovecs_and_a_short_read_ends_the_call() {
    use ValueType::I32;
    for mode in BOTH {
        let config = Preview1Config {
            stdin: Some(Box::new(Cursor::new(b"abc".to_vec()))),
            ..Preview1Config::default()
        };
        // fd_read(0, iovs at 0, 1 iovec, nread at 16); the iovec is { buf 32, len 8 }.
        let module = module(
            &[("fd_read", fd_write_signature())],
            signature(&[], &[I32]),
            body(&[(0, vec![0, 0, 1, 16])]),
            vec![(0, vec![32, 0, 0, 0, 8, 0, 0, 0])],
        );
        let (mut instance, _) = instantiate(mode, config, module);
        assert_eq!(
            instance.call("run", Vec::new()).expect("run returns"),
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: the read succeeds"
        );
        assert_eq!(
            le_u32(&instance.read_memory("mem", 16, 4).expect("reads")),
            3,
            "{mode:?}: three bytes arrived"
        );
        assert_eq!(
            instance.read_memory("mem", 32, 3).expect("reads"),
            b"abc",
            "{mode:?}: the bytes landed in the iovec"
        );
    }
}

#[test]
fn a_closed_stream_is_badf_afterwards() {
    use ValueType::I32;
    for mode in BOTH {
        let stdout = Sink::default();
        let config = Preview1Config {
            stdout: Some(Box::new(stdout.clone())),
            ..Preview1Config::default()
        };
        let module = module(
            &[
                ("fd_close", signature(&[I32], &[I32])),
                ("fd_write", fd_write_signature()),
            ],
            signature(&[], &[I32]),
            body(&[(0, vec![1]), (1, vec![1, 0, 1, 16])]),
            vec![(0, vec![8, 0, 0, 0, 3, 0, 0, 0]), (8, b"hi\n".to_vec())],
        );
        let (mut instance, _) = instantiate(mode, config, module);
        assert_eq!(
            instance.call("run", Vec::new()).expect("run returns"),
            vec![Value::I32(errno::BADF)],
            "{mode:?}: fd 1 is closed, so a write to it is BADF"
        );
        assert!(
            stdout.contents().is_empty(),
            "{mode:?}: nothing reached the closed stream"
        );
    }
}
