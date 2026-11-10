// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Debug sessions: pause a Micro call between instructions, look at it, and resume.
//!
//! A session runs one exported function on Micro, one instruction at a time, and
//! lets the caller stop at breakpoints, read the call stack, and read locals and
//! the operand stack. It is available on the Micro backend only. The compiled
//! backends run a whole function in one piece, so they have no instruction
//! boundary to stop at, and asking for a session on them is an error.
//!
//! ## Isolation
//!
//! The session owns the instance exclusively, which the borrow checker enforces. It
//! also works on a private copy of the store, and that is the point of the rule
//! below. While a session is open, the engine refuses every other call and every
//! instantiation, so no other instance can change the store under it. The copy is
//! written back only when the call returns or traps. A session that is dropped before
//! then is a rollback: memory and globals are as they were before it started.
//!
//! ## Breakpoints
//!
//! A breakpoint names an instruction by its function index and its position in that
//! function's decoded instruction list, not by a byte offset. A breakpoint stops
//! *before* the instruction runs. Resuming from a breakpoint runs that instruction
//! rather than stopping on it again.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use tpt_wasm_micro::machine::{step, Frame, Machine, Step};
use tpt_wasm_micro::store::FuncInstance;
use tpt_wasm_types::{Trap, Value};

use crate::{check_arguments, default_value, lock_call_gate, lock_store, Instance, RuntimeError};

/// Why a session stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    /// Stopped before the instruction at this position ran.
    Breakpoint { function: u32, pc: usize },
    /// One instruction ran and the call is still going.
    Stepped,
    /// The entry function returned these values. The store has been committed.
    Returned(Vec<Value>),
    /// The call trapped. The store has been committed.
    Trapped(Trap),
}

/// One frame of the call stack.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameInfo {
    /// The function index of the frame.
    pub function: u32,
    /// The position of the next instruction to run, in that function's instruction list.
    pub pc: usize,
    /// The frame's parameters and locals, in index order.
    pub locals: Vec<Value>,
}

/// A paused Micro call. See the module documentation for the isolation rule.
pub struct DebugSession<'a> {
    instance: &'a mut Instance,
    machine: Machine,
    breakpoints: BTreeSet<(u32, usize)>,
    /// The position the session last stopped at on a breakpoint, so resuming from
    /// it does not stop there again.
    stopped_at: Option<(u32, usize)>,
    finished: bool,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Instance {
    /// Start a debug session that will run the export `name` with `args`.
    ///
    /// Fails on the compiled backends, on a host function, on a name that is not a
    /// function, on bad arguments, and while another session is open on the same
    /// engine.
    pub fn debug(
        &mut self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<DebugSession<'_>, RuntimeError> {
        if self.baseline.is_some() {
            return Err(RuntimeError::UnsupportedFeature(
                "debug sessions run on the Micro backend",
            ));
        }
        let address = self.get_function(name)?;
        let gate = lock_call_gate(&self.context.call_gate)?;
        if self.context.debug_active.load(Ordering::SeqCst) {
            return Err(RuntimeError::DebugSessionActive);
        }
        let frame = self.entry_frame(address, args)?;
        let store = lock_store(&self.context.store)?.clone();
        let mut machine = Machine::with_execution(store, self.execution);
        machine.push_frame(frame).map_err(RuntimeError::Trap)?;
        self.context.debug_active.store(true, Ordering::SeqCst);
        let active = std::sync::Arc::clone(&self.context.debug_active);
        drop(gate);
        Ok(DebugSession {
            instance: self,
            machine,
            breakpoints: BTreeSet::new(),
            stopped_at: None,
            finished: false,
            active,
        })
    }

    /// The frame an entry call starts with, built the way the ordinary call path builds it.
    fn entry_frame(&self, address: u32, args: Vec<Value>) -> Result<Frame, RuntimeError> {
        let function = {
            let store = lock_store(&self.context.store)?;
            store.function(address)?.clone()
        };
        match function {
            FuncInstance::Wasm(function) => {
                check_arguments(&function.func_type, &args)?;
                let mut locals = args;
                locals.extend(
                    function
                        .local_types
                        .iter()
                        .skip(function.func_type.params.0.len())
                        .copied()
                        .map(default_value),
                );
                Ok(Frame::new(
                    function.instance_idx,
                    function.func_idx,
                    locals,
                    function.instrs.clone(),
                    function.func_type.results.0.len(),
                ))
            }
            FuncInstance::Host(_) => Err(RuntimeError::UnsupportedFeature(
                "a host function cannot be debugged",
            )),
        }
    }
}

