// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The core of WASI Preview 1, implemented against explicit grants.
//!
//! Each function here does what its Preview 1 definition says, for the part of the
//! ABI that needs no filesystem: command-line arguments, environment variables, a
//! clock, random bytes, standard output and error, standard input, and process exit.
//! Nothing is ambient. A module can write to standard output only if the embedder
//! passed a sink for it in [`Preview1Config`]; the clock is a virtual clock the
//! embedder starts and steps; random bytes come from a seed the embedder chose. The
//! same configuration therefore produces the same results on every run and on every
//! backend.
//!
//! Errors follow the ABI: a function returns an errno, which is `0` on success and a
//! small code otherwise, and the pointers it reads or writes are checked against the
//! caller's memory, so a bad pointer is `EFAULT` rather than a trap.
//!
//! Files are reached only through directories the embedder grants as [`Preopen`]s. Each
//! grant is an in-memory tree, so a module can read and, if the grant is writable, write
//! and create files, but never leave the granted directory and never touch the host
//! filesystem. `path_open`, `fd_seek`, `fd_prestat_get`, and `fd_prestat_dir_name` cover
//! that part of the ABI.
//!
//! Not provided, and so not installed: `path_*` operations other than `path_open` (such
//! as unlink, rename, and directory creation), file metadata queries, sockets, polling,
//! and the clock and random variants beyond the ones listed.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use tpt_wasm_runtime::{HostFunction, Linker};
use tpt_wasm_types::{FunctionType, GuestMemory, ResultType, Value, ValueType};

mod fs;

pub use fs::{Directory, Preopen, TreeUsage, MAX_FILE_BYTES};

use fs::{FsState, OpenRequest};

/// The errno values this module returns.
pub mod errno {
    pub const SUCCESS: i32 = 0;
    pub const BADF: i32 = 8;
    pub const XDEV: i32 = 18;
    pub const EXIST: i32 = 20;
    pub const FAULT: i32 = 21;
    pub const FBIG: i32 = 27;
    pub const INVAL: i32 = 28;
    pub const IO: i32 = 29;
    pub const ISDIR: i32 = 31;
    pub const MFILE: i32 = 33;
    pub const NOENT: i32 = 44;
    pub const NOTDIR: i32 = 54;
    pub const NOTEMPTY: i32 = 55;
    pub const NOTSUP: i32 = 58;
    pub const SPIPE: i32 = 70;
    pub const NOTCAPABLE: i32 = 76;
}

/// What the embedder grants to the Preview 1 functions.
///
/// Every field defaults to "nothing granted": no arguments, no variables, no standard
/// streams, a clock that starts at zero, and random bytes from seed zero.
#[derive(Default)]
pub struct Preview1Config {
    /// Command-line arguments, the first of which is conventionally the program name.
    pub args: Vec<String>,
    /// Environment variables as `(key, value)` pairs, in the order given.
    pub env: Vec<(String, String)>,
    /// Where file descriptor 0 reads from. Absent means `fd_read` on 0 is `EBADF`.
    pub stdin: Option<Box<dyn Read + Send>>,
    /// Where file descriptor 1 writes. Absent means `fd_write` on 1 is `EBADF`.
    pub stdout: Option<Box<dyn Write + Send>>,
    /// Where file descriptor 2 writes. Absent means `fd_write` on 2 is `EBADF`.
    pub stderr: Option<Box<dyn Write + Send>>,
    /// The virtual clock's first reading, in nanoseconds.
    pub clock_start_ns: u64,
    /// How far the virtual clock moves forward on each `clock_time_get`.
    pub clock_step_ns: u64,
    /// The seed for `random_get`. The same seed gives the same bytes.
    pub random_seed: u64,
    /// Directories the module may open files in. They take descriptors 3 and up, in the
    /// order given, and the module reaches nothing else on the filesystem.
    pub preopens: Vec<Preopen>,
}

/// The state the functions share. It lives behind one lock, so the functions see one
/// consistent view of the streams, the clock, and the random source.
struct State {
    args: Vec<Vec<u8>>,
    env: Vec<Vec<u8>>,
    stdin: Option<Box<dyn Read + Send>>,
    stdout: Option<Box<dyn Write + Send>>,
    stderr: Option<Box<dyn Write + Send>>,
    clock_ns: u64,
    clock_step_ns: u64,
    random: SplitMix64,
    exit_code: Option<u32>,
    fs: FsState,
}

