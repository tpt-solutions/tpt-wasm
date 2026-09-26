// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Runs the vendored WebAssembly Core spec test suite: `module`, `invoke`,
//! `assert_return`, `assert_trap`, `assert_exhaustion`, and `register`
//! directives, executed through the real decode/validate/execute pipeline.
//!
//! `.wast` source is text, and the vendored Core suite is written almost
//! entirely in the text format rather than `(module binary ...)`. Parsing and
//! encoding that text to bytes is delegated to the upstream `wast` crate (part
//! of `bytecodealliance/wasm-tools`) -- used *only* to turn s-expressions into
//! bytes. Everything the bytes are then run through -- decode, validate,
//! instantiate, call -- is tpt-wasm's own code, unchanged from the binary-format
//! suite in `binary_format.rs`. That division keeps the thing actually under
//! test (this project's decoder, validator, and interpreter) independent of the
//! text encoder: a bug in `wast`'s encoder would produce a binary module that
//! is not what the test source describes, and would surface as a spurious
//! decode/validate/value mismatch here rather than being silently absorbed by a
//! second, self-written implementation of the format.
//!
//! Directives this harness does not drive to completion -- `assert_malformed`,
//! `assert_invalid`, `assert_unlinkable`, and the exception/thread/component
//! directives -- are reported as [`CoreOutcome::Skipped`] rather than being
//! silently ignored, so a suite's coverage is visible in its case list.
//!
//! [`run_core_suite_with`] chooses the execution backend, so the same directives
//! can be run through `Micro` and through the compiled baseline. Everything
//! between the decoded bytes and the compared result is identical either way, so
//! a disagreement between the two is a disagreement about execution.

use std::collections::HashMap;

use tpt_wasm_runtime::{Config, Engine, EngineMode, Instance, RuntimeError};
use tpt_wasm_types::{Trap, Value};

use wast::core::{NanPattern, WastArgCore, WastRetCore};
use wast::parser::ParseBuffer;
use wast::token::{F32, F64};
use wast::{Wast, WastArg, WastDirective, WastExecute, WastRet};

/// What running one directive against the real pipeline produced.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreOutcome {
    /// A module decoded, validated, and instantiated (running its start
    /// function, if any).
    Instantiated,
    /// An `invoke` or `assert_return` executed and produced the expected
    /// values (a bare `invoke` only requires executing without trapping).
    Returned,
    /// An `assert_trap` / `assert_exhaustion` invocation trapped, as required.
    Trapped,
    /// This harness does not run this directive.
    Skipped(&'static str),
    /// The directive ran, but not the way it was supposed to.
    Failed(String),
}

/// One directive, with the line it starts on and its outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreCase {
    pub line: usize,
    pub directive: &'static str,
    pub outcome: CoreOutcome,
}

/// Parse and run every directive in a Core suite `.wast` source, in file order,
/// on the `Micro` backend.
///
/// Use [`run_core_suite_with`] to choose a different one; the two differ only in
/// which backend executes the decoded bytes.
pub fn run_core_suite(source: &str) -> Result<Vec<CoreCase>, String> {
    run_core_suite_with(source, EngineMode::Micro)
}

/// Parse and run every directive in a Core suite `.wast` source, in file order,
/// on the given backend.
///
/// The backend only decides how the decoded bytes are executed. Parsing the text,
/// instantiating, and comparing the returned values against upstream's patterns
/// are the same code either way, so a directive that passes on one backend and
/// fails on the other is a difference in execution, not in the harness.
pub fn run_core_suite_with(source: &str, mode: EngineMode) -> Result<Vec<CoreCase>, String> {
    let buf = ParseBuffer::new(source).map_err(|error| error.to_string())?;
    let wast: Wast = wast::parser::parse(&buf).map_err(|error| error.to_string())?;

    let config = Config {
        engine_mode: mode,
        ..Config::default()
    };
    let engine = Engine::new(config).map_err(|error| error.to_string())?;
    let mut runner = Runner {
        engine,
        modules: Vec::new(),
        named: HashMap::new(),
        current: None,
    };

    let mut cases = Vec::new();
    for directive in wast.directives {
        let (line, _) = directive.span().linecol_in(source);
        let (directive_name, outcome) = runner.run(directive);
        cases.push(CoreCase {
            line: line + 1,
            directive: directive_name,
            outcome,
        });
    }
    Ok(cases)
}

