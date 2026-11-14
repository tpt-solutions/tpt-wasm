// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Translation validation of the CFG simplification pass against the Lean model.
//!
//! The test generates random functions in a subset the Lean model covers: `i32` constants,
//! the six `i32` binary operators, branches, two-way branches, returns, and `unreachable`.
//! Each function is verified by `tpt-wasm-ir`, simplified by `graph::simplify`, and both
//! versions are written as Lean definitions. The Lean checker in `formal/TranslationCases`
//! then compares the runs of each pair, at fuels 0 to 39, from the empty state.
//!
//! The test needs Lean and Lake, and takes a while, so it runs only when the environment
//! variable `TPT_TRANSLATION_VALIDATION` is set. Without it the test returns at once.
//!
//! What this shows is that the pass agrees with the model on the generated functions. It is
//! testing evidence, not a proof that the Rust pass refines the model.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use tpt_wasm_ir::{
    BasicBlock, BlockId, IrFunction, IrInstr, IrModule, IrValue, Terminator, ValueId,
};
use tpt_wasm_opt::graph::simplify;
use tpt_wasm_types::{FunctionType, ResultType, ValueType};

/// SplitMix64: a small generator with a fixed sequence for a fixed seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Generate one random function with the verifier's SSA discipline by construction: a block
/// reads only the entry's values, its own parameters, and its own earlier results. Edges go
/// forward, plus some back edges to earlier non-entry blocks, which make loops. Visibility
/// is per block, not per path, so SSA holds whatever the control flow.
fn generate(rng: &mut Rng, blocks: usize) -> IrFunction {
    let mut next_value = 0u32;
    let mut values: Vec<IrValue> = Vec::new();
    let mut fresh = |values: &mut Vec<IrValue>| {
        let id = ValueId(next_value);
        next_value += 1;
        values.push(IrValue {
            id,
            value_type: ValueType::I32,
        });
        id
    };

    // Parameters per block: block 0 has none.
    let params_of: Vec<usize> = (0..blocks)
        .map(|j| if j == 0 { 0 } else { rng.below(3) as usize })
        .collect();

    // Edges: each block j > 0 gets one predecessor where capacity allows, and some joins.
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); blocks];
    for j in 1..blocks {
        let candidates: Vec<usize> = (0..j).filter(|&i| successors[i].len() < 2).collect();
        if !candidates.is_empty() {
            let i = candidates[rng.below(candidates.len() as u64) as usize];
            successors[i].push(j);
        }
    }
    for j in 1..blocks {
        if rng.below(4) == 0 {
            let candidates: Vec<usize> = (0..j)
                .filter(|&i| successors[i].len() < 2 && !successors[i].contains(&j))
                .collect();
            if !candidates.is_empty() {
                let i = candidates[rng.below(candidates.len() as u64) as usize];
                successors[i].push(j);
            }
        }
    }

    // Back edges: a block branches to an earlier block other than the entry, if it has room.
    for (j, succ) in successors.iter_mut().enumerate().skip(2) {
        if succ.len() < 2 && rng.below(4) == 0 {
            let candidates: Vec<usize> = (1..j).filter(|i| !succ.contains(i)).collect();
            if !candidates.is_empty() {
                let i = candidates[rng.below(candidates.len() as u64) as usize];
                succ.push(i);
            }
        }
    }

    // Parameter values are fresh values of their block; bodies define more.
    let entry_consts = 2 + rng.below(2) as usize;
    let mut entry_visible: Vec<ValueId> = Vec::new();
    let mut bodies: Vec<Vec<IrInstr>> = vec![Vec::new(); blocks];
    let mut params: Vec<Vec<ValueId>> = vec![Vec::new(); blocks];
    for j in 0..blocks {
        for _ in 0..params_of[j] {
            params[j].push(fresh(&mut values));
        }
    }
    // Entry constants first, so every later block can read them.
    for _ in 0..entry_consts {
        let result = fresh(&mut values);
        let value = rng.below(7) as i32 - 3;
        bodies[0].push(IrInstr::ConstI32 { result, value });
        entry_visible.push(result);
    }
    let mut visible_per_block: Vec<Vec<ValueId>> = vec![Vec::new(); blocks];
    visible_per_block[0] = entry_visible.clone();
    for j in 0..blocks {
        let mut visible: Vec<ValueId> = entry_visible.clone();
        visible.extend(params[j].iter().copied());
        if j == 0 {
            // The entry's own constants are already in `visible` via `entry_visible`.
        }
        let body_len = if j == 0 { 0 } else { rng.below(4) as usize };
        for _ in 0..body_len {
            let result = fresh(&mut values);
            if visible.is_empty() || rng.below(2) == 0 {
                let value = rng.below(7) as i32 - 3;
                bodies[j].push(IrInstr::ConstI32 { result, value });
            } else {
                let left = visible[rng.below(visible.len() as u64) as usize];
                let right = visible[rng.below(visible.len() as u64) as usize];
                let instr = match rng.below(6) {
                    0 => IrInstr::I32Add {
                        result,
                        left,
                        right,
                    },
                    1 => IrInstr::I32Sub {
                        result,
                        left,
                        right,
                    },
                    2 => IrInstr::I32Mul {
                        result,
                        left,
                        right,
                    },
                    3 => IrInstr::I32And {
                        result,
                        left,
                        right,
                    },
                    4 => IrInstr::I32Or {
                        result,
                        left,
                        right,
                    },
                    _ => IrInstr::I32Xor {
                        result,
                        left,
                        right,
                    },
                };
                bodies[j].push(instr);
            }
            visible.push(result);
        }
        visible_per_block[j] = visible;
    }

    let pick = |rng: &mut Rng, visible: &[ValueId]| -> ValueId {
        visible[rng.below(visible.len() as u64) as usize]
    };
    let mut out_blocks = Vec::with_capacity(blocks);
    for j in 0..blocks {
        let visible = &visible_per_block[j];
        let terminator = match successors[j].as_slice() {
            [] => {
                if rng.below(4) == 0 {
                    Terminator::Unreachable
                } else {
                    Terminator::Return(vec![pick(rng, visible)])
                }
            }
            [t] => Terminator::Branch {
                target: BlockId(*t as u32),
                values: (0..params_of[*t]).map(|_| pick(rng, visible)).collect(),
            },
            [t1, t2] => Terminator::CondBranch {
                condition: pick(rng, visible),
                then_target: BlockId(*t1 as u32),
                then_values: (0..params_of[*t1]).map(|_| pick(rng, visible)).collect(),
                else_target: BlockId(*t2 as u32),
                else_values: (0..params_of[*t2]).map(|_| pick(rng, visible)).collect(),
            },
            _ => unreachable!("at most two successors by construction"),
        };
        out_blocks.push(BasicBlock {
            id: BlockId(j as u32),
            params: params[j].clone(),
            instrs: std::mem::take(&mut bodies[j]),
            terminator,
        });
    }

    IrFunction {
        function_type: FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        },
        params: Vec::new(),
        locals: Vec::new(),
        values,
        entry: BlockId(0),
        blocks: out_blocks,
    }
}

