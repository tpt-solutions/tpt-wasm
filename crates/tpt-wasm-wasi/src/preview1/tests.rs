// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Tests for the Preview 1 core, run through the public engine on both backends.
//!
//! Each test builds a small module that imports some Preview 1 functions and calls them
//! in a fixed order, then checks the bytes the functions left in the module's memory,
//! the value `run` returned, and what the embedder's sinks received.

use std::io::{self, Cursor, Write};
use std::sync::{Arc, Mutex};

use super::{errno, Directory, Preopen, Preview1, Preview1Config, MAX_FILE_BYTES};
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

// --- Files in granted directories -------------------------------------------------------

const NEW_PATH: u32 = 1600; // "newdir"
const TELL_OUT: u32 = 4320;
const STAT_OUT: u32 = 4500;
const POLL_SUB: u32 = 6000;
const POLL_OUT: u32 = 6100;
const POLL_COUNT: u32 = 6200;
const CLOCK_OUT: u32 = 6300;
const RES_OUT: u32 = 7000;

/// The shared file layout plus a path or record a test needs.
fn file_data_with(extra: Vec<(u32, Vec<u8>)>) -> Vec<(u32, Vec<u8>)> {
    let mut data = file_data(&[]);
    data.extend(extra);
    data
}

/// A clock subscription: user data, the clock (monotonic), the timeout, and whether the
/// timeout is an absolute deadline.
fn clock_subscription(userdata: u64, timeout: u64, absolute: bool) -> Vec<u8> {
    let mut record = vec![0u8; 48];
    record[0..8].copy_from_slice(&userdata.to_le_bytes());
    record[8] = 0;
    record[16..20].copy_from_slice(&1u32.to_le_bytes());
    record[24..32].copy_from_slice(&timeout.to_le_bytes());
    record[40..42].copy_from_slice(&u16::from(absolute).to_le_bytes());
    record
}

fn u64_at(instance: &Instance, at: u32) -> u64 {
    let bytes = instance
        .read_memory("mem", u64::from(at), 8)
        .expect("reads");
    u64::from_le_bytes(bytes.try_into().expect("eight bytes"))
}

#[test]
fn fd_tell_and_fdstat_describe_an_open_file() {
    for mode in BOTH {
        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(HELLO_PATH as i32),
                        Arg::I32(9),
                        Arg::I32(0),
                        Arg::I64(2),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                (
                    "fd_seek",
                    vec![
                        Arg::I32(4),
                        Arg::I64(6),
                        Arg::I32(0),
                        Arg::I32(SEEK_OUT as i32),
                    ],
                ),
                ("fd_tell", vec![Arg::I32(4), Arg::I32(TELL_OUT as i32)]),
                (
                    "fd_fdstat_get",
                    vec![Arg::I32(4), Arg::I32(STAT_OUT as i32)],
                ),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(
            u64_at(&instance, TELL_OUT),
            6,
            "{mode:?}: tell reports the seek"
        );
        let stat = bytes_at(&instance, STAT_OUT, 24);
        assert_eq!(stat[0], 4, "{mode:?}: a regular file");
        let base = u64::from_le_bytes(stat[8..16].try_into().expect("eight bytes"));
        assert_ne!(base & (1 << 1), 0, "{mode:?}: the descriptor can read");
    }
}

#[test]
fn filestat_reports_the_size_and_type_of_a_path() {
    for mode in BOTH {
        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[(
                "path_filestat_get",
                vec![
                    Arg::I32(3),
                    Arg::I32(0),
                    Arg::I32(HELLO_PATH as i32),
                    Arg::I32(9),
                    Arg::I32(STAT_OUT as i32),
                ],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        let stat = bytes_at(&instance, STAT_OUT, 64);
        assert_eq!(stat[16], 4, "{mode:?}: a regular file");
        assert_eq!(
            u64_at(&instance, STAT_OUT + 32),
            11,
            "{mode:?}: eleven bytes"
        );

        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[(
                "path_filestat_get",
                vec![
                    Arg::I32(3),
                    Arg::I32(0),
                    Arg::I32(SUB_PATH as i32),
                    Arg::I32(3),
                    Arg::I32(STAT_OUT as i32),
                ],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(
            bytes_at(&instance, STAT_OUT, 64)[16],
            3,
            "{mode:?}: a directory"
        );
    }
}

#[test]
fn a_directory_is_created_once_and_removed_when_it_is_empty() {
    let create = || {
        (
            "path_create_directory",
            vec![Arg::I32(3), Arg::I32(NEW_PATH as i32), Arg::I32(6)],
        )
    };
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[create()],
            file_data_with(vec![(NEW_PATH, b"newdir".to_vec())]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: created"
        );

        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[create(), create()],
            file_data_with(vec![(NEW_PATH, b"newdir".to_vec())]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::EXIST)],
            "{mode:?}: exists the second time"
        );

        // Create, remove, then stat: the path is gone.
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                create(),
                (
                    "path_remove_directory",
                    vec![Arg::I32(3), Arg::I32(NEW_PATH as i32), Arg::I32(6)],
                ),
                (
                    "path_filestat_get",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(NEW_PATH as i32),
                        Arg::I32(6),
                        Arg::I32(STAT_OUT as i32),
                    ],
                ),
            ],
            file_data_with(vec![(NEW_PATH, b"newdir".to_vec())]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOENT)], "{mode:?}: removed");
    }
}

