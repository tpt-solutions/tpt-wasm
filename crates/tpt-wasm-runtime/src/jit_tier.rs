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
//! one memory. Its globals are copied out of the baseline's handles before the call and written back
//! after it, including after a trap, so the two tiers observe one set of globals as well. It charges
//! no execution steps, which is why the engine turns this tier off when a step limit, profiling or
//! debugging is in effect.

use tpt_wasm_codegen::SharedGlobal;
use tpt_wasm_ir::IrModule;
use tpt_wasm_types::Value;
#[cfg(feature = "jit")]
use tpt_wasm_types::ValueType;

use crate::native_cache::NativeCache;
use crate::RuntimeError;

/// The linear memory of a native call: its bytes, and its size in bytes, which a grow updates.
#[cfg(feature = "jit")]
pub(crate) use tpt_wasm_jit::LinearMemory;

/// Without the feature no native call runs, so the view exists only to type the same calls.
#[cfg(not(feature = "jit"))]
#[allow(dead_code)]
pub(crate) struct LinearMemory<'a> {
    pub(crate) bytes: &'a mut [u8],
    pub(crate) len: u64,
}

#[derive(Default)]
pub(crate) struct JitTier {
    #[cfg(feature = "jit")]
    module: Option<tpt_wasm_jit::CompiledModule>,
    /// The parameter types and result type of every function, by index, for checking arguments.
    #[cfg(feature = "jit")]
    signatures: Vec<(Vec<ValueType>, Option<ValueType>)>,
    /// The type and mutability of every global, by index, for moving its value in and out.
    #[cfg(feature = "jit")]
    globals: Vec<(ValueType, bool)>,
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
                let result = function.function_type.results.0.first().copied();
                (params, result)
            })
            .collect();
        let globals = ir
            .globals
            .iter()
            .map(|global| (global.value_type, global.mutable))
            .collect();
        Self {
            module: Some(module),
            signatures,
            globals,
        }
    }

    /// As `compile`, but the module is loaded from the native cache when the cache holds a trusted
    /// entry for it, and is stored there after a compile. A store that fails means only that the next
    /// instantiation compiles the module again. A module with globals is not cached: an artifact
    /// records no global count, so a loaded one could not be checked against its globals.
    #[cfg(all(feature = "jit", any(unix, windows)))]
    pub(crate) fn compile_cached(
        ir: &IrModule,
        max_call_depth: u64,
        table_private: bool,
        cache: Option<(&NativeCache, &str)>,
    ) -> Self {
        // An artifact records neither globals nor a grown memory, so such a module is compiled each time.
        let Some((cache, key)) =
            cache.filter(|_| ir.globals.is_empty() && !tpt_wasm_jit::module_uses_memory_grow(ir))
        else {
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
    #[cfg(not(all(feature = "jit", any(unix, windows))))]
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

    /// The value of every global as raw bits, read from the baseline's handles, for the native code
    /// to read and write. A global whose type the native code does not handle is left at zero and is
    /// never read by it. `None` when a handle is missing or cannot be read, so the caller runs the
    /// function on the baseline instead.
    #[cfg(feature = "jit")]
    pub(crate) fn global_bits(&self, handles: &[Option<SharedGlobal>]) -> Option<Vec<u64>> {
        self.globals
            .iter()
            .enumerate()
            .map(|(index, (value_type, _))| {
                if !is_scalar(*value_type) {
                    return Some(0);
                }
                let handle = handles.get(index)?.as_ref()?;
                let value = handle.lock().ok()?;
                let bits = value_bits(&value)?;
                (value_type_of(&value) == Some(*value_type)).then_some(bits)
            })
            .collect()
    }

    /// Without the feature no function has native code, so no globals are moved.
    #[cfg(not(feature = "jit"))]
    pub(crate) fn global_bits(&self, _handles: &[Option<SharedGlobal>]) -> Option<Vec<u64>> {
        Some(Vec::new())
    }

    /// Write the native code's globals back to the baseline's handles. Only mutable globals can have
    /// changed, so only those are written.
    #[cfg(feature = "jit")]
    pub(crate) fn store_global_bits(
        &self,
        handles: &[Option<SharedGlobal>],
        bits: &[u64],
    ) -> Result<(), RuntimeError> {
        for (index, (value_type, mutable)) in self.globals.iter().enumerate() {
            if !*mutable || !is_scalar(*value_type) {
                continue;
            }
            let handle = handles.get(index).and_then(Option::as_ref).ok_or(
                RuntimeError::UnsupportedFeature("a global without a handle"),
            )?;
            let mut value = handle.lock().map_err(|_| {
                RuntimeError::UnsupportedFeature("a global whose lock was poisoned")
            })?;
            *value = bits_value(*value_type, bits[index]);
        }
        Ok(())
    }

    /// Without the feature no globals are moved.
    #[cfg(not(feature = "jit"))]
    pub(crate) fn store_global_bits(
        &self,
        _handles: &[Option<SharedGlobal>],
        _bits: &[u64],
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    /// Run the native function at `index` on `args`, with `memory` as linear memory and `globals`
    /// as the module's globals by index, as raw bits (see [`Self::global_bits`]).
    #[cfg(feature = "jit")]
    pub(crate) fn run(
        &self,
        index: usize,
        args: &[Value],
        memory: Option<&mut LinearMemory>,
        globals: &mut [u64],
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
            .call_with_memory(index, &slots, memory, globals)
            .map_err(|error| match error {
                tpt_wasm_jit::JitError::Trap(trap) => RuntimeError::Trap(trap),
                _ => RuntimeError::UnsupportedFeature("a native function that could not run"),
            })?;
        // A function with no result returns nothing, whatever its compiled code left in the register.
        let value = match result {
            None => return Ok(Vec::new()),
            Some(ValueType::I32) => Value::I32(raw as u32 as i32),
            Some(ValueType::I64) => Value::I64(raw as i64),
            Some(ValueType::F32) => Value::F32(raw as u32),
            Some(ValueType::F64) => Value::F64(raw),
            _ => {
                return Err(RuntimeError::UnsupportedFeature(
                    "a reference result from a native function",
                ))
            }
        };
        Ok(vec![value])
    }

    /// Whether compiled code grows the memory, so the memory must be reserved before a call.
    #[cfg(feature = "jit")]
    pub(crate) fn memory_grows(&self) -> bool {
        self.module
            .as_ref()
            .is_some_and(|module| module.memory_grows())
    }

    /// Without the feature no function grows memory.
    #[cfg(not(feature = "jit"))]
    pub(crate) fn memory_grows(&self) -> bool {
        false
    }

    /// Without the feature no function has native code, so this is never reached.
    #[cfg(not(feature = "jit"))]
    pub(crate) fn run(
        &self,
        _index: usize,
        _args: &[Value],
        _memory: Option<&mut LinearMemory>,
        _globals: &mut [u64],
    ) -> Result<Vec<Value>, RuntimeError> {
        Err(RuntimeError::UnsupportedFeature("the jit feature"))
    }
}

/// Whether a global of this type is one the native code reads and writes.
#[cfg(feature = "jit")]
fn is_scalar(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::I32 | ValueType::I64 | ValueType::F32 | ValueType::F64
    )
}

/// The raw bits of a scalar value: an `i32` in the low four bytes, a float as its bits.
#[cfg(feature = "jit")]
fn value_bits(value: &Value) -> Option<u64> {
    match value {
        Value::I32(value) => Some(u64::from(*value as u32)),
        Value::I64(value) => Some(*value as u64),
        Value::F32(bits) => Some(u64::from(*bits)),
        Value::F64(bits) => Some(*bits),
        _ => None,
    }
}

/// The type of a scalar value, or `None` for a reference.
#[cfg(feature = "jit")]
fn value_type_of(value: &Value) -> Option<ValueType> {
    match value {
        Value::I32(_) => Some(ValueType::I32),
        Value::I64(_) => Some(ValueType::I64),
        Value::F32(_) => Some(ValueType::F32),
        Value::F64(_) => Some(ValueType::F64),
        _ => None,
    }
}

/// The value of a scalar global of `value_type` from its raw bits.
#[cfg(feature = "jit")]
fn bits_value(value_type: ValueType, bits: u64) -> Value {
    match value_type {
        ValueType::I32 => Value::I32(bits as u32 as i32),
        ValueType::I64 => Value::I64(bits as i64),
        ValueType::F32 => Value::F32(bits as u32),
        _ => Value::F64(bits),
    }
}