fn module_of(function: IrFunction) -> IrModule {
    IrModule {
        functions: vec![function],
        ..IrModule::default()
    }
}

/// The Lean term for a value id.
fn lean_value(v: ValueId) -> String {
    v.0.to_string()
}

fn lean_list(values: &[ValueId]) -> String {
    let parts: Vec<String> = values.iter().map(|v| lean_value(*v)).collect();
    format!("[{}]", parts.join(", "))
}

/// Lean source for one function. Only the subset the Lean model covers is written; any other
/// instruction makes the function unrepresentable, which the caller treats as a skip.
fn lean_function(name: &str, f: &IrFunction) -> Option<String> {
    let mut out = String::new();
    writeln!(out, "def {name} : TptWasm.IR.Function :=").ok()?;
    writeln!(out, "  {{ params := [], locals := [],").ok()?;
    let declared: Vec<ValueId> = f.values.iter().map(|v| v.id).collect();
    writeln!(out, "    declared := {},", lean_list(&declared)).ok()?;
    writeln!(out, "    blocks := [").ok()?;
    for (index, block) in f.blocks.iter().enumerate() {
        let mut instrs = Vec::new();
        for instr in &block.instrs {
            let text = match instr {
                IrInstr::ConstI32 { result, value } => {
                    format!(
                        "TptWasm.IR.Instr.constI32 {} ({})",
                        lean_value(*result),
                        value
                    )
                }
                IrInstr::I32Add {
                    result,
                    left,
                    right,
                } => format!(
                    "TptWasm.IR.Instr.binI32 {} TptWasm.IR.BinOp.add {} {}",
                    lean_value(*result),
                    lean_value(*left),
                    lean_value(*right)
                ),
                IrInstr::I32Sub {
                    result,
                    left,
                    right,
                } => format!(
                    "TptWasm.IR.Instr.binI32 {} TptWasm.IR.BinOp.sub {} {}",
                    lean_value(*result),
                    lean_value(*left),
                    lean_value(*right)
                ),
                IrInstr::I32Mul {
                    result,
                    left,
                    right,
                } => format!(
                    "TptWasm.IR.Instr.binI32 {} TptWasm.IR.BinOp.mul {} {}",
                    lean_value(*result),
                    lean_value(*left),
                    lean_value(*right)
                ),
                IrInstr::I32And {
                    result,
                    left,
                    right,
                } => format!(
                    "TptWasm.IR.Instr.binI32 {} TptWasm.IR.BinOp.and {} {}",
                    lean_value(*result),
                    lean_value(*left),
                    lean_value(*right)
                ),
                IrInstr::I32Or {
                    result,
                    left,
                    right,
                } => format!(
                    "TptWasm.IR.Instr.binI32 {} TptWasm.IR.BinOp.or {} {}",
                    lean_value(*result),
                    lean_value(*left),
                    lean_value(*right)
                ),
                IrInstr::I32Xor {
                    result,
                    left,
                    right,
                } => format!(
                    "TptWasm.IR.Instr.binI32 {} TptWasm.IR.BinOp.xor {} {}",
                    lean_value(*result),
                    lean_value(*left),
                    lean_value(*right)
                ),
                _ => return None,
            };
            instrs.push(text);
        }
        let terminator = match &block.terminator {
            Terminator::Return(values) => {
                format!("TptWasm.IR.Terminator.ret {}", lean_list(values))
            }
            Terminator::Unreachable => "TptWasm.IR.Terminator.unreachable".to_string(),
            Terminator::Branch { target, values } => format!(
                "TptWasm.IR.Terminator.branch {} {}",
                target.0,
                lean_list(values)
            ),
            Terminator::CondBranch {
                condition,
                then_target,
                then_values,
                else_target,
                else_values,
            } => format!(
                "TptWasm.IR.Terminator.condBranch {} ({}, {}) ({}, {})",
                lean_value(*condition),
                then_target.0,
                lean_list(then_values),
                else_target.0,
                lean_list(else_values)
            ),
            Terminator::Trap(_) => return None,
        };
        let separator = if index + 1 == f.blocks.len() { "" } else { "," };
        writeln!(
            out,
            "      {{ id := {}, params := {}, instrs := [{}], terminator := {} }}{}",
            block.id.0,
            lean_list(&block.params),
            instrs.join(", "),
            terminator,
            separator
        )
        .ok()?;
    }
    writeln!(out, "    ],").ok()?;
    writeln!(out, "    entry := {} }}", f.entry.0).ok()?;
    Some(out)
}