#[test]
fn a_directory_with_contents_is_not_removed() {
    for mode in BOTH {
        let root = Directory::new()
            .with_file("sub/inner.txt", b"x".to_vec())
            .expect("the fixture tree is valid");
        let (result, _, _) = run_files(
            mode,
            vec![Preopen::new("/", root, true)],
            &[(
                "path_remove_directory",
                vec![Arg::I32(3), Arg::I32(SUB_PATH as i32), Arg::I32(3)],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOTEMPTY)], "{mode:?}");
    }
}

#[test]
fn unlink_removes_the_file_for_later_opens_and_the_host() {
    for mode in BOTH {
        let (result, _, preview1) = run_files(
            mode,
            grant(true),
            &[
                (
                    "path_unlink_file",
                    vec![Arg::I32(3), Arg::I32(HELLO_PATH as i32), Arg::I32(9)],
                ),
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(HELLO_PATH as i32),
                        Arg::I32(9),
                        Arg::I32(0),
                        Arg::I64(2),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOENT)], "{mode:?}");
        assert!(
            preview1.read_file(0, "hello.txt").is_none(),
            "{mode:?}: the host sees it gone"
        );
    }
}

#[test]
fn a_read_only_grant_refuses_directory_creation_removal_and_unlink() {
    for mode in BOTH {
        let calls: [(&str, Vec<Arg>); 4] = [
            (
                "path_rename",
                vec![
                    Arg::I32(3),
                    Arg::I32(HELLO_PATH as i32),
                    Arg::I32(9),
                    Arg::I32(3),
                    Arg::I32(RENAMED_PATH as i32),
                    Arg::I32(11),
                ],
            ),
            (
                "path_create_directory",
                vec![Arg::I32(3), Arg::I32(NEW_PATH as i32), Arg::I32(6)],
            ),
            (
                "path_unlink_file",
                vec![Arg::I32(3), Arg::I32(HELLO_PATH as i32), Arg::I32(9)],
            ),
            (
                "path_remove_directory",
                vec![Arg::I32(3), Arg::I32(SUB_PATH as i32), Arg::I32(3)],
            ),
        ];
        for (name, arguments) in calls {
            let (result, _, _) = run_files(
                mode,
                grant(false),
                &[(name, arguments)],
                file_data_with(vec![
                    (NEW_PATH, b"newdir".to_vec()),
                    (RENAMED_PATH, b"renamed.txt".to_vec()),
                ]),
            );
            assert_eq!(
                result,
                vec![Value::I32(errno::NOTCAPABLE)],
                "{mode:?}: {name}"
            );
        }
    }
}

#[test]
fn a_stat_that_leaves_the_grant_is_not_capable() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(false),
            &[(
                "path_filestat_get",
                vec![
                    Arg::I32(3),
                    Arg::I32(0),
                    Arg::I32(ESCAPE_PATH as i32),
                    Arg::I32(9),
                    Arg::I32(STAT_OUT as i32),
                ],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOTCAPABLE)], "{mode:?}");
    }
}

#[test]
fn poll_oneoff_sleeps_on_the_virtual_clock_and_reports_the_event() {
    for mode in BOTH {
        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "poll_oneoff",
                    vec![
                        Arg::I32(POLL_SUB as i32),
                        Arg::I32(POLL_OUT as i32),
                        Arg::I32(1),
                        Arg::I32(POLL_COUNT as i32),
                    ],
                ),
                // The clock now reads the deadline, 5000 ns after the start.
                (
                    "clock_time_get",
                    vec![Arg::I32(1), Arg::I64(0), Arg::I32(CLOCK_OUT as i32)],
                ),
            ],
            file_data_with(vec![(POLL_SUB, clock_subscription(7, 5000, false))]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(
            u64_at(&instance, CLOCK_OUT),
            5000,
            "{mode:?}: the clock slept to the deadline"
        );
        assert_eq!(le_at(&instance, POLL_COUNT), 1, "{mode:?}: one event");
        assert_eq!(
            u64_at(&instance, POLL_OUT),
            7,
            "{mode:?}: the user data comes back"
        );
        assert_eq!(
            bytes_at(&instance, POLL_OUT + 10, 1)[0],
            0,
            "{mode:?}: a clock event"
        );
    }
}

#[test]
fn a_descriptor_subscription_is_answered_without_sleeping() {
    for mode in BOTH {
        let mut descriptor = vec![0u8; 48];
        descriptor[0..8].copy_from_slice(&9u64.to_le_bytes());
        descriptor[8] = 1; // fd_read
        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "poll_oneoff",
                    vec![
                        Arg::I32(POLL_SUB as i32),
                        Arg::I32(POLL_OUT as i32),
                        Arg::I32(1),
                        Arg::I32(POLL_COUNT as i32),
                    ],
                ),
                (
                    "clock_time_get",
                    vec![Arg::I32(1), Arg::I64(0), Arg::I32(CLOCK_OUT as i32)],
                ),
            ],
            file_data_with(vec![(POLL_SUB, descriptor)]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(
            u64_at(&instance, CLOCK_OUT),
            0,
            "{mode:?}: the clock did not move"
        );
        let error = bytes_at(&instance, POLL_OUT + 8, 2);
        assert_eq!(
            u16::from_le_bytes([error[0], error[1]]),
            errno::NOTSUP as u16,
            "{mode:?}"
        );
    }
}

#[test]
fn sched_yield_clock_resolution_and_proc_raise_answer_as_the_abi_says() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(false),
            &[("sched_yield", vec![])],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: yielding succeeds"
        );

        let (result, _, _) = run_files(
            mode,
            grant(false),
            &[("proc_raise", vec![Arg::I32(9)])],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::NOTSUP)],
            "{mode:?}: signals are refused"
        );

        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[("clock_res_get", vec![Arg::I32(1), Arg::I32(RES_OUT as i32)])],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(u64_at(&instance, RES_OUT), 1, "{mode:?}: one nanosecond");

        let (result, _, _) = run_files(
            mode,
            grant(false),
            &[("clock_res_get", vec![Arg::I32(7), Arg::I32(RES_OUT as i32)])],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::INVAL)],
            "{mode:?}: an unknown clock"
        );
    }
}

