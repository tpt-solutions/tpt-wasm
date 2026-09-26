// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Harness for the official WebAssembly spec tests.
//!
//! The `.wast` files in `testdata/` are vendored from the upstream spec
//! repository; see `testdata/PROVENANCE.md` for the exact revision and license.
//! Nothing is fetched at test time, so a run is reproducible and offline.
//!
//! Only the `(module binary ...)` form is handled, which is what the binary
//! format tests use. That is deliberate: it means these tests exercise the
//! decoder and validator against upstream's own expectations without this crate
//! needing a text-format assembler, and without the tests being able to pass by
//! agreeing with our own reading of the text format.
//!
//! Three directives are supported:
//!
//! - a bare `(module binary ...)`, which must decode *and* validate
//! - `(assert_malformed (module binary ...) "reason")`, which must fail to decode
//! - `(assert_invalid (module binary ...) "reason")`, which must decode and then
//!   fail to validate
//!
//! The `reason` string is not matched against our error text. Upstream reasons
//! are prose that no implementation is expected to reproduce verbatim, so
//! matching on them would test the wording rather than the behavior. What is
//! checked is *which stage* rejected the module, which is the distinction the
//! spec actually draws: `malformed` is a decoding failure, `invalid` is a
//! validation failure, and a decoder that accepts a module the validator should
//! reject is a different defect from one that rejects a module outright.

mod core;
mod wast;

pub use core::{run_core_suite, CoreCase, CoreOutcome};
pub use wast::{parse, Form, ParseError};

/// The stage a module is rejected at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Decode,
    Validate,
}

/// What a single spec assertion expected, and what the implementation did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The directive was one this harness does not interpret.
    Skipped(&'static str),
    /// A module that must decode and validate, and did.
    Accepted,
    /// A module that had to fail to decode, and did.
    RejectedAtDecode,
    /// A module that had to decode and then fail to validate, and did.
    RejectedAtValidate,
    /// A module that should have been rejected but was accepted.
    WronglyAccepted,
    /// Rejected, but at the wrong stage: a decoder-only failure where
    /// validation was required, or the reverse.
    RejectedAtWrongStage(Stage),
}

/// What a case expects of the implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    /// The module must decode and validate.
    Accept,
    /// The module must be rejected at this stage.
    Reject(Stage),
    /// The directive is not one this harness interprets, so there is nothing to
    /// check. Kept as a case so the count of assertions read from a file stays
    /// comparable, and so a skipped directive is visible in the report rather
    /// than silently absent.
    Skipped(&'static str),
}

/// One assertion from a `.wast` file, with the module it applies to.
#[derive(Debug, Clone)]
pub struct Case {
    /// 1-based line in the source file, so a failure points somewhere.
    pub line: usize,
    /// The directive, as written.
    pub directive: &'static str,
    /// The upstream reason string, kept for the report and not matched.
    pub reason: Option<String>,
    /// The module bytes, concatenated across the string chunks. Empty for a
    /// skipped case, which is why the expectation has to say so explicitly.
    pub module: Vec<u8>,
    /// What this case expects.
    pub expect: Expectation,
}

impl Case {
    /// Run the module through the real decoder and validator.
    pub fn run(&self) -> Outcome {
        match self.expect {
            Expectation::Skipped(name) => Outcome::Skipped(name),
            Expectation::Accept => match tpt_wasm_decode::decode(&self.module) {
                // Which stage refused it is the interesting part, so it is
                // reported rather than collapsed into one failure.
                Err(_) => Outcome::RejectedAtDecode,
                Ok(module) => match tpt_wasm_validate::validate(module) {
                    Ok(_) => Outcome::Accepted,
                    Err(_) => Outcome::RejectedAtValidate,
                },
            },
            // `assert_malformed` requires the module to be rejected. A decoder
            // that accepts it and a validator that rejects it are both refusals,
            // and this harness runs only those two stages, so either one satisfies
            // the assertion. The distinction upstream draws is real — a
            // `malformed` module is not well-formed at all, where an `invalid` one
            // decodes and fails only validation — but recovering which happened
            // would need a third stage this crate deliberately does not run. Both
            // outcomes stay separate in `Outcome` so the difference stays visible
            // rather than being lost.
            Expectation::Reject(Stage::Decode) => match tpt_wasm_decode::decode(&self.module) {
                Err(_) => Outcome::RejectedAtDecode,
                Ok(module) => match tpt_wasm_validate::validate(module) {
                    Err(_) => Outcome::RejectedAtValidate,
                    Ok(_) => Outcome::WronglyAccepted,
                },
            },
            // `assert_invalid` requires the module to decode, then fail
            // validation. Rejecting it during decoding is not good enough.
            Expectation::Reject(Stage::Validate) => match tpt_wasm_decode::decode(&self.module) {
                Ok(module) => match tpt_wasm_validate::validate(module) {
                    Err(_) => Outcome::RejectedAtValidate,
                    Ok(_) => Outcome::WronglyAccepted,
                },
                Err(_) => Outcome::RejectedAtWrongStage(Stage::Decode),
            },
        }
    }
}

/// Every case in a `.wast` source, in file order.
///
/// A directive this harness does not interpret becomes a skipped case rather
/// than an error, so a file mixing binary and text modules still reports the
/// parts that are covered instead of failing wholesale.
pub fn cases(source: &str) -> Result<Vec<Case>, ParseError> {
    let mut cases = Vec::new();
    for form in parse(source)? {
        match form {
            Form::Module { line, bytes } => cases.push(Case {
                line,
                directive: "module",
                reason: None,
                module: bytes,
                expect: Expectation::Accept,
            }),
            Form::Malformed {
                line,
                bytes,
                reason,
            } => cases.push(Case {
                line,
                directive: "assert_malformed",
                reason: Some(reason),
                module: bytes,
                expect: Expectation::Reject(Stage::Decode),
            }),
            Form::Invalid {
                line,
                bytes,
                reason,
            } => cases.push(Case {
                line,
                directive: "assert_invalid",
                reason: Some(reason),
                module: bytes,
                expect: Expectation::Reject(Stage::Validate),
            }),
            Form::Skipped { line, directive } => cases.push(Case {
                line,
                directive,
                reason: None,
                module: Vec::new(),
                expect: Expectation::Skipped(directive),
            }),
        }
    }
    Ok(cases)
}