/// A call that did not produce the requested values.
enum ExecFailure {
    /// The call trapped.
    Trap(Trap),
    /// Anything else: a decode/validate/instantiate/link failure, an unknown
    /// export, an argument or result this harness does not convert, and so on.
    Other(String),
}

impl From<RuntimeError> for ExecFailure {
    fn from(error: RuntimeError) -> Self {
        match error {
            RuntimeError::Trap(trap) => ExecFailure::Trap(trap),
            other => ExecFailure::Other(other.to_string()),
        }
    }
}

struct Runner {
    engine: Engine,
    /// Every module instantiated so far, in order.
    modules: Vec<Instance>,
    /// Every name a module has been given, either its own `$id` or a
    /// `register`ed import name, mapped to its index in `modules`.
    named: HashMap<String, usize>,
    /// The most recently instantiated module, which an unqualified `invoke`
    /// or `assert_return` targets.
    current: Option<usize>,
}

impl Runner {
    fn run(&mut self, directive: WastDirective<'_>) -> (&'static str, CoreOutcome) {
        match directive {
            WastDirective::Module(mut quote) => {
                let id = quote.name().map(|id| id.name().to_string());
                let outcome = match quote.encode() {
                    Err(error) => {
                        CoreOutcome::Failed(format!("could not encode module text: {error}"))
                    }
                    Ok(bytes) => match self.engine.instantiate_bytes(&bytes) {
                        Ok(instance) => {
                            let index = self.modules.len();
                            self.modules.push(instance);
                            self.current = Some(index);
                            if let Some(id) = id {
                                self.named.insert(id, index);
                            }
                            CoreOutcome::Instantiated
                        }
                        Err(error) => CoreOutcome::Failed(format!("instantiation failed: {error}")),
                    },
                };
                ("module", outcome)
            }
            WastDirective::Invoke(invoke) => {
                let module = invoke.module.map(|id| id.name().to_string());
                let outcome = match self.invoke(module.as_deref(), invoke.name, &invoke.args) {
                    Ok(_) => CoreOutcome::Returned,
                    Err(ExecFailure::Trap(trap)) => {
                        CoreOutcome::Failed(format!("invoke trapped: {trap}"))
                    }
                    Err(ExecFailure::Other(reason)) => CoreOutcome::Failed(reason),
                };
                ("invoke", outcome)
            }
            WastDirective::AssertReturn { exec, results, .. } => {
                let outcome = match self.execute(exec) {
                    Ok(actual) => match check_results(&actual, &results) {
                        Ok(()) => CoreOutcome::Returned,
                        Err(reason) => CoreOutcome::Failed(reason),
                    },
                    Err(ExecFailure::Trap(trap)) => {
                        CoreOutcome::Failed(format!("expected a return, got a trap: {trap}"))
                    }
                    Err(ExecFailure::Other(reason)) => CoreOutcome::Failed(reason),
                };
                ("assert_return", outcome)
            }
            WastDirective::AssertTrap { exec, .. } => {
                let outcome = match self.execute(exec) {
                    Ok(values) => {
                        CoreOutcome::Failed(format!("expected a trap, got a return: {values:?}"))
                    }
                    Err(ExecFailure::Trap(_)) => CoreOutcome::Trapped,
                    Err(ExecFailure::Other(reason)) => CoreOutcome::Failed(reason),
                };
                ("assert_trap", outcome)
            }
            WastDirective::AssertExhaustion { call, .. } => {
                let module = call.module.map(|id| id.name().to_string());
                let outcome = match self.invoke(module.as_deref(), call.name, &call.args) {
                    Ok(values) => CoreOutcome::Failed(format!(
                        "expected exhaustion, got a return: {values:?}"
                    )),
                    Err(ExecFailure::Trap(_)) => CoreOutcome::Trapped,
                    Err(ExecFailure::Other(reason)) => CoreOutcome::Failed(reason),
                };
                ("assert_exhaustion", outcome)
            }
            WastDirective::Register { name, module, .. } => {
                let index = match module {
                    Some(id) => self.named.get(id.name()).copied(),
                    None => self.current,
                };
                let outcome = match index {
                    Some(index) => {
                        // Recording the name alone would leave a later module's
                        // import unresolved, because imports are resolved through
                        // the engine's `Linker` and nothing was put there. Wiring
                        // the instance's exports in is what makes `register` mean
                        // anything: the next module that imports `name` links
                        // against this one, sharing the same store memory, table,
                        // and globals rather than getting a copy.
                        let linked = self
                            .engine
                            .linker_mut()
                            .define_instance(name, &self.modules[index]);
                        if let Err(error) = linked {
                            CoreOutcome::Failed(format!("register failed: {error}"))
                        } else {
                            self.named.insert(name.to_string(), index);
                            CoreOutcome::Instantiated
                        }
                    }
                    None => CoreOutcome::Failed("register: no module to register".to_string()),
                };
                ("register", outcome)
            }
            WastDirective::ModuleDefinition(_) => (
                "module_definition",
                CoreOutcome::Skipped("module_definition"),
            ),
            WastDirective::ModuleInstance { .. } => {
                ("module_instance", CoreOutcome::Skipped("module_instance"))
            }
            WastDirective::AssertMalformed { .. } => (
                "assert_malformed",
                CoreOutcome::Skipped("text-format parse-failure checks are not run here"),
            ),
            WastDirective::AssertMalformedCustom { .. } => (
                "assert_malformed_custom",
                CoreOutcome::Skipped("assert_malformed_custom"),
            ),
            WastDirective::AssertInvalid { .. } => {
                ("assert_invalid", CoreOutcome::Skipped("assert_invalid"))
            }
            WastDirective::AssertInvalidCustom { .. } => (
                "assert_invalid_custom",
                CoreOutcome::Skipped("assert_invalid_custom"),
            ),
            WastDirective::AssertUnlinkable { .. } => (
                "assert_unlinkable",
                CoreOutcome::Skipped("assert_unlinkable"),
            ),
            WastDirective::AssertException { .. } => {
                ("assert_exception", CoreOutcome::Skipped("assert_exception"))
            }
            WastDirective::AssertSuspension { .. } => (
                "assert_suspension",
                CoreOutcome::Skipped("assert_suspension"),
            ),
            WastDirective::Thread(_) => ("thread", CoreOutcome::Skipped("thread")),
            WastDirective::Wait { .. } => ("wait", CoreOutcome::Skipped("wait")),
        }
    }