/// An argument to a call in a test body: `i32` or `i64`, as the signature needs.
enum Arg {
    I32(i32),
    I64(i64),
}

/// The arguments of `path_open` for `hello.txt` read-only, which leaves its descriptor at 4.
fn open_hello_args() -> Vec<Arg> {
    vec![
        Arg::I32(3),
        Arg::I32(0),
        Arg::I32(HELLO_PATH as i32),
        Arg::I32(9),
        Arg::I32(0),
        Arg::I64(2),
        Arg::I64(0),
        Arg::I32(0),
        Arg::I32(FD_OUT as i32),
    ]
}

#[test]
fn allocate_grows_a_file_with_zeros_and_never_shrinks_it() {
    for mode in BOTH {
        let (result, _, preview1) = run_files(
            mode,
            grant(true),
            &[
                ("path_open", open_hello_args()),
                ("fd_allocate", vec![Arg::I32(4), Arg::I64(0), Arg::I64(16)]),
                ("fd_allocate", vec![Arg::I32(4), Arg::I64(0), Arg::I64(4)]),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        let mut expected = b"hello world".to_vec();
        expected.resize(16, 0);
        assert_eq!(
            preview1.read_file(0, "hello.txt"),
            Some(expected),
            "{mode:?}"
        );
    }
}

#[test]
fn allocate_refuses_an_empty_range_a_directory_a_read_only_grant_and_a_huge_size() {
    for mode in BOTH {
        type Calls = Vec<(&'static str, Vec<Arg>)>;
        let cases: [(Calls, Vec<Preopen>, i32); 4] = [
            (
                vec![
                    ("path_open", open_hello_args()),
                    ("fd_allocate", vec![Arg::I32(4), Arg::I64(0), Arg::I64(0)]),
                ],
                grant(true),
                errno::INVAL,
            ),
            (
                vec![
                    ("path_open", open_hello_args()),
                    (
                        "fd_allocate",
                        vec![Arg::I32(4), Arg::I64(0), Arg::I64(67_108_865)],
                    ),
                ],
                grant(true),
                errno::FBIG,
            ),
            (
                vec![
                    ("path_open", open_hello_args()),
                    ("fd_allocate", vec![Arg::I32(4), Arg::I64(0), Arg::I64(16)]),
                ],
                grant(false),
                errno::BADF,
            ),
            (
                vec![("fd_allocate", vec![Arg::I32(3), Arg::I64(0), Arg::I64(16)])],
                grant(true),
                errno::ISDIR,
            ),
        ];
        for (calls, preopens, expected) in cases {
            let (result, _, _) = run_files(mode, preopens, &calls, file_data_with(vec![]));
            assert_eq!(result, vec![Value::I32(expected)], "{mode:?}");
        }
    }
}

#[test]
fn advice_must_be_defined_and_sync_checks_the_descriptor() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[(
                "fd_advise",
                vec![Arg::I32(3), Arg::I64(0), Arg::I64(16), Arg::I32(2)],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}: random");
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[(
                "fd_advise",
                vec![Arg::I32(3), Arg::I64(0), Arg::I64(16), Arg::I32(9)],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::INVAL)],
            "{mode:?}: undefined"
        );
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                ("path_open", open_hello_args()),
                ("fd_sync", vec![Arg::I32(4)]),
                ("fd_datasync", vec![Arg::I32(4)]),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: open file"
        );
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[("fd_datasync", vec![Arg::I32(9)])],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::BADF)],
            "{mode:?}: no descriptor"
        );
    }
}

#[test]
fn links_and_timestamps_are_not_supported_by_the_tree() {
    for mode in BOTH {
        let calls: [(&str, Vec<Arg>); 5] = [
            (
                "path_link",
                vec![
                    Arg::I32(3),
                    Arg::I32(0),
                    Arg::I32(HELLO_PATH as i32),
                    Arg::I32(9),
                    Arg::I32(3),
                    Arg::I32(NEW_PATH as i32),
                    Arg::I32(6),
                ],
            ),
            (
                "path_symlink",
                vec![
                    Arg::I32(HELLO_PATH as i32),
                    Arg::I32(9),
                    Arg::I32(3),
                    Arg::I32(NEW_PATH as i32),
                    Arg::I32(6),
                ],
            ),
            (
                "path_readlink",
                vec![
                    Arg::I32(3),
                    Arg::I32(HELLO_PATH as i32),
                    Arg::I32(9),
                    Arg::I32(DIRENT_OUT as i32),
                    Arg::I32(64),
                    Arg::I32(DIRENT_USED as i32),
                ],
            ),
            (
                "path_filestat_set_times",
                vec![
                    Arg::I32(3),
                    Arg::I32(0),
                    Arg::I32(HELLO_PATH as i32),
                    Arg::I32(9),
                    Arg::I64(0),
                    Arg::I64(0),
                    Arg::I32(0),
                ],
            ),
            (
                "fd_filestat_set_times",
                vec![Arg::I32(3), Arg::I64(0), Arg::I64(0), Arg::I32(0)],
            ),
        ];
        for (name, arguments) in calls {
            let (result, _, _) = run_files(
                mode,
                grant(true),
                &[(name, arguments)],
                file_data_with(vec![(NEW_PATH, b"newdir".to_vec())]),
            );
            assert_eq!(result, vec![Value::I32(errno::NOTSUP)], "{mode:?}: {name}");
        }
    }
}

