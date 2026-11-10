// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A differential oracle over arbitrary Wasm bytes.
//!
//! Micro is the golden machine, so every other backend is judged against it. For a
//! byte string that decodes, validates, and imports nothing, the oracle instantiates
//! it on Micro, the baseline, and the optimizing engine, calls every function export
//! with zero arguments, and requires the same answers. Two results agree when the
//! returned values are bit-identical or the traps are the same variant.
//!
//! ## What is out of scope, and why
//!
//! * Bytes that do not decode or do not validate. Rejecting them is the decoder's and
//!   validator's job, and they are checked elsewhere; there is nothing to compare.
//! * Modules with imports. Their behaviour depends on host callbacks the oracle does
//!   not supply.
//! * Exports whose parameters are not numeric. A zero value is only defined for the
//!   four numeric types.
//! * A backend that refuses a module as `UnsupportedFeature`. That is a documented
//!   limit of the backend, not a disagreement about what the module means.
//!
//! Anything else is a divergence and is reported as an error string. A panic is
//! not caught here on purpose: a fuzzer reports panics as crashes, and catching them
//! would turn a crash into a quiet pass.

use tpt_wasm_format::{ExportDesc, Module};
use tpt_wasm_runtime::{Config, Engine, EngineMode, Instance, RuntimeError};
use tpt_wasm_types::{ResourceLimits, Trap, Value, ValueType};

/// Instructions allowed per call. Keeps a non-terminating input cheap to run. Both
/// backends charge the budget at the same instruction, so the trap is compared too.
pub const STEP_BUDGET: u64 = 50_000;

/// How one call ended, in a form two backends can be compared on.
#[derive(Debug, Clone, PartialEq)]
pub enum CallOutcome {
    Returned(Vec<Value>),
    Trapped(Trap),
    /// A runtime error that is not a trap, compared by its message.
    Refused(String),
}

impl CallOutcome {
    fn of(result: Result<Vec<Value>, RuntimeError>) -> Self {
        match result {
            Ok(values) => Self::Returned(values),
            Err(RuntimeError::Trap(trap)) => Self::Trapped(trap),
            Err(other) => Self::Refused(format!("{other:?}")),
        }
    }
}

/// Whether two outcomes from different backends agree.
///
/// `Refused` on both sides agrees on the fact of refusal and not the wording, since
/// the two backends word their own errors differently. Values are compared by their
/// bits through `Value`'s equality, so a NaN payload is part of the answer.
pub fn agree(golden: &CallOutcome, other: &CallOutcome) -> bool {
    match (golden, other) {
        (CallOutcome::Returned(a), CallOutcome::Returned(b)) => a == b,
        (CallOutcome::Trapped(a), CallOutcome::Trapped(b)) => a == b,
        (CallOutcome::Refused(_), CallOutcome::Refused(_)) => true,
        _ => false,
    }
}

fn config(mode: EngineMode) -> Config {
    Config {
        engine_mode: mode,
        limits: ResourceLimits {
            max_execution_steps: Some(STEP_BUDGET),
            ..ResourceLimits::default()
        },
        ..Config::default()
    }
}

/// The zero value of a numeric type, or `None` for a type the oracle does not call with.
fn zero(value_type: ValueType) -> Option<Value> {
    match value_type {
        ValueType::I32 => Some(Value::I32(0)),
        ValueType::I64 => Some(Value::I64(0)),
        ValueType::F32 => Some(Value::F32(0)),
        ValueType::F64 => Some(Value::F64(0)),
        _ => None,
    }
}

/// Each function export with the zero arguments it is called with, sorted by name so
/// every backend sees the calls in the same order. `None` when a parameter is not
/// numeric, so the module is out of scope.
fn entry_points(module: &Module) -> Option<Vec<(String, Vec<Value>)>> {
    let mut calls = Vec::new();
    for export in &module.exports {
        let ExportDesc::Function(index) = export.desc else {
            continue;
        };
        let function = module.functions.get(index as usize)?;
        let signature = module.types.get(function.type_index as usize)?;
        let args = signature
            .params
            .0
            .iter()
            .map(|t| zero(*t))
            .collect::<Option<Vec<_>>>()?;
        calls.push((export.name.clone(), args));
    }
    calls.sort_by(|a, b| a.0.cmp(&b.0));
    Some(calls)
}

/// What the oracle did with one input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Out of scope, or refused by every backend, so nothing was compared.
    OutOfScope,
    /// The backends agreed on `calls` calls. A check that reports this is the one
    /// that proved something.
    Agreed { calls: usize },
}

/// Run the oracle on `bytes`. `Ok(())` means the backends agreed, or the input was
/// out of scope. `Err` describes the first divergence found.
pub fn differential_check(bytes: &[u8]) -> Result<(), String> {
    check(bytes).map(|_| ())
}

