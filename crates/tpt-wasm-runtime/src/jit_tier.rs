//! The native tier of `EngineMode::Jit`.
//!
//! Each function the JIT accepts runs as machine code; every other function runs on the
//! baseline, which owns the module's state. The split is per function, so one function the
//! compiler refuses does not hold back the rest of its module.
//!
//! A native function is a leaf in the compiler's subset: it makes no calls, reads no
//! globals, and touches no table. It reads and writes linear memory through the baseline's
//! own memory, under that memory's lock, so the two tiers observe one memory. It charges no
//! execution steps, which is why the engine turns this tier off when a step limit, profiling
//! or debugging is in effect.

#[cfg(feature = "jit")]
use std::collections::BTreeMap;

use tpt_wasm_ir::IrModule;
use tpt_wasm_types::Value;
#[cfg(feature = "jit")]
use tpt_wasm_types::ValueType;

use crate::RuntimeError;

#[derive(Default)]
pub(crate) struct JitTier {
    #[cfg(feature = "jit")]
    functions: BTreeMap<usize, tpt_wasm_jit::CompiledFunction>,
}

impl JitTier {
    /// Compile every function of `ir` that the JIT accepts with an integer signature.
    #[cfg(feature = "jit")]
    pub(crate) fn compile(ir: &IrModule) -> Self {
        let integer =
            |value_type: &ValueType| matches!(value_type, ValueType::I32 | ValueType::I64);
        let functions = ir
            .functions
            .iter()
            .enumerate()
            .filter_map(|(index, function)| {
                let compiled = tpt_wasm_jit::compile(function).ok()?;
                let integers = compiled.params().iter().all(integer) && integer(&compiled.result());
                integers.then_some((index, compiled))
            })
            .collect();
        Self { functions }
    }

    /// Without the feature there is no native code to compile.
    #[cfg(not(feature = "jit"))]
    pub(crate) fn compile(_ir: &IrModule) -> Self {
        Self::default()
    }

    /// Whether the function at `index` (its position in the lowered module) has native code.
    pub(crate) fn contains(&self, index: usize) -> bool {
        #[cfg(feature = "jit")]
        {
            self.functions.contains_key(&index)
        }
        #[cfg(not(feature = "jit"))]
        {
            let _ = index;
            false
        }
    }

    /// Run the native function at `index` on `args`, with `memory` as linear memory.
    #[cfg(feature = "jit")]
    pub(crate) fn run(
        &self,
        index: usize,
        args: &[Value],
        memory: Option<&mut [u8]>,
    ) -> Result<Vec<Value>, RuntimeError> {
        let function = self
            .functions
            .get(&index)
            .ok_or(RuntimeError::UnsupportedFeature(
                "a function without native code",
            ))?;
        // The same check the baseline makes, so a wrong argument is reported the same way.
        crate::check_values("argument", function.params(), args)?;
        let mut slots = Vec::with_capacity(args.len());
        for value in args {
            slots.push(match value {
                Value::I32(value) => i64::from(*value),
                Value::I64(value) => *value,
                _ => {
                    return Err(RuntimeError::UnsupportedFeature(
                        "a non-integer argument to a native function",
                    ))
                }
            });
        }
        let raw = function
            .call_raw(&slots, memory)
            .map_err(|error| match error {
                tpt_wasm_jit::JitError::Trap(trap) => RuntimeError::Trap(trap),
                _ => RuntimeError::UnsupportedFeature("a native function that could not run"),
            })?;
        let result = match function.result() {
            ValueType::I32 => Value::I32(raw as u32 as i32),
            _ => Value::I64(raw as i64),
        };
        Ok(vec![result])
    }

    /// Without the feature no function has native code, so this is never reached.
    #[cfg(not(feature = "jit"))]
    pub(crate) fn run(
        &self,
        _index: usize,
        _args: &[Value],
        _memory: Option<&mut [u8]>,
    ) -> Result<Vec<Value>, RuntimeError> {
        Err(RuntimeError::UnsupportedFeature("the jit feature"))
    }
}