#[test]
fn set_flags_accepts_only_the_empty_set_on_a_known_descriptor() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[("fd_fdstat_set_flags", vec![Arg::I32(3), Arg::I32(0)])],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}: empty");
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[("fd_fdstat_set_flags", vec![Arg::I32(3), Arg::I32(1)])],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOTSUP)], "{mode:?}: append");
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[("fd_fdstat_set_flags", vec![Arg::I32(9), Arg::I32(0)])],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::BADF)], "{mode:?}: unknown");
    }
}

#[test]
fn renumber_moves_a_descriptor_and_frees_its_old_number() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                ("path_open", open_hello_args()),
                ("fd_renumber", vec![Arg::I32(4), Arg::I32(10)]),
                ("fd_tell", vec![Arg::I32(4), Arg::I32(TELL_OUT as i32)]),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::BADF)],
            "{mode:?}: old number"
        );
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                ("path_open", open_hello_args()),
                ("fd_renumber", vec![Arg::I32(4), Arg::I32(10)]),
                ("fd_tell", vec![Arg::I32(10), Arg::I32(TELL_OUT as i32)]),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: new number"
        );
    }
}

#[test]
fn renumber_replaces_an_open_target_and_later_opens_do_not_reuse_the_number() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                ("path_open", open_hello_args()),
                ("path_open", open_hello_args()),
                ("fd_renumber", vec![Arg::I32(4), Arg::I32(5)]),
                ("fd_tell", vec![Arg::I32(4), Arg::I32(TELL_OUT as i32)]),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::BADF)],
            "{mode:?}: moved away"
        );
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                ("path_open", open_hello_args()),
                ("fd_renumber", vec![Arg::I32(4), Arg::I32(10)]),
                ("path_open", open_hello_args()),
                ("fd_tell", vec![Arg::I32(10), Arg::I32(TELL_OUT as i32)]),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::SUCCESS)],
            "{mode:?}: not overwritten"
        );
    }
}

#[test]
fn renumber_refuses_grants_stdio_and_targets_on_them() {
    for mode in BOTH {
        let cases: [(&str, Vec<Arg>); 3] = [
            ("fd_renumber", vec![Arg::I32(3), Arg::I32(10)]),
            ("fd_renumber", vec![Arg::I32(4), Arg::I32(2)]),
            ("fd_renumber", vec![Arg::I32(4), Arg::I32(3)]),
        ];
        for (index, (name, arguments)) in cases.into_iter().enumerate() {
            let mut calls = Vec::new();
            if index > 0 {
                calls.push(("path_open", open_hello_args()));
            }
            calls.push((name, arguments));
            let (result, _, _) = run_files(mode, grant(true), &calls, file_data_with(vec![]));
            assert_eq!(
                result,
                vec![Value::I32(errno::NOTSUP)],
                "{mode:?}: case {index}"
            );
        }
    }
}

const DIRENT_OUT: u32 = 7400;
const DIRENT_USED: u32 = 7500;

/// The arguments of `fd_readdir` on the root grant, writing at `DIRENT_OUT` into a buffer of
/// `buffer` bytes and starting at `cookie`.
fn readdir_call(cookie: i64, buffer: i32) -> Vec<Arg> {
    vec![
        Arg::I32(3),
        Arg::I32(DIRENT_OUT as i32),
        Arg::I32(buffer),
        Arg::I64(cookie),
        Arg::I32(DIRENT_USED as i32),
    ]
}

/// The records `fd_readdir` wrote: each entry's next cookie, name, and file type.
fn dirents(instance: &Instance, at: u32, used: u32) -> Vec<(u64, String, u8)> {
    let bytes = bytes_at(instance, at, used);
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 24 <= bytes.len() {
        let next = u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("eight bytes"));
        let namlen = le_u32(&bytes[offset + 16..offset + 20]) as usize;
        let filetype = bytes[offset + 20];
        let name_end = (offset + 24 + namlen).min(bytes.len());
        let name = String::from_utf8_lossy(&bytes[offset + 24..name_end]).into_owned();
        out.push((next, name, filetype));
        offset = name_end;
    }
    out
}