    fn resolve(&mut self, module: Option<&str>) -> Result<&mut Instance, ExecFailure> {
        let index = match module {
            Some(name) => self
                .named
                .get(name)
                .copied()
                .ok_or_else(|| ExecFailure::Other(format!("unknown module ${name}")))?,
            None => self
                .current
                .ok_or_else(|| ExecFailure::Other("no module has been instantiated yet".into()))?,
        };
        self.modules
            .get_mut(index)
            .ok_or_else(|| ExecFailure::Other("module index out of range".into()))
    }

    fn invoke(
        &mut self,
        module: Option<&str>,
        name: &str,
        args: &[WastArg<'_>],
    ) -> Result<Vec<Value>, ExecFailure> {
        let values = to_values(args).map_err(ExecFailure::Other)?;
        let instance = self.resolve(module)?;
        instance.call(name, values).map_err(ExecFailure::from)
    }

    fn execute(&mut self, exec: WastExecute<'_>) -> Result<Vec<Value>, ExecFailure> {
        match exec {
            WastExecute::Invoke(invoke) => {
                let module = invoke.module.map(|id| id.name().to_string());
                self.invoke(module.as_deref(), invoke.name, &invoke.args)
            }
            WastExecute::Wat(_) => Err(ExecFailure::Other(
                "inline-module execution is not supported".into(),
            )),
            WastExecute::Get { .. } => Err(ExecFailure::Other(
                "global `get` execution is not supported".into(),
            )),
        }
    }
}

fn to_values(args: &[WastArg<'_>]) -> Result<Vec<Value>, String> {
    args.iter().map(to_value).collect()
}

fn to_value(arg: &WastArg<'_>) -> Result<Value, String> {
    let WastArg::Core(core) = arg else {
        return Err("component-model arguments are not supported".to_string());
    };
    match core {
        WastArgCore::I32(value) => Ok(Value::I32(*value)),
        WastArgCore::I64(value) => Ok(Value::I64(*value)),
        WastArgCore::F32(value) => Ok(Value::F32(value.bits)),
        WastArgCore::F64(value) => Ok(Value::F64(value.bits)),
        other => Err(format!("unsupported argument value: {other:?}")),
    }
}

fn check_results(actual: &[Value], expected: &[WastRet<'_>]) -> Result<(), String> {
    if actual.len() != expected.len() {
        return Err(format!(
            "expected {} result value(s), got {}: {actual:?}",
            expected.len(),
            actual.len()
        ));
    }
    for (index, (value, ret)) in actual.iter().zip(expected).enumerate() {
        let WastRet::Core(core_ret) = ret else {
            return Err(format!(
                "result {index}: component-model values are not supported"
            ));
        };
        check_one(value, core_ret).map_err(|reason| format!("result {index}: {reason}"))?;
    }
    Ok(())
}

fn check_one(value: &Value, expected: &WastRetCore<'_>) -> Result<(), String> {
    match expected {
        WastRetCore::I32(want) => match value {
            Value::I32(got) if got == want => Ok(()),
            _ => Err(format!("expected i32.const {want}, got {value:?}")),
        },
        WastRetCore::I64(want) => match value {
            Value::I64(got) if got == want => Ok(()),
            _ => Err(format!("expected i64.const {want}, got {value:?}")),
        },
        WastRetCore::F32(pattern) => match value {
            Value::F32(bits) => check_f32(*bits, pattern),
            _ => Err(format!("expected an f32 result, got {value:?}")),
        },
        WastRetCore::F64(pattern) => match value {
            Value::F64(bits) => check_f64(*bits, pattern),
            _ => Err(format!("expected an f64 result, got {value:?}")),
        },
        WastRetCore::Either(alternatives) => {
            for alternative in alternatives {
                if check_one(value, alternative).is_ok() {
                    return Ok(());
                }
            }
            Err(format!(
                "{value:?} matched none of the {} accepted alternatives",
                alternatives.len()
            ))
        }
        other => Err(format!("unsupported expected result: {other:?}")),
    }
}

/// f32 bit-pattern checks per the spec's three result forms: an exact value
/// (including signed zero and any NaN payload), a canonical NaN (either sign,
/// the single canonical quiet payload), or an arithmetic NaN (either sign, the
/// quiet bit set, any other payload bit unspecified).
fn check_f32(bits: u32, pattern: &NanPattern<F32>) -> Result<(), String> {
    const QUIET_BIT_32: u32 = 0x0040_0000;
    const MANTISSA_32: u32 = 0x007f_ffff;
    const EXPONENT_32: u32 = 0x7f80_0000;
    const CANONICAL_NAN_32: u32 = 0x7fc0_0000;

    match pattern {
        NanPattern::CanonicalNan => {
            if bits & 0x7fff_ffff == CANONICAL_NAN_32 {
                Ok(())
            } else {
                Err(format!("expected a canonical NaN, got {bits:#010x}"))
            }
        }
        NanPattern::ArithmeticNan => {
            let is_nan = bits & EXPONENT_32 == EXPONENT_32 && bits & MANTISSA_32 != 0;
            if is_nan && bits & QUIET_BIT_32 != 0 {
                Ok(())
            } else {
                Err(format!("expected an arithmetic NaN, got {bits:#010x}"))
            }
        }
        NanPattern::Value(expected) => {
            if bits == expected.bits {
                Ok(())
            } else {
                Err(format!(
                    "expected {:#010x}, got {bits:#010x}",
                    expected.bits
                ))
            }
        }
    }
}

/// The f64 counterpart of [`check_f32`], with the equivalent 64-bit masks.
fn check_f64(bits: u64, pattern: &NanPattern<F64>) -> Result<(), String> {
    const QUIET_BIT_64: u64 = 0x0008_0000_0000_0000;
    const MANTISSA_64: u64 = 0x000f_ffff_ffff_ffff;
    const EXPONENT_64: u64 = 0x7ff0_0000_0000_0000;
    const CANONICAL_NAN_64: u64 = 0x7ff8_0000_0000_0000;

    match pattern {
        NanPattern::CanonicalNan => {
            if bits & 0x7fff_ffff_ffff_ffff == CANONICAL_NAN_64 {
                Ok(())
            } else {
                Err(format!("expected a canonical NaN, got {bits:#018x}"))
            }
        }
        NanPattern::ArithmeticNan => {
            let is_nan = bits & EXPONENT_64 == EXPONENT_64 && bits & MANTISSA_64 != 0;
            if is_nan && bits & QUIET_BIT_64 != 0 {
                Ok(())
            } else {
                Err(format!("expected an arithmetic NaN, got {bits:#018x}"))
            }
        }
        NanPattern::Value(expected) => {
            if bits == expected.bits {
                Ok(())
            } else {
                Err(format!(
                    "expected {:#018x}, got {bits:#018x}",
                    expected.bits
                ))
            }
        }
    }
}