/// SplitMix64: a small, well-known generator with a fixed sequence for a fixed seed.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// The Preview 1 functions for one embedder configuration.
///
/// Build one with [`Preview1::new`], install it with [`Preview1::define`], and read
/// [`Preview1::exit_code`] after a run to learn whether the module called `proc_exit`.
pub struct Preview1 {
    state: Arc<Mutex<State>>,
}

impl Preview1 {
    /// Build the functions from the grants in `config`.
    pub fn new(config: Preview1Config) -> Self {
        let state = State {
            args: config.args.into_iter().map(String::into_bytes).collect(),
            env: config
                .env
                .into_iter()
                .map(|(key, value)| format!("{key}={value}").into_bytes())
                .collect(),
            stdin: config.stdin,
            stdout: config.stdout,
            stderr: config.stderr,
            clock_ns: config.clock_start_ns,
            clock_step_ns: config.clock_step_ns,
            random: SplitMix64(config.random_seed),
            exit_code: None,
            fs: FsState::new(config.preopens),
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Install every function into `linker` under `wasi_snapshot_preview1`.
    ///
    /// This replaces any binding the same names had on the linker. Do not install this
    /// and a capability-backed [`WasiAdapter`](crate::WasiAdapter) for the same names.
    pub fn define(&self, linker: &mut Linker) {
        for function in Function::ALL {
            linker.define_function(
                crate::PREVIEW1_MODULE,
                function.name(),
                Implementation {
                    function,
                    state: Arc::clone(&self.state),
                },
            );
        }
    }

    /// The code the module passed to `proc_exit`, if it called it.
    pub fn exit_code(&self) -> Option<u32> {
        self.state.lock().ok().and_then(|state| state.exit_code)
    }

    /// The contents of a file in the granted directory at index `preopen` (the order
    /// the grants were given in), read as the run left it. `path` is relative to that
    /// directory. `None` if the grant or the file does not exist.
    pub fn read_file(&self, preopen: usize, path: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.fs.read_file(preopen, path))
    }

    /// What the granted trees hold now: files, directories, and file bytes. This is the host
    /// memory the module's files occupy, so it is the allocation a module causes outside its
    /// linear memory. `None` only if the state is poisoned.
    pub fn tree_usage(&self) -> Option<TreeUsage> {
        self.state.lock().ok().map(|state| state.fs.tree_usage())
    }
}

/// One Preview 1 function this module implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Function {
    ArgsSizesGet,
    ArgsGet,
    EnvironSizesGet,
    EnvironGet,
    ClockTimeGet,
    RandomGet,
    FdWrite,
    FdRead,
    FdClose,
    FdSeek,
    FdPrestatGet,
    FdPrestatDirName,
    PathOpen,
    FdTell,
    FdFdstatGet,
    FdFilestatGet,
    PathFilestatGet,
    PathCreateDirectory,
    PathRemoveDirectory,
    PathUnlinkFile,
    PathRename,
    FdReaddir,
    FdSync,
    FdDatasync,
    FdAdvise,
    FdAllocate,
    PathLink,
    PathSymlink,
    PathReadlink,
    PathFilestatSetTimes,
    FdFilestatSetTimes,
    FdFdstatSetFlags,
    FdRenumber,
    PollOneoff,
    SchedYield,
    ClockResGet,
    ProcRaise,
    ProcExit,
}

impl Function {
    const ALL: [Function; 38] = [
        Function::ArgsSizesGet,
        Function::ArgsGet,
        Function::EnvironSizesGet,
        Function::EnvironGet,
        Function::ClockTimeGet,
        Function::RandomGet,
        Function::FdWrite,
        Function::FdRead,
        Function::FdClose,
        Function::FdSeek,
        Function::FdPrestatGet,
        Function::FdPrestatDirName,
        Function::PathOpen,
        Function::FdTell,
        Function::FdFdstatGet,
        Function::FdFilestatGet,
        Function::PathFilestatGet,
        Function::PathCreateDirectory,
        Function::PathRemoveDirectory,
        Function::PathUnlinkFile,
        Function::PathRename,
        Function::FdReaddir,
        Function::FdSync,
        Function::FdDatasync,
        Function::FdAdvise,
        Function::FdAllocate,
        Function::PathLink,
        Function::PathSymlink,
        Function::PathReadlink,
        Function::PathFilestatSetTimes,
        Function::FdFilestatSetTimes,
        Function::FdFdstatSetFlags,
        Function::FdRenumber,
        Function::PollOneoff,
        Function::SchedYield,
        Function::ClockResGet,
        Function::ProcRaise,
        Function::ProcExit,
    ];

