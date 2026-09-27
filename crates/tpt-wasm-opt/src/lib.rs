// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Optimization passes over the TPT IR.
//!
//! The Micro interpreter is the golden machine. An optimization is correct only
//! if the optimized module is indistinguishable from the unoptimized one in
//! *every* observable respect: values, control flow, traps, memory effects,
//! global effects, table effects, host effects, and the order of all of them.
//! `spec.txt` section 67 is the contract; [`contract`] is where it is checked.
//!
//! The pipeline runs over `IrModule` and produces a new `IrModule`. It never
//! mutates its input, so a caller can keep the original for differential
//! comparison, which is how the contract is tested.
//!
//! ## What every pass has to respect
//!
//! Three properties of this IR constrain every pass, and all three are easy to
//! get wrong:
//!
//! * **A local is storage, not an SSA name.** One `ValueId` is re-assigned by
//!   each `local.set`, so a value's definition does not pin its content for the
//!   whole function. A pass that treats a local like an SSA name will happily
//!   forward a stale constant across a store.
//! * **A dead result is not a dead instruction.** `i32.div_s` with an unused
//!   result still traps, and a `call` to an imported function still reaches the
//!   host. [`analysis::is_removable`] is the one place that decides this.
//! * **Instruction order is observable.** A trap's *position* matters as much as
//!   its kind, so a pass may not hoist an access above a store that could fault,
//!   nor sink one below a store that would change what it reads.

use std::fmt;

use tpt_wasm_ir::{verify_module, IrModule, VerificationError};

pub mod analysis;
pub mod const_fold;
pub mod const_prop;
pub mod contract;
pub mod dce;
pub mod graph;
pub mod lvn;
pub mod memory;
pub mod numeric;
pub mod regalloc;
pub mod sched;
#[cfg(test)]
mod tests;

/// How hard the optimizer should try.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptimizationLevel {
    /// Run no passes. The module is returned unchanged, which is what keeps the
    /// unoptimized path available for differential comparison.
    None,
    /// Local, cheap, and always profitable: folding, propagation, dead code,
    /// control-flow cleanup, redundancy elimination, and memory forwarding.
    #[default]
    Fast,
    /// Everything, including the passes whose cost scales with the function and
    /// which are only worth it when the result will be executed many times.
    Full,
}

/// The outcome of optimizing a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationOutcome {
    module: IrModule,
    report: OptimizationReport,
}

impl OptimizationOutcome {
    pub fn module(&self) -> &IrModule {
        &self.module
    }

    pub fn report(&self) -> &OptimizationReport {
        &self.report
    }

    pub fn into_parts(self) -> (IrModule, OptimizationReport) {
        (self.module, self.report)
    }
}

/// What each pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OptimizationReport {
    /// Per-pass counts, in the order the passes ran.
    pub passes: Vec<PassReport>,
    pub instructions_before: usize,
    pub instructions_after: usize,
    pub blocks_before: usize,
    pub blocks_after: usize,
}

impl OptimizationReport {
    /// Instructions the pipeline removed in total.
    pub fn instructions_removed(&self) -> usize {
        self.instructions_before
            .saturating_sub(self.instructions_after)
    }

    /// Blocks the pipeline removed in total.
    pub fn blocks_removed(&self) -> usize {
        self.blocks_before.saturating_sub(self.blocks_after)
    }
}

/// One pass's contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassReport {
    pub name: &'static str,
    pub instructions_removed: usize,
    pub blocks_removed: usize,
    /// Rewrites that were not removals: folded operations, forwarded loads,
    /// removed bounds checks, moved instructions.
    pub rewrites: usize,
}

/// Something the optimizer could not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptimizationError {
    /// A pass produced IR the structural verifier rejects.
    ///
    /// This is a compiler bug, not a user error: the input was verified, and
    /// every pass is written to preserve what the verifier checks. It is
    /// reported rather than papered over, because a silently miscompiled module
    /// is exactly the failure this project exists to make impossible.
    Verification {
        pass: &'static str,
        error: Box<VerificationError>,
    },
}

impl fmt::Display for OptimizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Verification { pass, error } => {
                write!(f, "the {pass} pass produced unverifiable IR: {error}")
            }
        }
    }
}

impl std::error::Error for OptimizationError {}

/// What one pass did to the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PassOutcome {
    pub instructions_removed: usize,
    pub blocks_removed: usize,
    pub rewrites: usize,
}

impl PassOutcome {
    pub fn is_empty(&self) -> bool {
        self.instructions_removed == 0 && self.blocks_removed == 0 && self.rewrites == 0
    }
}

