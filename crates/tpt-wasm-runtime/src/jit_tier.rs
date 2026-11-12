//! The native tier of `EngineMode::Jit`.
//!
//! Each function the JIT accepts runs as machine code; every other function runs on the
//! baseline, which owns the module's state. The split is per function, so one function the
//! compiler refuses does not hold back the rest of its module. A function that calls a function
//! the compiler refuses is itself refused, since the call would have nowhere to go; the baseline
//! runs it, and its calls reach the baseline's copy of the callee.
//!
//! A native function may call other native functions of its module. It reads and writes linear
//! memory through the baseline's own memory, under that memory's lock, so the two tiers observe
//! one memory. It charges no execution steps, which is why the engine turns this tier off when a
//! step limit, profiling or debugging is in effect.

use tpt_wasm_ir::IrModule;
use tpt_wasm_types::Value;
#[cfg(feature = "jit")]
use tpt_wasm_types::ValueType;

use crate::native_cache::NativeCache;
use crate::RuntimeError;

#[derive(Default)]
pub(crate) struct JitTier {
    #[cfg(feature = "jit")]
    module: Option<tpt_wasm_jit::CompiledModule>,
    /// The parameter types and result type of every function, by index, for checking arguments.
    #[cfg(feature = "jit")]
    signatures: Vec<(Vec<ValueType>, ValueType)>,
}

impl JitTier {
    /// Compile the functions of `ir` that the JIT accepts, together, so calls between them stay
    /// native. `max_call_depth` is the call-depth limit the native calls enforce.
    #[cfg(feature = "jit")]
    pub(crate) fn compile(ir: &IrModule, max_call_depth: u64, table_private: bool) -> Self {
        Self::from_compiled(
            ir,
            tpt_wasm_jit::compile_module(ir, max_call_depth, table_private),
        )
    }

    /// The tier for a module already compiled. The signatures come from the IR, which the baseline
    /// lowers from the same bytes, so they match the compiled functions.
    #[cfg(feature = "jit")]
    fn from_compiled(ir: &IrModule, module: tpt_wasm_jit::CompiledModule) -> Self {
        let signatures = ir
            .functions
            .iter()
            .map(|function| {
                let params = function.function_type.params.0.clone();
                let result = function
                    .function_type
                    .results
                    .0
                    .first()
                    .copied()
                    .unwrap_or(ValueType::I32);
                (params, result)
            })
            .collect();
        Self {
            module: Some(module),
            signatures,
        }
    }

    /// As `compile`, but the module is loaded from the native cache when the cache holds a trusted
    /// entry for it, and is stored there after a compile. A store that fails means only that the next
    /// instantiation compiles the module again.
    #[cfg(all(feature = "jit", unix))]
    pub(crate) fn compile_cached(
        ir: &IrModule,
        max_call_depth: u64,
        table_private: bool,
        cache: Option<(&NativeCache, &str)>,
    ) -> Self {
        let Some((cache, key)) = cache else {
            return Self::compile(ir, max_call_depth, table_private);
        };
        if let Some(module) = cache.load(key, ir.functions.len(), max_call_depth) {
            return Self::from_compiled(ir, module);
        }
        let module = tpt_wasm_jit::compile_module(ir, max_call_depth, table_private);
        let _ = cache.store(key, &module, max_call_depth);
        Self::from_compiled(ir, module)
    }

    /// Without a cache that can be used on this platform, the module is compiled.
    #[cfg(not(all(feature = "jit", unix)))]
    pub(crate) fn compile_cached(
        ir: &IrModule,
        max_call_depth: u64,
        table_private: bool,
        _cache: Option<(&NativeCache, &str)>,
    ) -> Self {
        Self::compile(ir, max_call_depth, table_private)
    }

    /// Without the feature there is no native code to compile.
    #[cfg(not(feature = "jit"))]
    pub(crate) fn compile(_ir: &IrModule, _max_call_depth: u64, _table_private: bool) -> Self {
        Self::default()
    }

    /// Whether the function at `index` (its position in the lowered module) has native code.
    pub(crate) fn contains(&self, index: usize) -> bool {
        #[cfg(feature = "jit")]
        {
            self.module
                .as_ref()
                .is_some_and(|module| module.is_compiled(index))
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
        let module = self
            .module
            .as_ref()
            .ok_or(RuntimeError::UnsupportedFeature(
                "a function without native code",
            ))?;
        let (params, result) =
            self.signatures
                .get(index)
                .ok_or(RuntimeError::UnsupportedFeature(
                    "a function without native code",
                ))?;
        // The same check the baseline makes, so a wrong argument is reported the same way.
        crate::check_values("argument", params, args)?;
        let mut slots = Vec::with_capacity(args.len());
        for value in args {
            slots.push(match value {
                Value::I32(value) => i64::from(*value),
                Value::I64(value) => *value,
                // A float travels as its bits, which the compiled code reads from the slot.
                Value::F32(bits) => i64::from(*bits),
                Value::F64(bits) => *bits as i64,
                _ => {
                    return Err(RuntimeError::UnsupportedFeature(
                        "a reference argument to a native function",
                    ))
                }
            });
        }
        let raw = module
            .call(index, &slots, memory)
            .map_err(|error| match error {
                tpt_wasm_jit::JitError::Trap(trap) => RuntimeError::Trap(trap),
                _ => RuntimeError::UnsupportedFeature("a native function that could not run"),
            })?;
        let value = match result {
            ValueType::I32 => Value::I32(raw as u32 as i32),
            ValueType::I64 => Value::I64(raw as i64),
            ValueType::F32 => Value::F32(raw as u32),
            ValueType::F64 => Value::F64(raw),
            _ => {
                return Err(RuntimeError::UnsupportedFeature(
                    "a reference result from a native function",
                ))
            }
        };
        Ok(vec![value])
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