    fn name(self) -> &'static str {
        match self {
            Function::ArgsSizesGet => "args_sizes_get",
            Function::ArgsGet => "args_get",
            Function::EnvironSizesGet => "environ_sizes_get",
            Function::EnvironGet => "environ_get",
            Function::ClockTimeGet => "clock_time_get",
            Function::RandomGet => "random_get",
            Function::FdWrite => "fd_write",
            Function::FdRead => "fd_read",
            Function::FdClose => "fd_close",
            Function::FdSeek => "fd_seek",
            Function::FdPrestatGet => "fd_prestat_get",
            Function::FdPrestatDirName => "fd_prestat_dir_name",
            Function::PathOpen => "path_open",
            Function::FdTell => "fd_tell",
            Function::FdFdstatGet => "fd_fdstat_get",
            Function::FdFilestatGet => "fd_filestat_get",
            Function::PathFilestatGet => "path_filestat_get",
            Function::PathCreateDirectory => "path_create_directory",
            Function::PathRemoveDirectory => "path_remove_directory",
            Function::PathUnlinkFile => "path_unlink_file",
            Function::PathRename => "path_rename",
            Function::FdReaddir => "fd_readdir",
            Function::FdSync => "fd_sync",
            Function::FdDatasync => "fd_datasync",
            Function::FdAdvise => "fd_advise",
            Function::FdAllocate => "fd_allocate",
            Function::PathLink => "path_link",
            Function::PathSymlink => "path_symlink",
            Function::PathReadlink => "path_readlink",
            Function::PathFilestatSetTimes => "path_filestat_set_times",
            Function::FdFilestatSetTimes => "fd_filestat_set_times",
            Function::FdFdstatSetFlags => "fd_fdstat_set_flags",
            Function::FdRenumber => "fd_renumber",
            Function::PollOneoff => "poll_oneoff",
            Function::SchedYield => "sched_yield",
            Function::ClockResGet => "clock_res_get",
            Function::ProcRaise => "proc_raise",
            Function::ProcExit => "proc_exit",
        }
    }

    /// The signature the ABI gives this function.
    fn signature(self) -> FunctionType {
        use ValueType::{I32, I64};
        let (params, results): (Vec<ValueType>, Vec<ValueType>) = match self {
            Function::ArgsSizesGet
            | Function::ArgsGet
            | Function::EnvironSizesGet
            | Function::EnvironGet
            | Function::RandomGet => (vec![I32, I32], vec![I32]),
            Function::ClockTimeGet => (vec![I32, I64, I32], vec![I32]),
            Function::FdWrite | Function::FdRead => (vec![I32, I32, I32, I32], vec![I32]),
            Function::FdClose | Function::FdSync | Function::FdDatasync => (vec![I32], vec![I32]),
            Function::FdAdvise => (vec![I32, I64, I64, I32], vec![I32]),
            Function::FdAllocate => (vec![I32, I64, I64], vec![I32]),
            Function::PathLink => (vec![I32, I32, I32, I32, I32, I32, I32], vec![I32]),
            Function::PathSymlink => (vec![I32, I32, I32, I32, I32], vec![I32]),
            Function::PathReadlink => (vec![I32, I32, I32, I32, I32, I32], vec![I32]),
            Function::PathFilestatSetTimes => (vec![I32, I32, I32, I32, I64, I64, I32], vec![I32]),
            Function::FdFilestatSetTimes => (vec![I32, I64, I64, I32], vec![I32]),
            Function::FdFdstatSetFlags | Function::FdRenumber => (vec![I32, I32], vec![I32]),
            Function::FdSeek => (vec![I32, I64, I32, I32], vec![I32]),
            Function::FdPrestatGet => (vec![I32, I32], vec![I32]),
            Function::FdPrestatDirName => (vec![I32, I32, I32], vec![I32]),
            Function::PathOpen => (vec![I32, I32, I32, I32, I32, I64, I64, I32, I32], vec![I32]),
            Function::FdTell
            | Function::FdFdstatGet
            | Function::FdFilestatGet
            | Function::ClockResGet => (vec![I32, I32], vec![I32]),
            Function::PathFilestatGet => (vec![I32, I32, I32, I32, I32], vec![I32]),
            Function::PathCreateDirectory
            | Function::PathRemoveDirectory
            | Function::PathUnlinkFile => (vec![I32, I32, I32], vec![I32]),
            Function::PathRename => (vec![I32, I32, I32, I32, I32, I32], vec![I32]),
            Function::FdReaddir => (vec![I32, I32, I32, I64, I32], vec![I32]),
            Function::PollOneoff => (vec![I32, I32, I32, I32], vec![I32]),
            Function::SchedYield => (Vec::new(), vec![I32]),
            Function::ProcRaise => (vec![I32], vec![I32]),
            Function::ProcExit => (vec![I32], Vec::new()),
        };
        FunctionType {
            params: ResultType(params),
            results: ResultType(results),
        }
    }
}