#[test]
fn readdir_lists_dot_entries_then_children_in_name_order() {
    for mode in BOTH {
        let (result, instance, _) = run_files(
            mode,
            grant(true),
            &[("fd_readdir", readdir_call(0, 512))],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        let used = le_u32(&bytes_at(&instance, DIRENT_USED, 4));
        let entries = dirents(&instance, DIRENT_OUT, used);
        let names: Vec<&str> = entries.iter().map(|(_, name, _)| name.as_str()).collect();
        assert_eq!(names, [".", "..", "hello.txt", "sub"], "{mode:?}");
        let nexts: Vec<u64> = entries.iter().map(|(next, _, _)| *next).collect();
        assert_eq!(
            nexts,
            [1, 2, 3, 4],
            "{mode:?}: each cookie names the next entry"
        );
        // 3 is a directory, 4 a regular file.
        let types: Vec<u8> = entries.iter().map(|(_, _, filetype)| *filetype).collect();
        assert_eq!(types, [3, 3, 4, 3], "{mode:?}");
    }
}

#[test]
fn readdir_resumes_from_a_cookie() {
    for mode in BOTH {
        let (result, instance, _) = run_files(
            mode,
            grant(true),
            &[("fd_readdir", readdir_call(2, 512))],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        let used = le_u32(&bytes_at(&instance, DIRENT_USED, 4));
        let names: Vec<String> = dirents(&instance, DIRENT_OUT, used)
            .into_iter()
            .map(|(_, name, _)| name)
            .collect();
        assert_eq!(names, ["hello.txt", "sub"], "{mode:?}");
    }
}

#[test]
fn readdir_cuts_the_last_entry_where_the_buffer_ends() {
    for mode in BOTH {
        let (result, instance, _) = run_files(
            mode,
            grant(true),
            &[("fd_readdir", readdir_call(0, 30))],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        let used = le_u32(&bytes_at(&instance, DIRENT_USED, 4));
        // "." is 25 bytes; the next record is cut at the buffer's end.
        assert_eq!(used, 30, "{mode:?}: the buffer is filled");
        let entries = dirents(&instance, DIRENT_OUT, used);
        assert_eq!(
            entries.len(),
            1,
            "{mode:?}: only whole records are read back"
        );
        assert_eq!(entries[0].1, ".", "{mode:?}");
    }
}

#[test]
fn readdir_refuses_a_file_a_bad_descriptor_and_a_bad_buffer() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(HELLO_PATH as i32),
                        Arg::I32(9),
                        Arg::I32(0),
                        Arg::I64(2),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                (
                    "fd_readdir",
                    vec![
                        Arg::I32(4),
                        Arg::I32(DIRENT_OUT as i32),
                        Arg::I32(512),
                        Arg::I64(0),
                        Arg::I32(DIRENT_USED as i32),
                    ],
                ),
            ],
            file_data_with(vec![]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOTDIR)], "{mode:?}: a file");
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[(
                "fd_readdir",
                vec![
                    Arg::I32(9),
                    Arg::I32(DIRENT_OUT as i32),
                    Arg::I32(512),
                    Arg::I64(0),
                    Arg::I32(DIRENT_USED as i32),
                ],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::BADF)],
            "{mode:?}: no descriptor"
        );
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[(
                "fd_readdir",
                vec![
                    Arg::I32(3),
                    Arg::I32(0x7fff_0000),
                    Arg::I32(512),
                    Arg::I64(0),
                    Arg::I32(DIRENT_USED as i32),
                ],
            )],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::FAULT)],
            "{mode:?}: buffer outside memory"
        );
    }
}

/// The arguments of `path_rename` from one path to another, both under descriptor 3.
fn rename_arguments(from: (u32, u32), to: (u32, u32)) -> Vec<Arg> {
    vec![
        Arg::I32(3),
        Arg::I32(from.0 as i32),
        Arg::I32(from.1 as i32),
        Arg::I32(3),
        Arg::I32(to.0 as i32),
        Arg::I32(to.1 as i32),
    ]
}

#[test]
fn rename_moves_a_file_and_leaves_the_old_name_empty() {
    for mode in BOTH {
        let (result, _, preview1) = run_files(
            mode,
            grant(true),
            &[(
                "path_rename",
                rename_arguments((HELLO_PATH, 9), (RENAMED_PATH, 11)),
            )],
            file_data_with(vec![(RENAMED_PATH, b"renamed.txt".to_vec())]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert!(
            preview1.read_file(0, "hello.txt").is_none(),
            "{mode:?}: the old name is gone"
        );
        assert_eq!(
            preview1.read_file(0, "renamed.txt").as_deref(),
            Some(&b"hello world"[..]),
            "{mode:?}: the contents moved"
        );
    }
}

#[test]
fn rename_refuses_a_file_over_a_directory_and_a_missing_source() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[(
                "path_rename",
                rename_arguments((HELLO_PATH, 9), (SUB_PATH, 3)),
            )],
            file_data_with(vec![]),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::ISDIR)],
            "{mode:?}: file over dir"
        );
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[(
                "path_rename",
                rename_arguments((MISSING_PATH, 7), (RENAMED_PATH, 11)),
            )],
            file_data_with(vec![(RENAMED_PATH, b"renamed.txt".to_vec())]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOENT)], "{mode:?}: missing");
    }
}

#[test]
fn a_directory_cannot_be_moved_into_itself() {
    for mode in BOTH {
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[(
                "path_rename",
                rename_arguments((SUB_PATH, 3), (INSIDE_PATH, 10)),
            )],
            file_data_with(vec![(INSIDE_PATH, b"sub/inside".to_vec())]),
        );
        assert_eq!(result, vec![Value::I32(errno::INVAL)], "{mode:?}");
    }
}

#[test]
fn rename_cannot_cross_from_one_grant_to_another() {
    for mode in BOTH {
        let mut preopens = grant(true);
        preopens.push(Preopen::new("/b", Directory::new(), true));
        let (result, _, _) = run_files(
            mode,
            preopens,
            &[(
                "path_rename",
                vec![
                    Arg::I32(3),
                    Arg::I32(HELLO_PATH as i32),
                    Arg::I32(9),
                    Arg::I32(4),
                    Arg::I32(RENAMED_PATH as i32),
                    Arg::I32(11),
                ],
            )],
            file_data_with(vec![(RENAMED_PATH, b"renamed.txt".to_vec())]),
        );
        assert_eq!(result, vec![Value::I32(errno::XDEV)], "{mode:?}");
    }
}