/// Shift every constant by one. A mutant of the pass: the check must then report
/// disagreements, which shows that it can fail.
fn corrupt_constants(function: &mut IrFunction) {
    for block in &mut function.blocks {
        for instr in &mut block.instrs {
            if let IrInstr::ConstI32 { value, .. } = instr {
                *value = value.wrapping_add(1);
            }
        }
    }
}

#[test]
fn the_cfg_pass_agrees_with_the_lean_model_on_generated_functions() {
    if std::env::var_os("TPT_TRANSLATION_VALIDATION").is_none() {
        eprintln!("translation validation skipped: set TPT_TRANSLATION_VALIDATION to run it");
        return;
    }
    // With TPT_TRANSLATION_MUTANT set, the optimized copies are corrupted and the test passes
    // only if the checker reports disagreements.
    let mutant = std::env::var_os("TPT_TRANSLATION_MUTANT").is_some();
    let cases: u64 = std::env::var("TPT_TRANSLATION_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);

    let mut source = String::new();
    source.push_str("import TptWasm\n\n");
    source.push_str("open TptWasm.IR\n\n");
    source.push_str("deriving instance DecidableEq for Result\n\n");
    // Fuel is a budget, not part of the behaviour: a pass may finish in fewer steps. So the
    // observation is the first result that finishes. The optimized function may finish where
    // the original runs out of budget, which is inconclusive; the reverse would be a bug.
    source.push_str(
        "def firstDone (f : TptWasm.IR.Function) : Option Result :=\n  \
         ((List.range 200).map fun n => runFunction f n State.empty).find? fun r =>\n    \
         match r with\n    | .exhausted _ => false\n    | _ => true\n\n\
         def checkCase (f g : TptWasm.IR.Function) : Bool :=\n  \
         match firstDone f, firstDone g with\n    \
         | some a, some b => decide (a = b)\n    \
         | some _, none => false\n    \
         | none, _ => true\n\n",
    );

    let mut rng = Rng(0x5eed_c0de);
    let mut written = 0usize;
    let mut total_removed = 0usize;
    let mut total_rewrites = 0usize;
    let mut skipped = 0usize;
    for case in 0..cases {
        let blocks = 2 + rng.below(6) as usize;
        let original = generate(&mut rng, blocks);
        let module = module_of(original.clone());
        if tpt_wasm_ir::verify_module(&module).is_err() {
            skipped += 1;
            continue;
        }
        let mut simplified = original.clone();
        let (removed, rewrites) = simplify(&mut simplified);
        if mutant {
            corrupt_constants(&mut simplified);
        }
        total_removed += removed;
        total_rewrites += rewrites;
        let (Some(before), Some(after)) = (
            lean_function(&format!("orig{case}"), &original),
            lean_function(&format!("opt{case}"), &simplified),
        ) else {
            skipped += 1;
            continue;
        };
        source.push_str(&before);
        source.push('\n');
        source.push_str(&after);
        source.push_str(&format!("#eval checkCase orig{case} opt{case}\n\n"));
        written += 1;
    }
    assert!(written > 0, "no generated function was representable");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let formal = root.join("formal");
    let out_dir = root.join("target").join("translation-validation");
    std::fs::create_dir_all(&out_dir).expect("create the output directory");
    let file = out_dir.join("TranslationCases.lean");
    std::fs::write(&file, &source).expect("write the Lean cases");

    let output = Command::new("lake")
        .arg("env")
        .arg("lean")
        .arg(&file)
        .current_dir(&formal)
        .output()
        .expect("run lake env lean");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the Lean checker failed:\n{stdout}\n{stderr}"
    );
    let results: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        results.len(),
        written,
        "one verdict per case; got {}:\n{stdout}",
        results.len()
    );
    let failures: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| **r != "true")
        .map(|(i, _)| i)
        .collect();
    if mutant {
        assert!(
            !failures.is_empty(),
            "every corrupted copy agreed with the model on all {written} cases; the check cannot fail"
        );
        eprintln!(
            "mutant check: {} of {written} corrupted copies disagree with the model",
            failures.len()
        );
        return;
    }
    assert!(
        failures.is_empty(),
        "the pass disagrees with the model on cases {failures:?} (of {written}; {skipped} skipped)"
    );
    eprintln!(
        "translation validation: {written} cases agree ({skipped} skipped); the pass removed \
         {total_removed} blocks and made {total_rewrites} rewrites"
    );
}