/// Optimize a module, re-verifying after every pass.
///
/// The per-pass re-verification is the point of this function. It costs a linear
/// scan per pass, which is negligible next to the passes themselves, and it
/// turns "the optimizer preserves the IR's structural invariants" from a claim
/// argued in review into a property checked on every compilation. A pass that
/// breaks dominance, redefines a value, or leaves a dangling block fails here
/// with its own name attached, rather than producing a module that misbehaves
/// somewhere later.
pub fn optimize(
    module: &IrModule,
    level: OptimizationLevel,
) -> Result<OptimizationOutcome, OptimizationError> {
    let (instructions_before, blocks_before) = size_of(module);
    if level == OptimizationLevel::None {
        return Ok(OptimizationOutcome {
            module: module.clone(),
            report: OptimizationReport {
                passes: Vec::new(),
                instructions_before,
                instructions_after: instructions_before,
                blocks_before,
                blocks_after: blocks_before,
            },
        });
    }

    let mut working = module.clone();
    let mut passes: Vec<PassReport> = Vec::new();
    let schedule = pipeline(level);

    // The pipeline is iterated to a fixed point rather than run once.
    //
    // A single sweep is not enough, and the reason is that the passes feed each
    // other: folding exposes constants that let propagation rewrite a local
    // read, which exposes a constant that lets the folder fold again. Running
    // each pass once would leave those cascades half-done.
    //
    // The *outer* loop is what "until nothing changes" means here. It is
    // important that this is not written as "stop when a pass does nothing":
    // several passes legitimately do nothing on a given sweep while a later one
    // still has work, and stopping there would silently disable the rest of the
    // pipeline. The condition is on the *round* as a whole.
    //
    // The bound is a backstop. Every round is monotone -- passes only remove or
    // simplify -- so the loop terminates; a rewrite that could grow the module
    // without bound would hit this instead of running forever.
    for round in 0..MAX_ROUNDS {
        let mut changed_this_round = false;
        for (name, pass) in &schedule {
            let outcome = pass(&mut working);

            // Every pass that removes an instruction orphans the value
            // declaration that instruction satisfied, and both the IR verifier and
            // the backend reject a declared-but-undefined value. Pruning here
            // rather than in each pass means no pass can forget to do it, and it
            // happens *before* verification so the verifier sees exactly what the
            // next pass will receive.
            let mut pruned = 0;
            for function in &mut working.functions {
                pruned += analysis::prune_garbage(function);
            }

            if let Err(error) = verify_module(&working) {
                return Err(OptimizationError::Verification {
                    pass: name,
                    error: Box::new(error),
                });
            }
            if !outcome.is_empty() || pruned > 0 {
                changed_this_round = true;
            }
            passes.push(PassReport {
                name,
                instructions_removed: outcome.instructions_removed,
                blocks_removed: outcome.blocks_removed,
                rewrites: outcome.rewrites + pruned,
            });
        }
        if !changed_this_round {
            break;
        }
        // The bound is advisory; a pipeline that has not converged by then is
        // still correct, just not fully optimized.
        let _ = round;
    }

    // Declarations are pruned once, at the end, rather than after every pass.
    //
    // Removing an instruction orphans the `IrValue` that instruction satisfied,
    // and both the IR verifier and the backend reject a declared-but-undefined
    // value. Pruning *mid-pipeline* is wrong, though: an intermediate state is not
    // a module anyone compiles, and a value defined in a block a later pass will
    // delete looks undefined to the pruner even though the final module is fine.
    // Pruning early therefore removes declarations the final module still needs,
    // and the backend then reports a value it cannot find -- an error whose cause
    // is two passes away from where it happened.
    //
    // So the rule is: run every pass, then prune once, then verify. The verifier
    // then sees exactly the module a backend will be handed.
    let mut pruned = 0;
    for function in &mut working.functions {
        pruned += analysis::prune_declarations(function);
    }
    if let Err(error) = verify_module(&working) {
        return Err(OptimizationError::Verification {
            pass: "declaration-pruning",
            error: Box::new(error),
        });
    }
    if let Some(last) = passes.last_mut() {
        last.rewrites += pruned;
    }

    let (instructions_after, blocks_after) = size_of(&working);
    Ok(OptimizationOutcome {
        module: working,
        report: OptimizationReport {
            passes,
            instructions_before,
            instructions_after,
            blocks_before,
            blocks_after,
        },
    })
}

/// One entry in the pass schedule: the pass and how to run it.
type PassFn = fn(&mut IrModule) -> PassOutcome;

/// A named pass, as the schedule records it and the report reports on it.
type ScheduledPass = (&'static str, PassFn);

/// How many times the pipeline is swept before giving up on convergence.
///
/// Every pass is monotone, so a correct pipeline converges in a handful of
/// rounds. This is a backstop against a future pass that is not, and hitting it
/// costs optimization rather than correctness.
const MAX_ROUNDS: usize = 8;

/// The pass pipeline for a level, in order.
///
/// The order is a dependency chain, not a preference:
///
/// 1. folding and propagation first, so every later pass sees constants;
/// 2. control-flow simplification next, so a later pass is not reasoning about
///    blocks that are about to disappear;
/// 3. dead code elimination, which needs the constants to know which
///    instructions cannot trap;
/// 4. redundancy elimination, which benefits from the dead code being gone;
/// 5. memory passes, which need stable addresses to forward and prove;
/// 6. scheduling last, because it reorders whatever survived everything else.
fn pipeline(level: OptimizationLevel) -> Vec<ScheduledPass> {
    let mut passes: Vec<ScheduledPass> = vec![
        ("constant-folding", const_fold::run),
        ("constant-propagation", const_prop::run),
        ("cfg-simplification", graph::run),
        ("dead-code-elimination", dce::run),
        ("lvn", lvn::run),
        ("cse", lvn::run_global),
        ("load-store-optimization", memory::run),
        ("bounds-check-elimination", memory::run_bounds_checks),
        ("instruction-scheduling", sched::run),
    ];
    if level == OptimizationLevel::Full {
        // Register allocation reads the final liveness, so it has to run after
        // every instruction-removing pass has had its say.
        passes.push(("register-allocation", regalloc::run));
    }
    passes
}

/// Total instructions and blocks in a module.
pub fn size_of(module: &IrModule) -> (usize, usize) {
    let instructions = module
        .functions
        .iter()
        .map(|function| {
            function
                .blocks
                .iter()
                .map(|block| block.instrs.len())
                .sum::<usize>()
        })
        .sum();
    let blocks = module
        .functions
        .iter()
        .map(|function| function.blocks.len())
        .sum();
    (instructions, blocks)
}