/// The file-related imports, in the order their indices are given. A call names a
/// function by its entry here, so the index is its position.
fn file_imports() -> Vec<(&'static str, FunctionType)> {
    use ValueType::{I32, I64};
    vec![
        (
            "path_open",
            signature(&[I32, I32, I32, I32, I32, I64, I64, I32, I32], &[I32]),
        ),
        ("fd_read", fd_write_signature()),
        ("fd_write", fd_write_signature()),
        ("fd_close", signature(&[I32], &[I32])),
        ("fd_seek", signature(&[I32, I64, I32, I32], &[I32])),
        ("fd_prestat_get", two_pointer_signature()),
        ("fd_prestat_dir_name", signature(&[I32, I32, I32], &[I32])),
        ("fd_tell", two_pointer_signature()),
        ("fd_fdstat_get", two_pointer_signature()),
        ("fd_filestat_get", two_pointer_signature()),
        (
            "path_filestat_get",
            signature(&[I32, I32, I32, I32, I32], &[I32]),
        ),
        ("path_create_directory", signature(&[I32, I32, I32], &[I32])),
        ("path_remove_directory", signature(&[I32, I32, I32], &[I32])),
        ("path_unlink_file", signature(&[I32, I32, I32], &[I32])),
        ("poll_oneoff", signature(&[I32, I32, I32, I32], &[I32])),
        ("sched_yield", signature(&[], &[I32])),
        ("clock_res_get", two_pointer_signature()),
        ("clock_time_get", signature(&[I32, I64, I32], &[I32])),
        ("proc_raise", signature(&[I32], &[I32])),
        (
            "path_rename",
            signature(&[I32, I32, I32, I32, I32, I32], &[I32]),
        ),
        ("fd_readdir", signature(&[I32, I32, I32, I64, I32], &[I32])),
        ("fd_sync", signature(&[I32], &[I32])),
        ("fd_datasync", signature(&[I32], &[I32])),
        ("fd_advise", signature(&[I32, I64, I64, I32], &[I32])),
        ("fd_allocate", signature(&[I32, I64, I64], &[I32])),
        (
            "path_link",
            signature(&[I32, I32, I32, I32, I32, I32, I32], &[I32]),
        ),
        (
            "path_symlink",
            signature(&[I32, I32, I32, I32, I32], &[I32]),
        ),
        (
            "path_readlink",
            signature(&[I32, I32, I32, I32, I32, I32], &[I32]),
        ),
        (
            "path_filestat_set_times",
            signature(&[I32, I32, I32, I32, I64, I64, I32], &[I32]),
        ),
        (
            "fd_filestat_set_times",
            signature(&[I32, I64, I64, I32], &[I32]),
        ),
        ("fd_fdstat_set_flags", signature(&[I32, I32], &[I32])),
        ("fd_renumber", signature(&[I32, I32], &[I32])),
    ]
}

fn leb_i64(mut value: i64, out: &mut Vec<u8>) {
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

/// A body that calls each named file import in order with typed arguments. Every call
/// but the last has its result dropped, so the last call's errno is what `run` returns.
fn typed_body(calls: &[(&str, Vec<Arg>)]) -> Vec<u8> {
    let imports = file_imports();
    let mut out = Vec::new();
    for (position, (name, arguments)) in calls.iter().enumerate() {
        for argument in arguments {
            match argument {
                Arg::I32(value) => {
                    out.push(0x41);
                    leb_i32(*value, &mut out);
                }
                Arg::I64(value) => {
                    out.push(0x42);
                    leb_i64(*value, &mut out);
                }
            }
        }
        let index = imports
            .iter()
            .position(|(import, _)| import == name)
            .expect("a named import") as u32;
        out.push(0x10);
        leb_u32(index, &mut out);
        if position + 1 < calls.len() {
            out.push(0x1a);
        }
    }
    out.push(0x0b);
    out
}

/// A run of `calls` against `preopens`, with `data` in memory. Returns the last call's
/// errno, the instance (for reading memory), and the handle (for reading files).
fn run_files(
    mode: EngineMode,
    preopens: Vec<Preopen>,
    calls: &[(&str, Vec<Arg>)],
    data: Vec<(u32, Vec<u8>)>,
) -> (Vec<Value>, Instance, Preview1) {
    use ValueType::I32;
    let config = Preview1Config {
        preopens,
        ..Preview1Config::default()
    };
    let imports = file_imports();
    let module = module(
        &imports
            .iter()
            .map(|(name, sig)| (*name, sig.clone()))
            .collect::<Vec<_>>(),
        signature(&[], &[I32]),
        typed_body(calls),
        data,
    );
    let (mut instance, preview1) = instantiate(mode, config, module);
    let result = instance.call("run", Vec::new()).expect("run returns");
    (result, instance, preview1)
}

/// The bytes that go in memory at `at`: the path, or an iovec and its buffer.
fn iov(buffer: u32, length: u32) -> Vec<u8> {
    let mut out = buffer.to_le_bytes().to_vec();
    out.extend_from_slice(&length.to_le_bytes());
    out
}

/// Memory layout shared by the file tests.
const HELLO_PATH: u32 = 1024; // "hello.txt"
const OUT_PATH: u32 = 1100; // "out.txt"
const ESCAPE_PATH: u32 = 1200; // "../secret"
const ABSOLUTE_PATH: u32 = 1300; // "/etc"
const MISSING_PATH: u32 = 1400; // "missing"
const SUB_PATH: u32 = 1500; // "sub"
const READ_IOV: u32 = 2000; // one iovec: 64 bytes at 3000
const READ_BUFFER: u32 = 3000;
const WRITE_IOV: u32 = 2100; // one iovec: "abc" at 5000
const WRITE_DATA: u32 = 5000;
const FD_OUT: u32 = 4100;
const COUNT_OUT: u32 = 4000;
const SEEK_OUT: u32 = 4300;
const PRESTAT_OUT: u32 = 4200;
const NAME_OUT: u32 = 4400;
const RENAMED_PATH: u32 = 1700; // "renamed.txt"
const INSIDE_PATH: u32 = 1800; // "sub/inside"

fn file_data(writes: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut data = vec![
        (HELLO_PATH, b"hello.txt".to_vec()),
        (OUT_PATH, b"out.txt".to_vec()),
        (ESCAPE_PATH, b"../secret".to_vec()),
        (ABSOLUTE_PATH, b"/etc".to_vec()),
        (MISSING_PATH, b"missing".to_vec()),
        (SUB_PATH, b"sub".to_vec()),
        (READ_IOV, iov(READ_BUFFER, 64)),
        (WRITE_IOV, iov(WRITE_DATA, writes.len() as u32)),
    ];
    if !writes.is_empty() {
        data.push((WRITE_DATA, writes.to_vec()));
    }
    data
}

/// A writable or read-only grant of `hello.txt` ("hello world") and an empty `sub/`.
fn grant(writable: bool) -> Vec<Preopen> {
    let root = Directory::new()
        .with_file("hello.txt", b"hello world".to_vec())
        .and_then(|root| root.with_directory("sub"))
        .expect("the fixture tree is valid");
    vec![Preopen::new("/", root, writable)]
}

fn le_at(instance: &Instance, at: u32) -> u32 {
    le_u32(
        &instance
            .read_memory("mem", u64::from(at), 4)
            .expect("reads"),
    )
}

fn bytes_at(instance: &Instance, at: u32, length: u32) -> Vec<u8> {
    instance
        .read_memory("mem", u64::from(at), length as usize)
        .expect("reads")
}

#[test]
fn a_file_in_a_granted_directory_is_opened_and_read() {
    for mode in BOTH {
        // Rights 2 is fd_read. The first opened file is descriptor 4: 3 is the grant.
        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(HELLO_PATH as i32),
                        Arg::I32(9),
                        Arg::I32(0),
                        Arg::I64(2),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                (
                    "fd_read",
                    vec![
                        Arg::I32(4),
                        Arg::I32(READ_IOV as i32),
                        Arg::I32(1),
                        Arg::I32(COUNT_OUT as i32),
                    ],
                ),
            ],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(
            le_at(&instance, FD_OUT),
            4,
            "{mode:?}: the first file is fd 4"
        );
        assert_eq!(
            le_at(&instance, COUNT_OUT),
            11,
            "{mode:?}: eleven bytes read"
        );
        assert_eq!(
            bytes_at(&instance, READ_BUFFER, 11),
            b"hello world",
            "{mode:?}: the file's bytes arrived"
        );
    }
}