impl DebugSession<'_> {
    /// The position of the next instruction: the innermost frame's function and pc.
    pub fn position(&self) -> Option<(u32, usize)> {
        self.machine
            .frames
            .last()
            .map(|frame| (frame.func_idx, frame.pc))
    }

    /// Stop before the instruction at `pc` in function `function` runs.
    pub fn add_breakpoint(&mut self, function: u32, pc: usize) {
        self.breakpoints.insert((function, pc));
    }

    /// Remove a breakpoint. Returns whether one was set there.
    pub fn remove_breakpoint(&mut self, function: u32, pc: usize) -> bool {
        self.breakpoints.remove(&(function, pc))
    }

    /// Whether the call has returned or trapped, after which the session is spent.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Instructions charged so far in this call, on the same count `last_call_steps`
    /// reports for an ordinary call.
    pub fn steps(&self) -> u64 {
        self.machine.steps
    }

    /// The call stack, innermost frame first.
    pub fn stack(&self) -> Vec<FrameInfo> {
        self.machine
            .frames
            .iter()
            .rev()
            .map(|frame| FrameInfo {
                function: frame.func_idx,
                pc: frame.pc,
                locals: frame.locals.clone(),
            })
            .collect()
    }

    /// The operand stack, bottom first.
    pub fn operand_stack(&self) -> &[Value] {
        &self.machine.operand_stack
    }

    /// Run exactly one instruction, ignoring breakpoints.
    pub fn step(&mut self) -> Result<StopReason, RuntimeError> {
        self.check_live()?;
        self.stopped_at = None;
        Ok(self.advance()?.unwrap_or(StopReason::Stepped))
    }

    /// Run until a breakpoint, or until the call returns or traps.
    ///
    /// A breakpoint at the current position is honoured unless the session stopped
    /// there last time, so resuming runs the instruction it stopped before.
    pub fn run(&mut self) -> Result<StopReason, RuntimeError> {
        self.check_live()?;
        loop {
            if let Some(position) = self.position() {
                if self.breakpoints.contains(&position) && self.stopped_at != Some(position) {
                    self.stopped_at = Some(position);
                    return Ok(StopReason::Breakpoint {
                        function: position.0,
                        pc: position.1,
                    });
                }
            }
            self.stopped_at = None;
            if let Some(stop) = self.advance()? {
                return Ok(stop);
            }
        }
    }

    fn check_live(&self) -> Result<(), RuntimeError> {
        if self.finished {
            Err(RuntimeError::DebugSessionFinished)
        } else {
            Ok(())
        }
    }

    /// Run one machine step, and any host call it makes. `None` while the call goes on.
    fn advance(&mut self) -> Result<Option<StopReason>, RuntimeError> {
        let mut outcome = step(&mut self.machine);
        loop {
            match outcome {
                Step::Continue => return Ok(None),
                Step::Return(values) => {
                    self.close()?;
                    return Ok(Some(StopReason::Returned(values)));
                }
                Step::Trap(trap) => {
                    self.close()?;
                    return Ok(Some(StopReason::Trapped(trap)));
                }
                Step::HostCall(call) => {
                    let results = match self.instance.invoke_host_call(&call) {
                        Ok(results) => results,
                        Err(error) => {
                            self.close()?;
                            return Err(error);
                        }
                    };
                    outcome = match self.machine.resume_host_call(results) {
                        Ok(next) => next,
                        Err(trap) => Step::Trap(trap),
                    };
                }
            }
        }
    }

    /// End the session: write the store back and release the engine.
    fn close(&mut self) -> Result<(), RuntimeError> {
        self.finished = true;
        self.active.store(false, Ordering::SeqCst);
        self.instance.commit_store(self.machine.store.clone())
    }
}

