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
//! Not provided, and so not installed: every `path_*` and `fd_*` operation on a file,
//! sockets, polling, and the clock and random variants beyond the ones listed.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use tpt_wasm_runtime::{HostFunction, Linker};
use tpt_wasm_types::{FunctionType, GuestMemory, ResultType, Value, ValueType};

/// The errno values this module returns.
pub mod errno {
    pub const SUCCESS: i32 = 0;
    pub const BADF: i32 = 8;
    pub const FAULT: i32 = 21;
    pub const INVAL: i32 = 28;
    pub const IO: i32 = 29;
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
    ProcExit,
}

impl Function {
    const ALL: [Function; 10] = [
        Function::ArgsSizesGet,
        Function::ArgsGet,
        Function::EnvironSizesGet,
        Function::EnvironGet,
        Function::ClockTimeGet,
        Function::RandomGet,
        Function::FdWrite,
        Function::FdRead,
        Function::FdClose,
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
            Function::FdClose => (vec![I32], vec![I32]),
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
        // Every other function reads or writes the caller's memory. Without memory the
        // pointers cannot be checked, so the answer is the one a bad pointer gets.
        let Some(memory) = memory else {
            return Ok(vec![Value::I32(errno::FAULT)]);
        };
        let result = match self.function {
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

/// One iovec: a buffer pointer and its length, both little-endian `u32`.
fn read_iovec(memory: &dyn GuestMemory, iovs_at: u64, index: u64) -> Option<(u32, u32)> {
    let at = iovs_at.checked_add(8 * index)?;
    let bytes = memory.read(at, 8)?;
    let buffer = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let length = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    Some((buffer, length))
}

fn write_u32(memory: &mut dyn GuestMemory, at: u64, value: u32) -> bool {
    memory.write(at, &value.to_le_bytes())
}

#[cfg(test)]
mod tests;