#[test]
fn a_writable_grant_creates_a_file_that_the_host_can_read_back() {
    for mode in BOTH {
        let writes = b"abc";
        // Create and write: oflags 1 is CREAT and rights 64 is fd_write.
        let (result, instance, preview1) = run_files(
            mode,
            grant(true),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(OUT_PATH as i32),
                        Arg::I32(7),
                        Arg::I32(1),
                        Arg::I64(64),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                (
                    "fd_write",
                    vec![
                        Arg::I32(4),
                        Arg::I32(WRITE_IOV as i32),
                        Arg::I32(1),
                        Arg::I32(COUNT_OUT as i32),
                    ],
                ),
            ],
            file_data(writes),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(
            le_at(&instance, COUNT_OUT),
            3,
            "{mode:?}: three bytes written"
        );
        assert_eq!(
            preview1.read_file(0, "out.txt").expect("the file exists"),
            writes,
            "{mode:?}: the file holds what the module wrote"
        );
    }
}

#[test]
fn a_path_that_leaves_the_grant_is_refused_as_not_capable() {
    for mode in BOTH {
        for (path, label) in [(ESCAPE_PATH, "../secret"), (ABSOLUTE_PATH, "/etc")] {
            let (result, _, _) = run_files(
                mode,
                grant(true),
                &[(
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(path as i32),
                        Arg::I32(label.len() as i32),
                        Arg::I32(1),
                        Arg::I64(64),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                )],
                file_data(&[]),
            );
            assert_eq!(
                result,
                vec![Value::I32(errno::NOTCAPABLE)],
                "{mode:?}: {label} is refused, and nothing outside the grant is reached"
            );
        }
    }
}

#[test]
fn a_read_only_grant_refuses_every_write_and_every_create() {
    for mode in BOTH {
        let create = vec![
            Arg::I32(3),
            Arg::I32(0),
            Arg::I32(OUT_PATH as i32),
            Arg::I32(7),
            Arg::I32(1),
            Arg::I64(0),
            Arg::I64(0),
            Arg::I32(0),
            Arg::I32(FD_OUT as i32),
        ];
        let (result, _, preview1) =
            run_files(mode, grant(false), &[("path_open", create)], file_data(&[]));
        assert_eq!(
            result,
            vec![Value::I32(errno::NOTCAPABLE)],
            "{mode:?}: creating a file in a read-only grant is refused"
        );
        assert!(
            preview1.read_file(0, "out.txt").is_none(),
            "{mode:?}: nothing was created"
        );

        // Opening an existing file for writing is refused too, by its write right.
        let write_open = vec![
            Arg::I32(3),
            Arg::I32(0),
            Arg::I32(HELLO_PATH as i32),
            Arg::I32(9),
            Arg::I32(0),
            Arg::I64(64),
            Arg::I64(0),
            Arg::I32(0),
            Arg::I32(FD_OUT as i32),
        ];
        let (result, _, _) = run_files(
            mode,
            grant(false),
            &[("path_open", write_open)],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOTCAPABLE)], "{mode:?}");
    }
}