/// A Preview 1 function bound to one configuration's shared state.
struct Implementation {
    function: Function,
    state: Arc<Mutex<State>>,
}

impl HostFunction for Implementation {
    fn function_type(&self) -> FunctionType {
        self.function.signature()
    }

    fn call(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        self.call_with_memory(None, args)
    }

    fn call_with_memory(
        &self,
        memory: Option<&mut dyn GuestMemory>,
        args: &[Value],
    ) -> Result<Vec<Value>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "the WASI state is poisoned".to_owned())?;
        let arguments: Vec<u64> = args
            .iter()
            .map(|value| match value {
                Value::I32(value) => u64::from(*value as u32),
                Value::I64(value) => *value as u64,
                _ => 0,
            })
            .collect();
        if self.function == Function::ProcExit {
            return proc_exit(&mut state, arguments[0]);
        }
        // None of these touches memory, so they answer without it.
        match self.function {
            // The tree has no links and keeps no times, so these are not supported.
            Function::PathLink
            | Function::PathSymlink
            | Function::PathReadlink
            | Function::PathFilestatSetTimes
            | Function::FdFilestatSetTimes => {
                return Ok(vec![Value::I32(errno::NOTSUP)]);
            }
            Function::FdRenumber => {
                return Ok(vec![Value::I32(fd_renumber(
                    &mut state,
                    arguments[0],
                    arguments[1],
                ))]);
            }
            Function::FdFdstatSetFlags => {
                return Ok(vec![Value::I32(fd_fdstat_set_flags(
                    &state,
                    arguments[0],
                    arguments[1],
                ))]);
            }
            Function::FdSync | Function::FdDatasync => {
                return Ok(vec![Value::I32(fd_sync(&state, arguments[0]))]);
            }
            Function::FdAdvise => {
                return Ok(vec![Value::I32(fd_advise(
                    &state,
                    arguments[0],
                    arguments[3],
                ))]);
            }
            Function::FdAllocate => {
                return Ok(vec![Value::I32(fd_allocate(
                    &mut state,
                    arguments[0],
                    arguments[1],
                    arguments[2],
                ))]);
            }
            // There is one thread, so yielding it is a no-op that succeeds.
            Function::SchedYield => return Ok(vec![Value::I32(errno::SUCCESS)]),
            // Signals are not delivered to a module, so raising one is refused.
            Function::ProcRaise => return Ok(vec![Value::I32(errno::NOTSUP)]),
            _ => {}
        }
        // Every other function reads or writes the caller's memory. Without memory the
        // pointers cannot be checked, so the answer is the one a bad pointer gets.
        let Some(memory) = memory else {
            return Ok(vec![Value::I32(errno::FAULT)]);
        };
        let result = match self.function {
            Function::PathOpen => fs::path_open(
                &mut state.fs,
                memory,
                OpenRequest {
                    dirfd: arguments[0],
                    path_at: arguments[2],
                    path_len: arguments[3],
                    oflags: arguments[4],
                    rights: arguments[5],
                    opened_at: arguments[8],
                },
            ),
            Function::FdSeek => fd_seek(
                &mut state,
                memory,
                arguments[0],
                arguments[1] as i64,
                arguments[2],
                arguments[3],
            ),
            Function::FdPrestatGet => {
                fs::prestat_get(&state.fs, memory, arguments[0], arguments[1])
            }
            Function::FdTell => fd_tell(&state, memory, arguments[0], arguments[1]),
            Function::FdFdstatGet => fd_fdstat_get(&state, memory, arguments[0], arguments[1]),
            Function::FdFilestatGet => fd_filestat_get(&state, memory, arguments[0], arguments[1]),
            Function::PathFilestatGet => fs::path_filestat_get(
                &state.fs,
                memory,
                arguments[0],
                arguments[2],
                arguments[3],
                arguments[4],
            ),
            Function::PathCreateDirectory => fs::create_directory(
                &mut state.fs,
                memory,
                arguments[0],
                arguments[1],
                arguments[2],
            ),
            Function::PathRemoveDirectory => fs::remove_directory(
                &mut state.fs,
                memory,
                arguments[0],
                arguments[1],
                arguments[2],
            ),
            Function::PathUnlinkFile => fs::unlink_file(
                &mut state.fs,
                memory,
                arguments[0],
                arguments[1],
                arguments[2],
            ),
            Function::FdReaddir => fs::readdir(
                &state.fs,
                memory,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3],
                arguments[4],
            ),
            Function::PathRename => fs::rename(
                &mut state.fs,
                memory,
                fs::PathArg {
                    dirfd: arguments[0],
                    path_at: arguments[1],
                    path_len: arguments[2],
                },
                fs::PathArg {
                    dirfd: arguments[3],
                    path_at: arguments[4],
                    path_len: arguments[5],
                },
            ),
            Function::PollOneoff => poll_oneoff(
                &mut state,
                memory,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3],
            ),
            Function::ClockResGet => clock_res_get(memory, arguments[0], arguments[1]),
            Function::SchedYield
            | Function::ProcRaise
            | Function::FdSync
            | Function::FdDatasync
            | Function::FdAdvise
            | Function::FdAllocate
            | Function::PathLink
            | Function::PathSymlink
            | Function::PathReadlink
            | Function::PathFilestatSetTimes
            | Function::FdFilestatSetTimes
            | Function::FdFdstatSetFlags
            | Function::FdRenumber => {
                unreachable!("handled before the memory check")
            }
            Function::FdPrestatDirName => {
                fs::prestat_dir_name(&state.fs, memory, arguments[0], arguments[1], arguments[2])
            }
            Function::ArgsSizesGet => sizes_get(memory, &state.args, arguments[0], arguments[1]),
            Function::ArgsGet => strings_get(memory, &state.args, arguments[0], arguments[1]),
            Function::EnvironSizesGet => sizes_get(memory, &state.env, arguments[0], arguments[1]),
            Function::EnvironGet => strings_get(memory, &state.env, arguments[0], arguments[1]),
            Function::ClockTimeGet => {
                clock_time_get(&mut state, memory, arguments[0], arguments[2])
            }
            Function::RandomGet => random_get(&mut state, memory, arguments[0], arguments[1]),
            Function::FdWrite => fd_write(
                &mut state,
                memory,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3],
            ),
            Function::FdRead => fd_read(
                &mut state,
                memory,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3],
            ),
            Function::FdClose => fd_close(&mut state, arguments[0]),
            Function::ProcExit => unreachable!("handled before the memory check"),
        };
        Ok(vec![Value::I32(result)])
    }
}

