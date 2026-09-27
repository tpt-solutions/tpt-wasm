// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The optimizing engine: compile through the optimizer, execute the result.
//!
//! The optimizing engine is the same execution path as the baseline, reached
//! through a different compilation. That is deliberate and it is the whole design:
//! `EngineMode::Optimizing` runs
//!
//! ```text
//! Wasm -> validate -> IR -> optimize -> verify -> baseline lowering -> execute
//! ```
//!
//! where `EngineMode::Baseline` runs the same chain with the optimizer's passes
//! disabled. Sharing the executor is what makes the two modes *comparable* rather
//! than merely similar -- there is one implementation of the semantics in the
//! compiled path, so a difference between the modes is a difference in
//! optimization and nothing else.
//!
//! What the optimizing engine is not yet: a native-code engine. The portable
//! executor interprets the optimized IR, so this mode currently buys a smaller
//! program rather than faster execution. The native backends (`tpt-wasm-jit` and
//! `tpt-wasm-aot`) are where the compiled form becomes machine code, and they
//! consume the same optimized IR this mode produces -- which is why the boundary
//! is drawn at the IR rather than at the executor.
//!
//! ## What optimization may change
//!
//! Nothing observable. The optimizer runs under the contract in `spec.txt`
//! section 67 -- values, control flow, traps, memory, global, table, and host
//! effects, and the order of all of them -- and the IR verifier re-checks the
//! module after every pass. An optimization that could not be shown to preserve
//! those is a compiler bug, and it surfaces as one rather than as a divergence.

use tpt_wasm_ir::IrModule;

use crate::{Config, EngineMode, RuntimeError};

/// Compile a module for the optimizing engine.
///
/// `None` for the other modes, so the store-backed interpreter path is used
/// unchanged. A module the optimizer cannot represent is reported here, at
/// instantiation, rather than failing later -- an instance is never left
/// half-usable.
pub fn compile(
    module: &tpt_wasm_validate::ValidatedModule,
    config: &Config,
) -> Result<Option<IrModule>, RuntimeError> {
    if config.engine_mode != EngineMode::Optimizing {
        return Ok(None);
    }
    let verified = tpt_wasm_ir::lower_and_verify(module).map_err(|error| {
        // The reason is carried through rather than replaced by a stage name, for
        // the same reason the baseline path does it: a module refused here would
        // otherwise be reported as a bare "optimizing lowering", which names
        // neither what is unsupported nor where.
        let reason = match error {
            tpt_wasm_ir::LowerAndVerifyError::Lowering(error) => format!("lowering: {error}"),
            tpt_wasm_ir::LowerAndVerifyError::Verification(error) => {
                format!("verification: {error}")
            }
        };
        RuntimeError::UnsupportedFeature(Box::leak(format!("optimizing {reason}").into_boxed_str()))
    })?;

    let level = level_for(config);
    let outcome = tpt_wasm_opt::optimize(verified.module(), level).map_err(|error| {
        // An `OptimizationError::Verification` is a compiler bug, not a user
        // error: the input verified and every pass preserves what the verifier
        // checks. It is surfaced with its detail rather than collapsed, because
        // "the optimizer produced bad IR" is the single most important thing an
        // embedder running this mode could learn.
        RuntimeError::UnsupportedFeature(Box::leak(format!("optimizing: {error}").into_boxed_str()))
    })?;

    // The optimization contract is checked on every compilation, not only in
    // tests. It is a linear scan over each function's effects, which is cheap
    // next to the passes themselves, and it means a pass that dropped a store or
    // reordered a host call fails at instantiation rather than at the call that
    // happened to notice.
    tpt_wasm_opt::contract::verify_contract(verified.module(), outcome.module()).map_err(
        |violation| {
            RuntimeError::UnsupportedFeature(Box::leak(
                format!("optimization contract: {violation}").into_boxed_str(),
            ))
        },
    )?;

    Ok(Some(outcome.into_parts().0))
}

/// The optimization level this configuration asks for.
///
/// `Full` when the embedder asked for deterministic or profiled execution is *not*
/// implied here: those are separate `Config` flags and this function deliberately
/// reads only the mode. An embedder that wants the more aggressive pipeline asks
/// for it explicitly, because `Full` costs more at instantiation and buys
/// nothing for a module that is called once.
fn level_for(config: &Config) -> tpt_wasm_opt::OptimizationLevel {
    match config.engine_mode {
        EngineMode::Optimizing => tpt_wasm_opt::OptimizationLevel::Full,
        _ => tpt_wasm_opt::OptimizationLevel::None,
    }
}