#[test]
fn a_missing_file_is_noent_and_an_exclusive_create_of_an_existing_one_is_exist() {
    for mode in BOTH {
        let open = |path: u32, length: i32, oflags: i32| {
            vec![
                Arg::I32(3),
                Arg::I32(0),
                Arg::I32(path as i32),
                Arg::I32(length),
                Arg::I32(oflags),
                Arg::I64(2),
                Arg::I64(0),
                Arg::I32(0),
                Arg::I32(FD_OUT as i32),
            ]
        };
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[("path_open", open(MISSING_PATH, 7, 0))],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::NOENT)], "{mode:?}");

        // oflags 5 is CREAT and EXCL together.
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[("path_open", open(HELLO_PATH, 9, 5))],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::EXIST)], "{mode:?}");
    }
}

#[test]
fn fd_seek_moves_the_position_that_the_next_read_starts_from() {
    for mode in BOTH {
        // Seek to 6 from the start (whence 0), then read: the rest of "hello world".
        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(HELLO_PATH as i32),
                        Arg::I32(9),
                        Arg::I32(0),
                        Arg::I64(2),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                (
                    "fd_seek",
                    vec![
                        Arg::I32(4),
                        Arg::I64(6),
                        Arg::I32(0),
                        Arg::I32(SEEK_OUT as i32),
                    ],
                ),
                (
                    "fd_read",
                    vec![
                        Arg::I32(4),
                        Arg::I32(READ_IOV as i32),
                        Arg::I32(1),
                        Arg::I32(COUNT_OUT as i32),
                    ],
                ),
            ],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::SUCCESS)], "{mode:?}");
        assert_eq!(
            instance
                .read_memory("mem", u64::from(SEEK_OUT), 8)
                .expect("reads"),
            6u64.to_le_bytes(),
            "{mode:?}: the new position is written back"
        );
        assert_eq!(le_at(&instance, COUNT_OUT), 5, "{mode:?}");
        assert_eq!(bytes_at(&instance, READ_BUFFER, 5), b"world", "{mode:?}");
    }
}

#[test]
fn the_preopened_directory_is_described_by_prestat_and_refused_past_the_grant() {
    for mode in BOTH {
        let (result, instance, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "fd_prestat_get",
                    vec![Arg::I32(3), Arg::I32(PRESTAT_OUT as i32)],
                ),
                (
                    "fd_prestat_dir_name",
                    vec![Arg::I32(3), Arg::I32(NAME_OUT as i32), Arg::I32(1)],
                ),
                // Descriptor 4 was never opened, so it is not a preopen.
                (
                    "fd_prestat_get",
                    vec![Arg::I32(4), Arg::I32(PRESTAT_OUT as i32)],
                ),
            ],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::BADF)], "{mode:?}");
        // The record is a tag of 0 (directory), three bytes of padding, then the length.
        assert_eq!(
            instance
                .read_memory("mem", u64::from(PRESTAT_OUT), 8)
                .expect("reads"),
            [0, 0, 0, 0, 1, 0, 0, 0],
            "{mode:?}: the preopen is a directory named by one byte"
        );
        assert_eq!(bytes_at(&instance, NAME_OUT, 1), b"/", "{mode:?}");
    }
}

#[test]
fn a_file_cannot_be_read_as_a_directory_or_used_after_it_is_closed() {
    for mode in BOTH {
        // Opening "sub" with oflags 2 (DIRECTORY) gives descriptor 4, which fd_read refuses.
        let (result, _, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(SUB_PATH as i32),
                        Arg::I32(3),
                        Arg::I32(2),
                        Arg::I64(0),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                (
                    "fd_read",
                    vec![
                        Arg::I32(4),
                        Arg::I32(READ_IOV as i32),
                        Arg::I32(1),
                        Arg::I32(COUNT_OUT as i32),
                    ],
                ),
            ],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::ISDIR)], "{mode:?}");

        // Close a file, then read it: the descriptor is gone.
        let (result, _, _) = run_files(
            mode,
            grant(false),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(HELLO_PATH as i32),
                        Arg::I32(9),
                        Arg::I32(0),
                        Arg::I64(2),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                ("fd_close", vec![Arg::I32(4)]),
                (
                    "fd_read",
                    vec![
                        Arg::I32(4),
                        Arg::I32(READ_IOV as i32),
                        Arg::I32(1),
                        Arg::I32(COUNT_OUT as i32),
                    ],
                ),
            ],
            file_data(&[]),
        );
        assert_eq!(result, vec![Value::I32(errno::BADF)], "{mode:?}");
    }
}

#[test]
fn a_file_cannot_grow_past_the_cap_and_the_write_is_refused() {
    for mode in BOTH {
        // Seek to exactly the cap, then write one byte past it.
        let (result, _, _) = run_files(
            mode,
            grant(true),
            &[
                (
                    "path_open",
                    vec![
                        Arg::I32(3),
                        Arg::I32(0),
                        Arg::I32(HELLO_PATH as i32),
                        Arg::I32(9),
                        Arg::I32(0),
                        Arg::I64(64),
                        Arg::I64(0),
                        Arg::I32(0),
                        Arg::I32(FD_OUT as i32),
                    ],
                ),
                (
                    "fd_seek",
                    vec![
                        Arg::I32(4),
                        Arg::I64(MAX_FILE_BYTES as i64),
                        Arg::I32(0),
                        Arg::I32(SEEK_OUT as i32),
                    ],
                ),
                (
                    "fd_write",
                    vec![
                        Arg::I32(4),
                        Arg::I32(WRITE_IOV as i32),
                        Arg::I32(1),
                        Arg::I32(COUNT_OUT as i32),
                    ],
                ),
            ],
            file_data(b"x"),
        );
        assert_eq!(
            result,
            vec![Value::I32(errno::FBIG)],
            "{mode:?}: a write that would grow the file past the cap is refused"
        );
    }
}