/// `proc_exit`: record the code and stop the module.
///
/// The stop is a host failure, which the runtime reports as an error from the call. The
/// code is kept in the state, so the embedder reads it with [`Preview1::exit_code`].
fn proc_exit(state: &mut State, code: u64) -> Result<Vec<Value>, String> {
    state.exit_code = Some(code as u32);
    Err("the module called proc_exit".into())
}

/// `args_sizes_get` and `environ_sizes_get`: the count, then the total bytes of all
/// strings with their terminators.
fn sizes_get(
    memory: &mut dyn GuestMemory,
    strings: &[Vec<u8>],
    count_at: u64,
    size_at: u64,
) -> i32 {
    let count = strings.len() as u32;
    let size: u32 = strings.iter().map(|string| string.len() as u32 + 1).sum();
    if write_u32(memory, count_at, count) && write_u32(memory, size_at, size) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `args_get` and `environ_get`: an array of pointers to NUL-terminated strings, and
/// the strings themselves, laid out one after another in the buffer.
fn strings_get(
    memory: &mut dyn GuestMemory,
    strings: &[Vec<u8>],
    array_at: u64,
    buffer_at: u64,
) -> i32 {
    let mut cursor = buffer_at;
    for (index, string) in strings.iter().enumerate() {
        let pointer_at = array_at + 4 * index as u64;
        if !write_u32(memory, pointer_at, cursor as u32) {
            return errno::FAULT;
        }
        let mut bytes = string.clone();
        bytes.push(0);
        if !memory.write(cursor, &bytes) {
            return errno::FAULT;
        }
        cursor += bytes.len() as u64;
    }
    errno::SUCCESS
}

/// `clock_time_get`: the virtual clock's reading, then the clock moves on by its step.
///
/// Only the realtime (0) and monotonic (1) clocks exist. Both read the same virtual
/// clock, which is what keeps a run reproducible.
fn clock_time_get(
    state: &mut State,
    memory: &mut dyn GuestMemory,
    clock: u64,
    time_at: u64,
) -> i32 {
    if clock > 1 {
        return errno::INVAL;
    }
    let now = state.clock_ns;
    state.clock_ns = state.clock_ns.saturating_add(state.clock_step_ns);
    if memory.write(time_at, &now.to_le_bytes()) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `random_get`: fill the buffer from the seeded generator.
fn random_get(state: &mut State, memory: &mut dyn GuestMemory, buffer_at: u64, length: u64) -> i32 {
    // A length past the memory can never be written, so refuse it before allocating.
    if length > memory.size() {
        return errno::FAULT;
    }
    let Ok(length) = usize::try_from(length) else {
        return errno::FAULT;
    };
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        bytes.extend_from_slice(&state.random.next_u64().to_le_bytes());
    }
    bytes.truncate(length);
    if memory.write(buffer_at, &bytes) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_write`: gather the iovecs and write them to the granted stream, then report
/// how many bytes went out.
fn fd_write(
    state: &mut State,
    memory: &mut dyn GuestMemory,
    fd: u64,
    iovs_at: u64,
    iovs_len: u64,
    written_at: u64,
) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::write(&mut state.fs, memory, fd, iovs_at, iovs_len, written_at);
    }
    let sink = match fd {
        1 => state.stdout.as_mut(),
        2 => state.stderr.as_mut(),
        _ => None,
    };
    let Some(sink) = sink else {
        return errno::BADF;
    };
    let mut total: u32 = 0;
    for index in 0..iovs_len {
        let Some((buffer, length)) = read_iovec(memory, iovs_at, index) else {
            return errno::FAULT;
        };
        let Some(bytes) = memory.read(u64::from(buffer), u64::from(length)) else {
            return errno::FAULT;
        };
        if sink.write_all(&bytes).is_err() {
            return errno::IO;
        }
        total = total.saturating_add(length);
    }
    if write_u32(memory, written_at, total) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_read`: fill the iovecs from the granted standard input, then report how many
/// bytes arrived.
///
/// A read that comes back short ends the call, as a short read does on a file: the
/// remaining iovecs are not filled from a second read in the same call.
fn fd_read(
    state: &mut State,
    memory: &mut dyn GuestMemory,
    fd: u64,
    iovs_at: u64,
    iovs_len: u64,
    read_at: u64,
) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::read(&mut state.fs, memory, fd, iovs_at, iovs_len, read_at);
    }
    if fd != 0 {
        return errno::BADF;
    }
    let Some(source) = state.stdin.as_mut() else {
        return errno::BADF;
    };
    let mut total: u32 = 0;
    for index in 0..iovs_len {
        let Some((buffer, length)) = read_iovec(memory, iovs_at, index) else {
            return errno::FAULT;
        };
        // Checked before allocating: a length past the memory can never be written.
        if u64::from(length) > memory.size() {
            return errno::FAULT;
        }
        let mut chunk = vec![0u8; length as usize];
        let filled = match source.read(&mut chunk) {
            Ok(count) => count,
            Err(_) => return errno::IO,
        };
        if !memory.write(u64::from(buffer), &chunk[..filled]) {
            return errno::FAULT;
        }
        total = total.saturating_add(filled as u32);
        if filled < chunk.len() {
            break;
        }
    }
    if write_u32(memory, read_at, total) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_close`: a standard stream that was granted can be closed, after which it is
/// `EBADF` like any descriptor that was never open.
fn fd_close(state: &mut State, fd: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::close(&mut state.fs, fd);
    }
    let closed = match fd {
        0 => state.stdin.take().is_some(),
        1 => state.stdout.take().is_some(),
        2 => state.stderr.take().is_some(),
        _ => false,
    };
    if closed {
        errno::SUCCESS
    } else {
        errno::BADF
    }
}

/// Whether a standard stream was granted and is still open.
fn stream_granted(state: &State, fd: u64) -> bool {
    match fd {
        0 => state.stdin.is_some(),
        1 => state.stdout.is_some(),
        2 => state.stderr.is_some(),
        _ => false,
    }
}

/// `fd_tell`: a file's position. A standard stream cannot be seeked, so it has none.
fn fd_tell(state: &State, memory: &mut dyn GuestMemory, fd: u64, at: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::tell(&state.fs, memory, fd, at);
    }
    if stream_granted(state, fd) {
        errno::SPIPE
    } else {
        errno::BADF
    }
}

/// `fd_renumber`. A stream cannot be renumbered, so it is refused with `NOTSUP`.
fn fd_renumber(state: &mut State, fd: u64, to: u64) -> i32 {
    let Ok(to) = u32::try_from(to) else {
        return errno::NOTSUP;
    };
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::renumber(&mut state.fs, fd, to);
    }
    if stream_granted(state, fd) {
        errno::NOTSUP
    } else {
        errno::BADF
    }
}

/// `fd_fdstat_set_flags`. A stream has no flags that can be set, so only the empty set is
/// accepted for it.
fn fd_fdstat_set_flags(state: &State, fd: u64, flags: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::set_flags(&state.fs, fd, flags);
    }
    if !stream_granted(state, fd) {
        errno::BADF
    } else if flags == 0 {
        errno::SUCCESS
    } else {
        errno::NOTSUP
    }
}

/// `fd_sync` and `fd_datasync`. A stream cannot be synced, so it is refused with `EINVAL`.
fn fd_sync(state: &State, fd: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::sync(&state.fs, fd);
    }
    if stream_granted(state, fd) {
        errno::INVAL
    } else {
        errno::BADF
    }
}

/// `fd_advise`. A stream cannot take advice, so it is refused with `EINVAL`.
fn fd_advise(state: &State, fd: u64, advice: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::advise(&state.fs, fd, advice);
    }
    if stream_granted(state, fd) {
        errno::INVAL
    } else {
        errno::BADF
    }
}

/// `fd_allocate`. A stream has no length to change, so it is refused with `EINVAL`.
fn fd_allocate(state: &mut State, fd: u64, offset: u64, length: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::allocate(&mut state.fs, fd, offset, length);
    }
    if stream_granted(state, fd) {
        errno::INVAL
    } else {
        errno::BADF
    }
}

/// `fd_fdstat_get`: a stream is a character device that can read (0) or write (1, 2).
fn fd_fdstat_get(state: &State, memory: &mut dyn GuestMemory, fd: u64, at: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::fdstat_get(&state.fs, memory, fd, at);
    }
    if !stream_granted(state, fd) {
        return errno::BADF;
    }
    let rights = if fd == 0 {
        fs::STDIN_RIGHTS
    } else {
        fs::STDOUT_RIGHTS
    };
    fs::stream_fdstat(memory, at, rights)
}

/// `fd_filestat_get`: the metadata of a file, directory, or granted stream.
fn fd_filestat_get(state: &State, memory: &mut dyn GuestMemory, fd: u64, at: u64) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::filestat_get(&state.fs, memory, fd, at);
    }
    if stream_granted(state, fd) {
        fs::stream_filestat(memory, at)
    } else {
        errno::BADF
    }
}

/// `clock_res_get`: the virtual clock ticks in nanoseconds, so its resolution is 1.
fn clock_res_get(memory: &mut dyn GuestMemory, clock: u64, at: u64) -> i32 {
    if clock > 1 {
        return errno::INVAL;
    }
    if memory.write(at, &1u64.to_le_bytes()) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    let mut word = [0u8; 2];
    word.copy_from_slice(&bytes[at..at + 2]);
    u16::from_le_bytes(word)
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

/// One `poll_oneoff` event: the user data, the error, and the event type (0 clock,
/// 1 fd_read, 2 fd_write), with the type's payload left zero.
fn event_record(userdata: u64, error: u16, kind: u8) -> [u8; 32] {
    let mut record = [0u8; 32];
    record[0..8].copy_from_slice(&userdata.to_le_bytes());
    record[8..10].copy_from_slice(&error.to_le_bytes());
    record[10] = kind;
    record
}

/// `poll_oneoff` over clock subscriptions and descriptor subscriptions.
///
/// A clock subscription sleeps on the virtual clock: the clock moves to the earliest
/// deadline, and no wall-clock time passes, so a run that sleeps stays reproducible. A
/// descriptor subscription is not modelled, so it is answered at once with `ENOTSUP` for
/// that event. If there is any descriptor subscription the call returns without sleeping.
fn poll_oneoff(
    state: &mut State,
    memory: &mut dyn GuestMemory,
    subscriptions_at: u64,
    events_at: u64,
    count: u64,
    written_at: u64,
) -> i32 {
    const SUBSCRIPTION: u64 = 48;
    const EVENT: u64 = 32;
    if count == 0 {
        return errno::INVAL;
    }
    // Checked before reading: a count past the memory can never be read.
    match count.checked_mul(SUBSCRIPTION) {
        Some(total) if total <= memory.size() => {}
        _ => return errno::FAULT,
    }
    let Some(bytes) = memory.read(subscriptions_at, count * SUBSCRIPTION) else {
        return errno::FAULT;
    };
    // (user data, absolute deadline in nanoseconds) for each clock subscription.
    let mut clocks: Vec<(u64, u64)> = Vec::new();
    // (user data, event type) for each descriptor subscription.
    let mut descriptors: Vec<(u64, u8)> = Vec::new();
    for index in 0..count {
        let start = (index * SUBSCRIPTION) as usize;
        let record = &bytes[start..start + SUBSCRIPTION as usize];
        let userdata = read_u64(record, 0);
        match record[8] {
            0 => {
                // The clock subscription's payload: id (u32), timeout (u64), precision
                // (u64), and flags (u16), at offsets 16, 24, 32, and 40.
                let clock = read_u32(record, 16);
                if clock > 1 {
                    return errno::INVAL;
                }
                let timeout = read_u64(record, 24);
                let absolute = read_u16(record, 40) & 1 != 0;
                let deadline = if absolute {
                    timeout
                } else {
                    state.clock_ns.saturating_add(timeout)
                };
                clocks.push((userdata, deadline));
            }
            tag @ (1 | 2) => descriptors.push((userdata, tag)),
            _ => return errno::INVAL,
        }
    }
    let events: Vec<[u8; 32]> = if descriptors.is_empty() {
        let earliest = clocks.iter().map(|(_, deadline)| *deadline).min();
        if let Some(deadline) = earliest {
            state.clock_ns = state.clock_ns.max(deadline);
        }
        clocks
            .iter()
            .filter(|(_, deadline)| *deadline <= state.clock_ns)
            .map(|(userdata, _)| event_record(*userdata, 0, 0))
            .collect()
    } else {
        descriptors
            .iter()
            .map(|(userdata, kind)| event_record(*userdata, errno::NOTSUP as u16, *kind))
            .collect()
    };
    for (index, record) in events.iter().enumerate() {
        if !memory.write(events_at + EVENT * index as u64, record) {
            return errno::FAULT;
        }
    }
    if write_u32(memory, written_at, events.len() as u32) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_seek`: move a file's position, or refuse on a descriptor that cannot seek.
///
/// The standard streams are not seekable, so a granted one is `ESPIPE` and an ungranted
/// one is `EBADF`, as for every other operation on it.
fn fd_seek(
    state: &mut State,
    memory: &mut dyn GuestMemory,
    fd: u64,
    offset: i64,
    whence: u64,
    new_offset_at: u64,
) -> i32 {
    if let Some(fd) = fs::file_descriptor(fd) {
        return fs::seek(&mut state.fs, memory, fd, offset, whence, new_offset_at);
    }
    let granted = match fd {
        0 => state.stdin.is_some(),
        1 => state.stdout.is_some(),
        2 => state.stderr.is_some(),
        _ => false,
    };
    if granted {
        errno::SPIPE
    } else {
        errno::BADF
    }
}

/// One iovec: a buffer pointer and its length, both little-endian `u32`.
pub(super) fn read_iovec(memory: &dyn GuestMemory, iovs_at: u64, index: u64) -> Option<(u32, u32)> {
    let at = iovs_at.checked_add(8 * index)?;
    let bytes = memory.read(at, 8)?;
    let buffer = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let length = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    Some((buffer, length))
}

pub(super) fn write_u32(memory: &mut dyn GuestMemory, at: u64, value: u32) -> bool {
    memory.write(at, &value.to_le_bytes())
}

#[cfg(test)]
mod tests;