/// As [`differential_check`], reporting what was compared, so a caller can tell an
/// agreement from an input that was never run.
pub fn check(bytes: &[u8]) -> Result<Verdict, String> {
    let Ok(module) = tpt_wasm_decode::decode(bytes) else {
        return Ok(Verdict::OutOfScope);
    };
    if tpt_wasm_validate::validate(module.clone()).is_err() || !module.imports.is_empty() {
        return Ok(Verdict::OutOfScope);
    }
    let Some(calls) = entry_points(&module) else {
        return Ok(Verdict::OutOfScope);
    };

    let golden_engine = Engine::new(config(EngineMode::Micro))
        .map_err(|error| format!("micro refused to start: {error:?}"))?;
    let golden = golden_engine.instantiate_bytes(bytes);

    let mut others: Vec<(EngineMode, Instance)> = Vec::new();
    for mode in [EngineMode::Baseline, EngineMode::Optimizing] {
        let engine = Engine::new(config(mode)).map_err(|error| format!("{mode:?}: {error:?}"))?;
        match (&golden, engine.instantiate_bytes(bytes)) {
            (Err(_), Err(_)) => {}
            (Ok(_), Err(RuntimeError::UnsupportedFeature(_))) => {}
            (Err(error), Ok(_)) => {
                return Err(format!(
                    "micro refused the module ({error:?}) but {mode:?} instantiated it"
                ));
            }
            (Ok(_), Err(error)) => {
                return Err(format!(
                    "{mode:?} refused a module micro accepted: {error:?}"
                ));
            }
            (Ok(_), Ok(instance)) => others.push((mode, instance)),
        }
    }
    let Ok(mut golden) = golden else {
        return Ok(Verdict::OutOfScope);
    };

    let mut compared = 0;
    for (name, args) in calls {
        let expected = CallOutcome::of(golden.call(&name, args.clone()));
        for (mode, instance) in &mut others {
            let actual = CallOutcome::of(instance.call(&name, args.clone()));
            if !agree(&expected, &actual) {
                return Err(format!(
                    "export `{name}`: micro {expected:?}, {mode:?} {actual:?}"
                ));
            }
        }
        compared += 1;
    }
    Ok(if compared == 0 {
        Verdict::OutOfScope
    } else {
        Verdict::Agreed { calls: compared }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `add(i32, i32) -> i32`, which returns the sum of its arguments.
    const ADD: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
        0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // type: (i32, i32) -> i32
        0x03, 0x02, 0x01, 0x00, // function 0 has type 0
        0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00, // export "add"
        0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b, // body
    ];

    /// `count() -> i32`: a loop that adds one until the local reaches 100.
    const COUNT: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
        0x03, 0x02, 0x01, 0x00, // function 0 has type 0
        0x07, 0x09, 0x01, 0x05, 0x63, 0x6f, 0x75, 0x6e, 0x74, 0x00, 0x00, // export "count"
        0x0a, 0x18, 0x01, 0x16, 0x01, 0x01, 0x7f, // body: one i32 local
        0x03, 0x40, // loop
        0x20, 0x00, 0x41, 0x01, 0x6a, 0x22, 0x00, // local.tee 0 (local.get 0 + 1)
        0x41, 0xe4, 0x00, 0x48, // i32.const 100; i32.lt_s
        0x0d, 0x00, // br_if 0
        0x0b, // end of loop
        0x20, 0x00, 0x0b, // local.get 0; end
    ];

    /// A small deterministic generator, so a failing mutant reproduces from its seed.
    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    /// One mutation of `input`: a bit flip, a byte overwrite, an insertion, a
    /// deletion, or a truncation. Many of these break the module, which is the point
    /// of mutating: the decoder and validator are exercised on the way.
    fn mutate(input: &[u8], rng: &mut Xorshift) -> Vec<u8> {
        let mut out = input.to_vec();
        if out.is_empty() {
            return vec![rng.next() as u8];
        }
        match rng.below(5) {
            0 => {
                let at = rng.below(out.len());
                out[at] ^= 1 << rng.below(8);
            }
            1 => {
                let at = rng.below(out.len());
                out[at] = rng.next() as u8;
            }
            2 => {
                let at = rng.below(out.len() + 1);
                out.insert(at, rng.next() as u8);
            }
            3 => {
                let at = rng.below(out.len());
                out.remove(at);
            }
            _ => {
                let keep = rng.below(out.len());
                out.truncate(keep);
            }
        }
        out
    }

    #[test]
    fn the_seed_modules_are_in_scope_and_agree() {
        for (name, seed) in [("add", ADD), ("count", COUNT)] {
            differential_check(seed)
                .unwrap_or_else(|divergence| panic!("seed `{name}` diverged: {divergence}"));
            // In scope means the oracle really compared something: the export list
            // is non-empty and every parameter is numeric.
            let module = tpt_wasm_decode::decode(seed).expect("the seed must decode");
            assert!(
                entry_points(&module).is_some_and(|calls| !calls.is_empty()),
                "seed `{name}` has no comparable export, so it proves nothing"
            );
        }
    }

    /// Every module reachable from the seeds by substituting an operator or a
    /// constant. Each one is valid by construction, so each one must be compared.
    /// Byte-level mutation mostly breaks modules and so mostly tests the decoder; these
    /// test the backends.
    fn semantic_variants(seed: &[u8]) -> Vec<Vec<u8>> {
        // Every i32 binary operator: add through rotr. All take two i32s and return one.
        // clz and ctz are unary and are deliberately absent.
        const I32_BINARY: [u8; 15] = [
            0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77,
            0x78,
        ];
        // The i32 comparisons: eq through ge_u.
        const I32_COMPARE: [u8; 10] = [0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f];
        // i32.const with a three-byte encoding, so every substitution keeps the section
        // sizes that were written around the original constant. The values are zero,
        // -129, 128, -65, and 100 (the seed's own bound). A padded encoding such as
        // `0x80 0x00` is valid LEB128 and decodes to zero.
        const CONSTANTS: [&[u8]; 5] = [
            &[0x41, 0x80, 0x00],
            &[0x41, 0xff, 0x7e],
            &[0x41, 0x80, 0x01],
            &[0x41, 0xbf, 0x7f],
            &[0x41, 0xe4, 0x00],
        ];

        fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        }

        let mut out = Vec::new();
        if seed == ADD {
            let at = find(seed, &[0x20, 0x01, 0x6a]).expect("ADD contains i32.add") + 2;
            for op in I32_BINARY {
                let mut variant = seed.to_vec();
                variant[at] = op;
                out.push(variant);
            }
        } else if seed == COUNT {
            // The loop bound is `i32.const 100; i32.lt_s`. Replace the bound and the
            // comparison together, so the loop still makes a decision.
            let at = find(seed, &[0x41, 0xe4, 0x00, 0x48]).expect("COUNT contains the bound");
            for constant in CONSTANTS {
                for op in I32_COMPARE {
                    let mut variant = seed[..at].to_vec();
                    variant.extend_from_slice(constant);
                    variant.extend_from_slice(&seed[at + 3..]);
                    variant[at + constant.len()] = op;
                    out.push(variant);
                }
            }
        }
        out
    }

    #[test]
    fn every_semantic_variant_is_compared_and_agrees() {
        for seed in [ADD, COUNT] {
            let variants = semantic_variants(seed);
            assert!(!variants.is_empty());
            for variant in variants {
                match check(&variant) {
                    Ok(Verdict::Agreed { calls }) => assert!(calls > 0),
                    other => {
                        panic!("a valid variant was not compared: {other:?} for {variant:02x?}")
                    }
                }
            }
        }
    }

    #[test]
    fn mutants_of_the_seeds_never_diverge() {
        let mut corpus: Vec<Vec<u8>> = vec![ADD.to_vec(), COUNT.to_vec()];
        corpus.extend(semantic_variants(ADD));
        corpus.extend(semantic_variants(COUNT));
        let mut rng = Xorshift(0x9e37_79b9_7f4a_7c15);
        let mut agreed = 0;
        let mut calls = 0;
        for case in 0..6_000 {
            let base = &corpus[rng.below(corpus.len())];
            let mutant = mutate(base, &mut rng);
            let verdict = check(&mutant).unwrap_or_else(|divergence| {
                panic!(
                    "mutant {case}: {divergence}
bytes: {mutant:02x?}"
                )
            });
            if let Verdict::Agreed { calls: n } = verdict {
                agreed += 1;
                calls += n;
            }
        }
        // Byte-level edits mostly break the module, so only a few of these reach the
        // backends (about one in thirty). The assertion keeps that share from falling
        // to zero, which would make the test pass without running anything. The
        // comparison itself is carried by `every_semantic_variant_is_compared_and_agrees`,
        // which requires every valid variant to be run.
        assert!(
            agreed >= 100,
            "only {agreed} of 6000 mutants were compared; the oracle is mostly vacuous"
        );
        assert!(
            calls >= agreed,
            "every compared mutant makes at least one call"
        );
    }

    #[test]
    fn a_disagreement_is_reported_and_a_refusal_on_both_sides_is_not() {
        let returned = CallOutcome::Returned(vec![Value::I32(3)]);
        assert!(!agree(
            &returned,
            &CallOutcome::Returned(vec![Value::I32(4)])
        ));
        assert!(!agree(&returned, &CallOutcome::Trapped(Trap::Unreachable)));
        assert!(!agree(
            &CallOutcome::Trapped(Trap::Unreachable),
            &CallOutcome::Trapped(Trap::IntegerDivisionByZero)
        ));
        assert!(agree(
            &CallOutcome::Trapped(Trap::StepsExhausted),
            &CallOutcome::Trapped(Trap::StepsExhausted)
        ));
        assert!(agree(
            &CallOutcome::Refused("a".into()),
            &CallOutcome::Refused("b".into())
        ));
        // NaN payloads are part of the answer: two NaNs with different bits differ.
        assert!(!agree(
            &CallOutcome::Returned(vec![Value::F32(0x7fc0_0000)]),
            &CallOutcome::Returned(vec![Value::F32(0x7fc0_0001)])
        ));
    }
}