impl Drop for DebugSession<'_> {
    fn drop(&mut self) {
        // A session that was not run to its end is a rollback: its store copy is
        // discarded, and only the engine is released.
        self.active.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Engine, EngineMode};

    /// `add(i32, i32) -> i32`. Its body is `local.get 0; local.get 1; i32.add; end`,
    /// so position 2 is the add, with both operands on the operand stack before it.
    const ADD: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, //
        0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, //
        0x03, 0x02, 0x01, 0x00, //
        0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00, //
        0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
    ];

    /// `set() -> ()` stores 42 at address 0 of a one-page memory; `mem` exports it.
    fn writing_bytes() -> Vec<u8> {
        use tpt_wasm_format::{Export, ExportDesc, Function, Memory, Module};
        use tpt_wasm_types::{FunctionType, Limits, MemoryType, ResultType};
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(Vec::new()),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x41, 0x00, 0x41, 0x2a, 0x36, 0x02, 0x00, 0x0b],
            }],
            memories: vec![Memory {
                memory_type: MemoryType {
                    limits: Limits { min: 1, max: None },
                    memory64: false,
                },
            }],
            exports: vec![
                Export {
                    name: "set".into(),
                    desc: ExportDesc::Function(0),
                },
                Export {
                    name: "mem".into(),
                    desc: ExportDesc::Memory(0),
                },
            ],
            ..Module::default()
        };
        tpt_wasm_decode::encode(&module).expect("the fixture must encode")
    }

    fn micro(bytes: &[u8]) -> Instance {
        Engine::new(Config {
            engine_mode: EngineMode::Micro,
            ..Config::default()
        })
        .expect("micro is accepted")
        .instantiate_bytes(bytes)
        .expect("the fixture instantiates")
    }

    #[test]
    fn a_breakpoint_stops_before_the_instruction_with_its_operands_on_the_stack() {
        let mut instance = micro(ADD);
        let mut session = instance
            .debug("add", vec![Value::I32(7), Value::I32(35)])
            .expect("the session starts");
        session.add_breakpoint(0, 2);
        assert_eq!(
            session.run().expect("runs to the breakpoint"),
            StopReason::Breakpoint { function: 0, pc: 2 }
        );
        assert_eq!(session.operand_stack(), &[Value::I32(7), Value::I32(35)]);
        assert_eq!(session.stack().len(), 1);
        assert_eq!(
            session.stack()[0].locals,
            vec![Value::I32(7), Value::I32(35)]
        );
        assert_eq!(
            session.run().expect("resumes past the breakpoint"),
            StopReason::Returned(vec![Value::I32(42)])
        );
        assert!(session.is_finished());
    }

    #[test]
    fn single_steps_add_up_to_the_same_count_an_ordinary_call_charges() {
        let mut stepped = micro(ADD);
        let mut session = stepped
            .debug("add", vec![Value::I32(1), Value::I32(2)])
            .expect("starts");
        let mut last = StopReason::Stepped;
        while !session.is_finished() {
            last = session.step().expect("steps");
        }
        assert_eq!(last, StopReason::Returned(vec![Value::I32(3)]));
        let stepped_count = session.steps();
        drop(session);

        let mut ordinary = micro(ADD);
        ordinary
            .call("add", vec![Value::I32(1), Value::I32(2)])
            .expect("calls");
        assert_eq!(
            stepped_count,
            ordinary.last_call_steps(),
            "stepping must charge the same instructions a call does"
        );
    }

    #[test]
    fn a_session_dropped_before_it_ends_leaves_memory_as_it_was() {
        let bytes = writing_bytes();
        let mut instance = micro(&bytes);
        {
            let mut session = instance.debug("set", Vec::new()).expect("starts");
            session.step().expect("one instruction runs");
            // Dropped here, mid-call.
        }
        assert_eq!(
            instance.read_memory("mem", 0, 4).expect("reads"),
            vec![0, 0, 0, 0],
            "a dropped session must not commit its store"
        );
        instance
            .call("set", Vec::new())
            .expect("an ordinary call works again");
        assert_eq!(
            instance.read_memory("mem", 0, 4).expect("reads"),
            vec![42, 0, 0, 0]
        );
    }

    #[test]
    fn a_finished_session_commits_its_store() {
        let bytes = writing_bytes();
        let mut instance = micro(&bytes);
        {
            let mut session = instance.debug("set", Vec::new()).expect("starts");
            assert_eq!(
                session.run().expect("runs"),
                StopReason::Returned(Vec::new())
            );
        }
        assert_eq!(
            instance.read_memory("mem", 0, 4).expect("reads"),
            vec![42, 0, 0, 0]
        );
    }

    #[test]
    fn while_a_session_is_open_no_other_instance_can_call_or_instantiate() {
        let bytes = writing_bytes();
        let engine = Engine::new(Config {
            engine_mode: EngineMode::Micro,
            ..Config::default()
        })
        .expect("accepted");
        let mut paused = engine.instantiate_bytes(&bytes).expect("instantiates");
        let mut other = engine.instantiate_bytes(&bytes).expect("instantiates");
        let mut session = paused.debug("set", Vec::new()).expect("starts");
        session.step().expect("one instruction runs");
        assert!(matches!(
            other.call("set", Vec::new()),
            Err(RuntimeError::DebugSessionActive)
        ));
        assert!(matches!(
            engine.instantiate_bytes(&bytes),
            Err(RuntimeError::DebugSessionActive)
        ));
        drop(session);
        other
            .call("set", Vec::new())
            .expect("the engine is released when the session ends");
    }

    #[test]
    fn a_second_session_on_the_same_engine_is_refused() {
        let bytes = writing_bytes();
        let engine = Engine::new(Config {
            engine_mode: EngineMode::Micro,
            ..Config::default()
        })
        .expect("accepted");
        let mut first = engine.instantiate_bytes(&bytes).expect("instantiates");
        let mut second = engine.instantiate_bytes(&bytes).expect("instantiates");
        let _session = first.debug("set", Vec::new()).expect("starts");
        assert!(matches!(
            second.debug("set", Vec::new()),
            Err(RuntimeError::DebugSessionActive)
        ));
    }

    #[test]
    fn a_session_is_refused_on_the_compiled_backends_and_for_bad_names() {
        let engine = Engine::new(Config {
            engine_mode: EngineMode::Baseline,
            ..Config::default()
        })
        .expect("accepted");
        let mut baseline = engine.instantiate_bytes(ADD).expect("instantiates");
        assert!(matches!(
            baseline.debug("add", vec![Value::I32(1), Value::I32(2)]),
            Err(RuntimeError::UnsupportedFeature(_))
        ));
        let mut instance = micro(ADD);
        assert!(matches!(
            instance.debug("nope", Vec::new()),
            Err(RuntimeError::UnknownExport(_))
        ));
        assert!(instance.debug("add", vec![Value::I32(1)]).is_err());
    }

    #[test]
    fn a_finished_session_refuses_further_steps() {
        let mut instance = micro(ADD);
        let mut session = instance
            .debug("add", vec![Value::I32(1), Value::I32(2)])
            .expect("starts");
        session.run().expect("runs to the end");
        assert!(matches!(
            session.step(),
            Err(RuntimeError::DebugSessionFinished)
        ));
    }
}
