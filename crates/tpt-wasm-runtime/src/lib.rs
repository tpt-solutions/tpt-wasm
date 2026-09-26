// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly runtime: Store, Instance, Linker, and Engine.
//!
//! This crate provides the MVP execution boundary. It accepts a validated
//! module, builds explicit store state, and drives the Micro interpreter.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use tpt_wasm_decode::{decode, DecodeError};

use tpt_wasm_format::{ConstExpr, DataMode, ElementMode, ExportDesc, ImportDesc, Module};
use tpt_wasm_micro::execution::ExecutionConfig;
use tpt_wasm_micro::instr::{decode_body, Instr};
use tpt_wasm_micro::machine::{Frame, Machine, Step};
pub use tpt_wasm_micro::store::{Instance as StoreInstance, Store, StoreError};
use tpt_wasm_types::{
    FunctionType, GlobalType, Limits, MemoryType, RefValue, ResourceLimits, TableType, Trap, Value,
    ValueType,
};
use tpt_wasm_validate::{ValidatedModule, ValidationError, Validator};

/// Which execution backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EngineMode {
    #[default]
    Micro,
    Baseline,
    Optimizing,
}

/// WebAssembly feature switches for the MVP runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    pub mvp: bool,
    pub reference_types: bool,
    pub bulk_memory: bool,
    pub simd: bool,
    pub multi_value: bool,
    pub sign_extension_ops: bool,
    pub non_trapping_float_to_int: bool,
    pub threads: bool,
    pub extended_const: bool,
    pub exception_handling: bool,
    pub memory64: bool,
    pub multi_memory: bool,
    pub tail_call: bool,
    pub relaxed_simd: bool,
    pub gc: bool,
    pub typed_function_references: bool,
    pub memory_control: bool,
    pub branch_hinting: bool,
    pub flexible_vectors: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            mvp: true,
            reference_types: false,
            bulk_memory: false,
            simd: false,
            multi_value: false,
            sign_extension_ops: false,
            non_trapping_float_to_int: false,
            threads: false,
            extended_const: false,
            exception_handling: false,
            memory64: false,
            multi_memory: false,
            tail_call: false,
            relaxed_simd: false,
            gc: false,
            typed_function_references: false,
            memory_control: false,
            branch_hinting: false,
            flexible_vectors: false,
        }
    }
}

impl Features {
    fn non_mvp_enabled(self) -> bool {
        self.reference_types
            || self.bulk_memory
            || self.simd
            || self.multi_value
            || self.sign_extension_ops
            || self.non_trapping_float_to_int
            || self.threads
            || self.extended_const
            || self.exception_handling
            || self.memory64
            || self.multi_memory
            || self.tail_call
            || self.relaxed_simd
            || self.gc
            || self.typed_function_references
            || self.memory_control
            || self.branch_hinting
            || self.flexible_vectors
    }
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub engine_mode: EngineMode,
    pub features: Features,
    pub limits: ResourceLimits,
    pub deterministic: bool,
    pub debugging: bool,
    pub profiling: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            engine_mode: EngineMode::Micro,
            features: Features::default(),
            limits: ResourceLimits::default(),
            deterministic: true,
            debugging: false,
            profiling: false,
        }
    }
}

/// Errors raised while building or calling a runtime instance.
#[derive(Debug)]
pub enum RuntimeError {
    UnsupportedFeature(&'static str),
    Validation(ValidationError),
    Decode(DecodeError),
    Io(std::io::Error),
    MissingImport {
        module: String,
        name: String,
    },
    DuplicateDefinition {
        module: String,
        name: String,
    },
    IncompatibleImport {
        module: String,
        name: String,
        reason: String,
    },
    ForeignStore,
    StorePoisoned,
    CallGatePoisoned,
    TypeMismatch {
        expected: String,
        actual: String,
    },
    InvalidConstantExpression(&'static str),
    InvalidBody(String),
    UnknownExport(String),
    Store(StoreError),
    Trap(Trap),
    HostCall(tpt_wasm_micro::machine::HostCall),
    HostFunction {
        name: String,
        message: String,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFeature(feature) => write!(f, "unsupported feature: {feature}"),
            Self::Validation(error) => write!(f, "validation error: {error}"),
            Self::Decode(error) => write!(f, "decode error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::MissingImport { module, name } => write!(f, "missing import {module}::{name}"),
            Self::DuplicateDefinition { module, name } => {
                write!(f, "duplicate definition {module}::{name}")
            }
            Self::IncompatibleImport {
                module,
                name,
                reason,
            } => write!(f, "incompatible import {module}::{name}: {reason}"),
            Self::ForeignStore => write!(f, "instance belongs to a different store"),
            Self::StorePoisoned => write!(f, "store lock is poisoned"),
            Self::CallGatePoisoned => write!(f, "call gate is poisoned"),
            Self::TypeMismatch { expected, actual } => {
                write!(f, "type mismatch: expected {expected}, got {actual}")
            }
            Self::InvalidConstantExpression(reason) => {
                write!(f, "invalid constant expression: {reason}")
            }
            Self::InvalidBody(reason) => write!(f, "invalid function body: {reason}"),
            Self::UnknownExport(name) => write!(f, "unknown export: {name}"),
            Self::Store(error) => write!(f, "store error: {error}"),
            Self::Trap(trap) => write!(f, "wasm trap: {trap}"),
            Self::HostCall(call) => write!(f, "host call required: {}", call.func_name),
            Self::HostFunction { name, message } => {
                write!(f, "host function {name} failed: {message}")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<StoreError> for RuntimeError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// An executable host import.
pub trait HostFunction: Send + Sync {
    fn function_type(&self) -> FunctionType;
    fn call(&self, args: &[Value]) -> Result<Vec<Value>, String>;
}

#[derive(Clone)]
struct HostFunctionDefinition {
    function_type: FunctionType,
    implementation: Arc<dyn HostFunction>,
}

impl fmt::Debug for HostFunctionDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostFunctionDefinition")
            .field("function_type", &self.function_type)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct RuntimeContext {
    store: Arc<Mutex<Store>>,
    call_gate: Arc<Mutex<()>>,
    host_functions: Arc<Mutex<HashMap<String, Arc<dyn HostFunction>>>>,
}

impl RuntimeContext {
    fn new(limits: ResourceLimits) -> Self {
        Self {
            store: Arc::new(Mutex::new(Store::new(limits))),
            call_gate: Arc::new(Mutex::new(())),
            host_functions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl fmt::Debug for RuntimeContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeContext")
            .field("store", &self.store)
            .field(
                "host_function_count",
                &lock_host_functions(&self.host_functions)
                    .map(|functions| functions.len())
                    .unwrap_or_default(),
            )
            .finish_non_exhaustive()
    }
}

fn lock_store(store: &Arc<Mutex<Store>>) -> Result<MutexGuard<'_, Store>, RuntimeError> {
    store.lock().map_err(|_| RuntimeError::StorePoisoned)
}

fn lock_call_gate(gate: &Arc<Mutex<()>>) -> Result<MutexGuard<'_, ()>, RuntimeError> {
    gate.lock().map_err(|_| RuntimeError::CallGatePoisoned)
}

type HostFunctionMap = HashMap<String, Arc<dyn HostFunction>>;

fn lock_host_functions(
    functions: &Arc<Mutex<HostFunctionMap>>,
) -> Result<MutexGuard<'_, HostFunctionMap>, RuntimeError> {
    functions.lock().map_err(|_| RuntimeError::StorePoisoned)
}

#[derive(Clone)]
enum LinkerDefinition {
    Host(HostFunctionDefinition),
    Instance(ExternalExport),
}

impl fmt::Debug for LinkerDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(definition) => f.debug_tuple("Host").field(definition).finish(),
            Self::Instance(definition) => f.debug_tuple("Instance").field(definition).finish(),
        }
    }
}

#[derive(Clone, Debug)]
enum ExternalKind {
    Function(FunctionType),
    Table(TableType),
    Memory(MemoryType),
    Global(GlobalType),
}

#[derive(Clone)]
struct ExternalExport {
    context: RuntimeContext,
    address: u32,
    kind: ExternalKind,
}

impl fmt::Debug for ExternalExport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalExport")
            .field("address", &self.address)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Resolves module imports to explicitly registered host or instance exports.
#[derive(Debug, Default)]
pub struct Linker {
    definitions: HashMap<(String, String), LinkerDefinition>,
}

impl Linker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn define_function(
        &mut self,
        module: impl Into<String>,
        name: impl Into<String>,
        function: impl HostFunction + 'static,
    ) -> &mut Self {
        let function_type = function.function_type();
        self.definitions.insert(
            (module.into(), name.into()),
            LinkerDefinition::Host(HostFunctionDefinition {
                function_type,
                implementation: Arc::new(function),
            }),
        );
        self
    }

    pub fn define_instance(
        &mut self,
        module: impl Into<String>,
        instance: &Instance,
    ) -> Result<&mut Self, RuntimeError> {
        let module = module.into();
        for name in instance.exports.keys() {
            if self
                .definitions
                .contains_key(&(module.clone(), name.clone()))
            {
                return Err(RuntimeError::DuplicateDefinition {
                    module: module.clone(),
                    name: name.clone(),
                });
            }
        }

        let store = lock_store(&instance.context.store)?;
        let mut registered = Vec::with_capacity(instance.exports.len());
        for (name, desc) in &instance.exports {
            let address = export_address(&store, instance.index, *desc)?;
            registered.push((
                name.clone(),
                LinkerDefinition::Instance(ExternalExport {
                    context: instance.context.clone(),
                    address,
                    kind: external_kind(&store, address, *desc)?,
                }),
            ));
        }
        drop(store);
        for (name, definition) in registered {
            self.definitions.insert((module.clone(), name), definition);
        }
        Ok(self)
    }

    fn instance_context(&self) -> Result<Option<RuntimeContext>, RuntimeError> {
        let mut selected: Option<RuntimeContext> = None;
        for definition in self.definitions.values() {
            let LinkerDefinition::Instance(export) = definition else {
                continue;
            };
            if let Some(existing) = &selected {
                if !Arc::ptr_eq(&existing.store, &export.context.store) {
                    return Err(RuntimeError::ForeignStore);
                }
            } else {
                selected = Some(export.context.clone());
            }
        }
        Ok(selected)
    }

    pub fn instantiate(
        &self,
        validated: ValidatedModule,
        limits: ResourceLimits,
        execution: ExecutionConfig,
    ) -> Result<Instance, RuntimeError> {
        let context = self
            .instance_context()?
            .unwrap_or_else(|| RuntimeContext::new(limits));
        instantiate_module(
            validated.module,
            self,
            context,
            execution,
            EngineMode::Micro,
        )
    }
}

/// The WebAssembly execution engine.
#[derive(Debug)]
pub struct Engine {
    config: Config,
    linker: Linker,
    context: RuntimeContext,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self, RuntimeError> {
        // The optimizing engine is not implemented. Baseline is a complete
        // execution backend and is accepted; see `instantiate_validated`.
        if config.engine_mode == EngineMode::Optimizing {
            return Err(RuntimeError::UnsupportedFeature("optimizing engine mode"));
        }
        if !config.features.mvp || config.features.non_mvp_enabled() {
            return Err(RuntimeError::UnsupportedFeature(
                "requested WebAssembly feature",
            ));
        }
        Ok(Self {
            context: RuntimeContext::new(config.limits),
            config,
            linker: Linker::new(),
        })
    }

    /// Decode, validate, and instantiate a binary module.
    pub fn instantiate_bytes(&self, bytes: &[u8]) -> Result<Instance, RuntimeError> {
        let module = decode(bytes).map_err(RuntimeError::Decode)?;
        self.instantiate(module)
    }

    /// Read, decode, validate, and instantiate a `.wasm` file.
    pub fn instantiate_file(&self, path: impl AsRef<Path>) -> Result<Instance, RuntimeError> {
        let bytes = std::fs::read(path).map_err(RuntimeError::Io)?;
        self.instantiate_bytes(&bytes)
    }

    /// Validate and instantiate a structural module.
    pub fn instantiate(&self, module: Module) -> Result<Instance, RuntimeError> {
        let validated = Validator::new()
            .validate(module)
            .map_err(RuntimeError::Validation)?;
        self.instantiate_validated(validated)
    }

    /// Instantiate a module that has already passed validation.
    pub fn instantiate_validated(
        &self,
        validated: ValidatedModule,
    ) -> Result<Instance, RuntimeError> {
        let execution = if self.config.deterministic {
            ExecutionConfig::deterministic(0)
        } else {
            ExecutionConfig::host_dependent()
        };
        instantiate_module(
            validated.module,
            &self.linker,
            self.context.clone(),
            execution,
            self.config.engine_mode,
        )
    }

    pub fn linker_mut(&mut self) -> &mut Linker {
        &mut self.linker
    }
}

fn value_type(value: &Value) -> ValueType {
    match value {
        Value::I32(_) => ValueType::I32,
        Value::I64(_) => ValueType::I64,
        Value::F32(_) => ValueType::F32,
        Value::F64(_) => ValueType::F64,
        Value::V128(_) => ValueType::V128,
        Value::Ref(value) => ValueType::Ref(match value {
            RefValue::Null(kind) => *kind,
            RefValue::FuncRef(_) => tpt_wasm_types::RefType::FuncRef,
            RefValue::ExternRef(_) => tpt_wasm_types::RefType::ExternRef,
        }),
    }
}

fn default_value(value_type: ValueType) -> Value {
    match value_type {
        ValueType::I32 => Value::I32(0),
        ValueType::I64 => Value::I64(0),
        ValueType::F32 => Value::F32(0),
        ValueType::F64 => Value::F64(0),
        ValueType::V128 => Value::V128(0),
        ValueType::Ref(kind) => Value::Ref(RefValue::Null(kind)),
    }
}

fn check_values(label: &str, expected: &[ValueType], values: &[Value]) -> Result<(), RuntimeError> {
    if values.len() != expected.len() {
        return Err(RuntimeError::TypeMismatch {
            expected: format!("{expected:?}"),
            actual: format!("{values:?}"),
        });
    }
    for (index, (value, expected)) in values.iter().zip(expected).enumerate() {
        let actual = value_type(value);
        if actual != *expected {
            return Err(RuntimeError::TypeMismatch {
                expected: format!("{label} {index}: {expected:?}"),
                actual: format!("{label} {index}: {actual:?}"),
            });
        }
    }
    Ok(())
}

fn check_arguments(function_type: &FunctionType, args: &[Value]) -> Result<(), RuntimeError> {
    check_values("argument", &function_type.params.0, args)
}

/// A runtime instance backed by its engine's shared store.
pub struct Instance {
    pub index: u32,
    pub exports: HashMap<String, ExportDesc>,
    pub execution: ExecutionConfig,
    /// The compiled baseline module, when the engine runs in baseline mode.
    ///
    /// It owns that module's memory, table, and globals, so a call through it
    /// observes and mutates the same state the previous call left behind.
    baseline: Option<tpt_wasm_codegen::BaselineModule>,
    context: RuntimeContext,
}

impl fmt::Debug for Instance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Instance")
            .field("index", &self.index)
            .field("context", &self.context)
            .field("exports", &self.exports)
            .field("execution", &self.execution)
            .finish()
    }
}

fn const_value(
    expr: &ConstExpr,
    store: &Store,
    global_addrs: &[u32],
) -> Result<Value, RuntimeError> {
    let instructions = decode_body(&expr.0)
        .map_err(|_| RuntimeError::InvalidConstantExpression("invalid constant expression"))?;
    match instructions.as_slice() {
        [Instr::I32Const(value), Instr::End] => Ok(Value::I32(*value)),
        [Instr::I64Const(value), Instr::End] => Ok(Value::I64(*value)),
        [Instr::F32Const(value), Instr::End] => Ok(Value::F32(*value)),
        [Instr::F64Const(value), Instr::End] => Ok(Value::F64(*value)),
        [Instr::GlobalGet(index), Instr::End] => {
            let address = global_addrs
                .get(*index as usize)
                .copied()
                .ok_or(RuntimeError::InvalidConstantExpression("unknown global"))?;
            store
                .global(address)
                .map(|global| global.value.clone())
                .map_err(|_| RuntimeError::InvalidConstantExpression("unknown global"))
        }
        _ => Err(RuntimeError::InvalidConstantExpression(
            "unsupported constant expression",
        )),
    }
}

fn const_i32(expr: &ConstExpr, store: &Store, global_addrs: &[u32]) -> Result<i32, RuntimeError> {
    match const_value(expr, store, global_addrs)? {
        Value::I32(value) => Ok(value),
        _ => Err(RuntimeError::InvalidConstantExpression(
            "expected i32.const",
        )),
    }
}

fn export_address(
    store: &Store,
    instance_index: u32,
    desc: ExportDesc,
) -> Result<u32, RuntimeError> {
    let instance = store.instance(instance_index)?;
    let result = match desc {
        ExportDesc::Function(index) => instance.func_addrs.get(index as usize).copied(),
        ExportDesc::Table(index) => instance.table_addrs.get(index as usize).copied(),
        ExportDesc::Memory(index) => instance.memory_addrs.get(index as usize).copied(),
        ExportDesc::Global(index) => instance.global_addrs.get(index as usize).copied(),
        ExportDesc::Tag(_) => return Err(RuntimeError::UnsupportedFeature("tags")),
    };
    result.ok_or(RuntimeError::Store(StoreError::UnknownInstance(
        instance_index,
    )))
}

fn external_kind(
    store: &Store,
    address: u32,
    desc: ExportDesc,
) -> Result<ExternalKind, RuntimeError> {
    match desc {
        ExportDesc::Function(_) => {
            let function_type = match store.function(address)? {
                tpt_wasm_micro::store::FuncInstance::Wasm(function) => function.func_type.clone(),
                tpt_wasm_micro::store::FuncInstance::Host(function) => function.func_type.clone(),
            };
            Ok(ExternalKind::Function(function_type))
        }
        ExportDesc::Table(_) => {
            let table = store.table(address)?;
            Ok(ExternalKind::Table(TableType {
                element_type: table.element_type,
                limits: Limits {
                    min: u64::from(table.min_elements),
                    max: table.declared_max_elements.map(u64::from),
                },
            }))
        }
        ExportDesc::Memory(_) => {
            let memory = store.memory(address)?;
            Ok(ExternalKind::Memory(MemoryType {
                limits: Limits {
                    min: memory.min_pages,
                    max: memory.declared_max_pages,
                },
                memory64: false,
            }))
        }
        ExportDesc::Global(_) => Ok(ExternalKind::Global(store.global(address)?.global_type)),
        ExportDesc::Tag(_) => Err(RuntimeError::UnsupportedFeature("tags")),
    }
}

fn limits_match(actual: &Limits, expected: &Limits) -> bool {
    actual.min >= expected.min
        && match (actual.max, expected.max) {
            (_, None) => true,
            (Some(actual_max), Some(expected_max)) => actual_max <= expected_max,
            (None, Some(_)) => false,
        }
}

fn incompatible(import: &tpt_wasm_format::Import, reason: impl Into<String>) -> RuntimeError {
    RuntimeError::IncompatibleImport {
        module: import.module.clone(),
        name: import.name.clone(),
        reason: reason.into(),
    }
}

fn same_context(context: &RuntimeContext, export: &ExternalExport) -> Result<(), RuntimeError> {
    if Arc::ptr_eq(&context.store, &export.context.store) {
        Ok(())
    } else {
        Err(RuntimeError::ForeignStore)
    }
}

/// Compile a module to the portable baseline, when the engine asks for it.
///
/// Returns `None` in micro mode, so the store-backed path is used unchanged.
/// A module the baseline cannot represent is reported here rather than failing
/// later, so a baseline instance is never left half-usable.
fn compile_baseline(
    module: &Module,
    engine_mode: EngineMode,
) -> Result<Option<tpt_wasm_codegen::BaselineModule>, RuntimeError> {
    if engine_mode != EngineMode::Baseline {
        return Ok(None);
    }
    let validated = Validator::new()
        .validate(module.clone())
        .map_err(RuntimeError::Validation)?;
    let verified = tpt_wasm_ir::lower_and_verify(&validated).map_err(|error| {
        RuntimeError::UnsupportedFeature(match error {
            tpt_wasm_ir::LowerAndVerifyError::Lowering(_) => "baseline lowering",
            tpt_wasm_ir::LowerAndVerifyError::Verification(_) => "baseline verification",
        })
    })?;
    let compiled = tpt_wasm_codegen::BaselineModule::lower(verified.module())
        .map_err(|_| RuntimeError::UnsupportedFeature("baseline code generation"))?;
    // The host boundary is installed by `instantiate_module`, once the imports
    // have been resolved to callbacks. Until then a `CallHost` traps, so a
    // compiled module cannot reach outside itself by accident.
    Ok(Some(compiled))
}

/// Routes a compiled module's host calls through the same capability-mediated
/// path Micro uses.
///
/// Micro resolves an import to a callback registered under a synthesized name and
/// the runtime invokes it through [`Instance::invoke_host_call`]. The baseline
/// names its imports by module and field instead, so this maps each import back
/// to that callback and reuses the same invocation, checks, and error reporting.
/// Sharing the path is what makes host-effect equivalence between the two
/// backends a property of the code rather than a coincidence of two tests.
#[derive(Debug, Default)]
struct BaselineHost {
    /// Filled in by the runtime once the store context exists.
    context: Option<RuntimeContext>,
    /// One entry per import the baseline may call, mapping `module::name` to the
    /// synthesized callback name Micro resolves it to.
    bindings: Vec<(String, String)>,
}

impl tpt_wasm_codegen::HostBoundary for BaselineHost {
    fn call(
        &mut self,
        module: &str,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, tpt_wasm_codegen::HostCallFailure> {
        let context = self
            .context
            .clone()
            .ok_or_else(|| tpt_wasm_codegen::HostCallFailure {
                name: format!("{module}::{name}"),
                message: "baseline host call has no runtime context".into(),
            })?;
        let wanted = format!("{module}::{name}");
        let callback = self
            .bindings
            .iter()
            .find(|(bound, _)| *bound == wanted)
            .map(|(_, callback)| callback.clone())
            .ok_or_else(|| tpt_wasm_codegen::HostCallFailure {
                name: wanted,
                message: "the import is not bound to a host function".into(),
            })?;
        // The failure is reported under the same registered name the
        // interpreter uses, so an embedder sees one error shape from both
        // backends.
        let fail = |message: String| tpt_wasm_codegen::HostCallFailure {
            name: callback.clone(),
            message,
        };
        let instance = Instance {
            index: 0,
            exports: HashMap::new(),
            execution: ExecutionConfig::default(),
            baseline: None,
            context,
        };
        let result = instance.invoke_host_call(&tpt_wasm_micro::machine::HostCall {
            func_name: callback.clone(),
            args,
            expected_results: 0,
        });
        drop(instance);
        // The interpreter's host failure is taken apart rather than stringified,
        // so both backends report the same name and message instead of the same
        // name wrapped in a second layer of prose.
        result.map_err(|error| match error {
            RuntimeError::HostFunction { name, message } => {
                tpt_wasm_codegen::HostCallFailure { name, message }
            }
            other => fail(other.to_string()),
        })
    }
}

/// Report a baseline failure the way the interpreter reports the same failure.
fn baseline_error(error: tpt_wasm_codegen::BaselineError) -> RuntimeError {
    match error {
        tpt_wasm_codegen::BaselineError::Trap(trap) => RuntimeError::Trap(trap),
        tpt_wasm_codegen::BaselineError::Host(failure) => RuntimeError::HostFunction {
            name: failure.name,
            message: failure.message,
        },
    }
}

fn instantiate_module(
    module: Module,
    linker: &Linker,
    context: RuntimeContext,
    execution: ExecutionConfig,
    engine_mode: EngineMode,
) -> Result<Instance, RuntimeError> {
    // Compiled first, while the module is still whole: the instantiation below
    // takes its sections apart, after which it can no longer be borrowed.
    let baseline = compile_baseline(&module, engine_mode)?;
    let start = module.start;
    let _call_gate = lock_call_gate(&context.call_gate)?;
    let mut store = lock_store(&context.store)?.clone();
    let mut func_addrs = Vec::new();
    let mut table_addrs = Vec::new();
    let mut memory_addrs = Vec::new();
    let mut global_addrs = Vec::new();
    let mut pending_host_functions = Vec::new();
    // Maps each import the baseline will call to the same synthesized callback
    // name Micro resolves it to, so both backends reach one implementation.
    let mut baseline_bindings: Vec<(String, String)> = Vec::new();
    let baseline_mode = engine_mode == EngineMode::Baseline;

    for import in &module.imports {
        let definition = linker
            .definitions
            .get(&(import.module.clone(), import.name.clone()))
            .ok_or_else(|| RuntimeError::MissingImport {
                module: import.module.clone(),
                name: import.name.clone(),
            })?;
        match &import.desc {
            ImportDesc::Function(type_index) => {
                let expected = module
                    .types
                    .get(*type_index as usize)
                    .ok_or_else(|| RuntimeError::InvalidBody("unknown import type".into()))?;
                match definition {
                    LinkerDefinition::Host(definition) => {
                        if &definition.function_type != expected {
                            return Err(RuntimeError::TypeMismatch {
                                expected: format!("{expected:?}"),
                                actual: format!("{:?}", definition.function_type),
                            });
                        }
                        let callback_name = format!(
                            "tpt-host:{}:{}::{}",
                            store.functions.len(),
                            import.module,
                            import.name
                        );
                        let address =
                            store.add_host_function(expected.clone(), callback_name.clone())?;
                        func_addrs.push(address);
                        pending_host_functions
                            .push((callback_name.clone(), definition.implementation.clone()));
                        // The baseline names its imports by module and field, so
                        // record the pairing it will need to find this callback.
                        baseline_bindings
                            .push((format!("{}::{}", import.module, import.name), callback_name));
                    }
                    LinkerDefinition::Instance(export) => {
                        // A cross-instance import is a Wasm-to-Wasm call, not a
                        // host capability, so the baseline's host boundary cannot
                        // express it. Refused here rather than failing later at
                        // the call site with a less useful message.
                        if baseline_mode {
                            return Err(RuntimeError::UnsupportedFeature(
                                "cross-instance imports in baseline mode",
                            ));
                        }
                        same_context(&context, export)?;
                        let ExternalKind::Function(actual) = &export.kind else {
                            return Err(incompatible(import, "definition is not a function"));
                        };
                        if actual != expected {
                            return Err(incompatible(
                                import,
                                format!("expected {expected:?}, got {actual:?}"),
                            ));
                        }
                        func_addrs.push(export.address);
                    }
                }
            }
            ImportDesc::Table(expected) => {
                let LinkerDefinition::Instance(export) = definition else {
                    return Err(incompatible(import, "definition is not a table"));
                };
                same_context(&context, export)?;
                let ExternalKind::Table(actual) = &export.kind else {
                    return Err(incompatible(import, "definition is not a table"));
                };
                if actual.element_type != expected.element_type
                    || !limits_match(&actual.limits, &expected.limits)
                {
                    return Err(incompatible(
                        import,
                        format!("expected {expected:?}, got {actual:?}"),
                    ));
                }
                table_addrs.push(export.address);
            }
            ImportDesc::Memory(expected) => {
                let LinkerDefinition::Instance(export) = definition else {
                    return Err(incompatible(import, "definition is not a memory"));
                };
                same_context(&context, export)?;
                let ExternalKind::Memory(actual) = &export.kind else {
                    return Err(incompatible(import, "definition is not a memory"));
                };
                if !limits_match(&actual.limits, &expected.limits) {
                    return Err(incompatible(
                        import,
                        format!("expected {expected:?}, got {actual:?}"),
                    ));
                }
                memory_addrs.push(export.address);
            }
            ImportDesc::Global(expected) => {
                let LinkerDefinition::Instance(export) = definition else {
                    return Err(incompatible(import, "definition is not a global"));
                };
                same_context(&context, export)?;
                let ExternalKind::Global(actual) = &export.kind else {
                    return Err(incompatible(import, "definition is not a global"));
                };
                if actual != expected {
                    return Err(incompatible(
                        import,
                        format!("expected {expected:?}, got {actual:?}"),
                    ));
                }
                global_addrs.push(export.address);
            }
            ImportDesc::Tag(_) => return Err(RuntimeError::UnsupportedFeature("tags")),
        }
    }

    for table in &module.tables {
        let min = u32::try_from(table.table_type.limits.min).map_err(|_| {
            RuntimeError::Store(StoreError::InvalidLimits("table minimum exceeds u32"))
        })?;
        let max = table
            .table_type
            .limits
            .max
            .map(u32::try_from)
            .transpose()
            .map_err(|_| {
                RuntimeError::Store(StoreError::InvalidLimits("table maximum exceeds u32"))
            })?;
        table_addrs.push(store.allocate_table(table.table_type.element_type, min, max)?);
    }
    for memory in &module.memories {
        memory_addrs.push(
            store.allocate_memory(memory.memory_type.limits.min, memory.memory_type.limits.max)?,
        );
    }
    for global in &module.globals {
        let value = const_value(&global.init, &store, &global_addrs)?;
        let address = store.add_global(global.global_type, value)?;
        global_addrs.push(address);
    }

    let instance_index = u32::try_from(store.instances.len())
        .map_err(|_| RuntimeError::Store(StoreError::LimitExceeded("too many instances")))?;
    if store.instances.len() >= store.limits.max_instances as usize {
        return Err(RuntimeError::Store(StoreError::LimitExceeded(
            "instance limit exceeded",
        )));
    }
    for function in &module.functions {
        let function_type = module
            .types
            .get(function.type_index as usize)
            .ok_or_else(|| RuntimeError::InvalidBody("unknown function type".into()))?;
        let instrs = decode_body(&function.body)
            .map_err(|error| RuntimeError::InvalidBody(error.to_string()))?;
        let mut local_types = function_type.params.0.clone();
        for declaration in &function.locals {
            let count = usize::try_from(declaration.count)
                .map_err(|_| RuntimeError::InvalidBody("local count overflows".into()))?;
            for _ in 0..count {
                local_types.push(declaration.value_type);
            }
        }
        let function_index = u32::try_from(func_addrs.len())
            .map_err(|_| RuntimeError::Store(StoreError::LimitExceeded("too many functions")))?;
        let address = store.add_wasm_function(
            instance_index,
            function_index,
            function_type.clone(),
            instrs,
            local_types,
        )?;
        func_addrs.push(address);
    }

    for element in &module.elements {
        let ElementMode::Active {
            table_index,
            offset,
        } = &element.mode
        else {
            return Err(RuntimeError::UnsupportedFeature(
                "non-active element segment",
            ));
        };
        let table_address = *table_addrs
            .get(*table_index as usize)
            .ok_or(RuntimeError::Store(StoreError::UnknownTable(*table_index)))?;
        let start = const_i32(offset, &store, &global_addrs)? as u32;
        for (delta, function_index) in element.init.iter().enumerate() {
            let address = *func_addrs
                .get(*function_index as usize)
                .ok_or(RuntimeError::Store(StoreError::UnknownFunction(
                    *function_index,
                )))?;
            let index = start
                .checked_add(delta as u32)
                .ok_or(StoreError::TableOutOfBounds)?;
            store
                .table_mut(table_address)?
                .set(index, RefValue::FuncRef(address))?;
        }
    }
    for segment in &module.data {
        let DataMode::Active {
            memory_index,
            offset,
        } = &segment.mode
        else {
            return Err(RuntimeError::UnsupportedFeature("passive data segment"));
        };
        let memory_address =
            *memory_addrs
                .get(*memory_index as usize)
                .ok_or(RuntimeError::Store(StoreError::UnknownMemory(
                    *memory_index,
                )))?;
        let address = const_i32(offset, &store, &global_addrs)? as u32;
        store
            .memory_mut(memory_address)?
            .write(address as u64, &segment.data)?;
    }

    let store_instance = StoreInstance {
        module_types: module.types.clone(),
        func_addrs,
        table_addrs,
        memory_addrs,
        global_addrs,
    };
    let allocated_index = store.add_instance(store_instance)?;
    debug_assert_eq!(allocated_index, instance_index);
    let exports = module
        .exports
        .into_iter()
        .map(|export| (export.name, export.desc))
        .collect();
    {
        let mut shared_store = lock_store(&context.store)?;
        *shared_store = store;
    }
    {
        let mut callbacks = lock_host_functions(&context.host_functions)?;
        for (name, implementation) in pending_host_functions {
            callbacks.insert(name, implementation);
        }
    }
    let mut instance = Instance {
        index: allocated_index,
        exports,
        execution,
        baseline,
        context: context.clone(),
    };
    // The baseline's host boundary needs the resolved callbacks, which only exist
    // once the import loop has run. It is installed here, before the start
    // function, so a start function that calls a host function resolves too.
    if let Some(module) = instance.baseline.as_mut() {
        if !baseline_bindings.is_empty() {
            module.set_host(BaselineHost {
                context: Some(context.clone()),
                bindings: baseline_bindings,
            });
        }
    }
    if let Some(start) = start {
        // The start function runs on the same backend the instance will use, so
        // its effects are visible to later calls in baseline mode.
        match instance.baseline.as_mut() {
            Some(module) => {
                // The start index is a Wasm function index, so it is rebased the
                // same way an export is.
                let index = module
                    .defined_index(start)
                    .ok_or(RuntimeError::UnsupportedFeature(
                        "start function is an import in baseline mode",
                    ))?;
                module.call(index, Vec::new()).map_err(baseline_error)?;
            }
            None => {
                let address = lock_store(&context.store)?
                    .instance(instance.index)?
                    .func_addrs
                    .get(start as usize)
                    .copied()
                    .ok_or(RuntimeError::Store(StoreError::UnknownFunction(start)))?;
                instance.call_address_locked(address, Vec::new())?;
            }
        }
    }
    Ok(instance)
}

impl Instance {
    pub fn get_function(&self, name: &str) -> Result<u32, RuntimeError> {
        let index = self.function_index(name)?;
        lock_store(&self.context.store)?
            .instance(self.index)?
            .func_addrs
            .get(index as usize)
            .copied()
            .ok_or_else(|| RuntimeError::UnknownExport(name.to_owned()))
    }

    pub fn call(&mut self, name: &str, args: Vec<Value>) -> Result<Vec<Value>, RuntimeError> {
        // In baseline mode the module is executed by its own compiled form
        // rather than by Micro, so the store lookup is not used at all.
        if self.baseline.is_some() {
            return self.call_baseline(name, args);
        }
        let address = self.get_function(name)?;
        self.call_address(address, args)
    }

    /// Resolve a Wasm export by name.
    ///
    /// The baseline addresses functions by module index rather than by store
    /// address, so the export table is consulted directly.
    ///
    /// A Wasm function index counts imported functions first, while the
    /// baseline's table holds only the module's own functions, so the index is
    /// rebased. An export of an imported function has no baseline counterpart,
    /// which is reported here rather than resolving to a different function.
    fn baseline_index(&self, name: &str) -> Result<usize, RuntimeError> {
        let index = match self.exports.get(name) {
            Some(ExportDesc::Function(index)) => *index,
            Some(_) => {
                return Err(RuntimeError::TypeMismatch {
                    expected: "function export".into(),
                    actual: "non-function export".into(),
                })
            }
            None => return Err(RuntimeError::UnknownExport(name.to_owned())),
        };
        let defined = self
            .baseline
            .as_ref()
            .map(|module| module.defined_index(index))
            .unwrap_or(None);
        defined.ok_or(RuntimeError::UnsupportedFeature(
            "export of an imported function in baseline mode",
        ))
    }

    fn call_baseline(&mut self, name: &str, args: Vec<Value>) -> Result<Vec<Value>, RuntimeError> {
        let index = self.baseline_index(name)?;
        let module = self
            .baseline
            .as_mut()
            .expect("checked by the caller that dispatched here");
        module.call(index, args).map_err(baseline_error)
    }

    pub fn store(&self) -> Result<MutexGuard<'_, Store>, RuntimeError> {
        lock_store(&self.context.store)
    }

    pub fn store_mut(&self) -> Result<MutexGuard<'_, Store>, RuntimeError> {
        lock_store(&self.context.store)
    }

    fn function_index(&self, name: &str) -> Result<u32, RuntimeError> {
        match self.exports.get(name) {
            Some(ExportDesc::Function(index)) => Ok(*index),
            Some(_) => Err(RuntimeError::TypeMismatch {
                expected: "function export".into(),
                actual: "non-function export".into(),
            }),
            None => Err(RuntimeError::UnknownExport(name.to_owned())),
        }
    }

    fn call_address(&mut self, address: u32, args: Vec<Value>) -> Result<Vec<Value>, RuntimeError> {
        let call_gate = Arc::clone(&self.context.call_gate);
        let _call_gate = lock_call_gate(&call_gate)?;
        self.call_address_locked(address, args)
    }

    fn call_address_locked(
        &mut self,
        address: u32,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, RuntimeError> {
        let function = {
            let store = lock_store(&self.context.store)?;
            store.function(address)?.clone()
        };
        let (func_type, local_types, instrs, func_idx, instance_idx) = match function {
            tpt_wasm_micro::store::FuncInstance::Wasm(function) => (
                function.func_type.clone(),
                function.local_types.clone(),
                function.instrs.clone(),
                function.func_idx,
                function.instance_idx,
            ),
            tpt_wasm_micro::store::FuncInstance::Host(function) => {
                let call = tpt_wasm_micro::machine::HostCall {
                    func_name: function.name.clone(),
                    args,
                    expected_results: function.func_type.results.0.len(),
                };
                return self.invoke_host_call(&call);
            }
        };
        check_arguments(&func_type, &args)?;
        let mut locals = args;
        locals.extend(
            local_types
                .iter()
                .skip(func_type.params.0.len())
                .copied()
                .map(default_value),
        );
        let frame = Frame::new(
            instance_idx,
            func_idx,
            locals,
            instrs,
            func_type.results.0.len(),
        );
        let mut machine =
            Machine::with_execution(lock_store(&self.context.store)?.clone(), self.execution);
        machine.push_frame(frame).map_err(RuntimeError::Trap)?;
        loop {
            match machine.run() {
                Step::Return(values) => {
                    self.commit_store(machine.store)?;
                    return Ok(values);
                }
                Step::Trap(trap) => {
                    self.commit_store(machine.store)?;
                    return Err(RuntimeError::Trap(trap));
                }
                Step::HostCall(call) => {
                    let results = match self.invoke_host_call(&call) {
                        Ok(results) => results,
                        Err(error) => {
                            self.commit_store(machine.store)?;
                            return Err(error);
                        }
                    };
                    if let Err(trap) = machine.resume_host_call(results) {
                        self.commit_store(machine.store)?;
                        return Err(RuntimeError::Trap(trap));
                    }
                }
                Step::Continue => {
                    self.commit_store(machine.store)?;
                    return Err(RuntimeError::InvalidBody(
                        "machine stopped without return".into(),
                    ));
                }
            }
        }
    }

    fn commit_store(&self, store: Store) -> Result<(), RuntimeError> {
        *lock_store(&self.context.store)? = store;
        Ok(())
    }

    fn invoke_host_call(
        &self,
        call: &tpt_wasm_micro::machine::HostCall,
    ) -> Result<Vec<Value>, RuntimeError> {
        let implementation = {
            let callbacks = lock_host_functions(&self.context.host_functions)?;
            callbacks
                .get(&call.func_name)
                .cloned()
                .ok_or_else(|| RuntimeError::HostFunction {
                    name: call.func_name.clone(),
                    message: "callback is not registered".into(),
                })?
        };
        let function_type = implementation.function_type();
        check_values("argument", &function_type.params.0, &call.args)?;
        let results =
            implementation
                .call(&call.args)
                .map_err(|message| RuntimeError::HostFunction {
                    name: call.func_name.clone(),
                    message,
                })?;
        check_values("result", &function_type.results.0, &results)?;
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use super::{Config, Engine, EngineMode, HostFunction, RuntimeError};
    use tpt_wasm_decode::encode;
    use tpt_wasm_format::{
        ConstExpr, DataMode, DataSegment, Export, ExportDesc, Function, Global, Import, ImportDesc,
        Memory, Module, Table,
    };
    use tpt_wasm_types::{
        FunctionType, GlobalType, Limits, MemoryType, RefType, ResultType, TableType, Value,
        ValueType,
    };

    fn i32_result_type() -> FunctionType {
        FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn const_i32(value: i32) -> ConstExpr {
        let mut bytes = vec![0x41];
        let mut remaining = value;
        loop {
            let mut byte = remaining as u8 & 0x7f;
            remaining >>= 7;
            let sign = byte & 0x40 != 0;
            if (remaining == 0 && !sign) || (remaining == -1 && sign) {
                bytes.push(byte);
                break;
            }
            byte |= 0x80;
            bytes.push(byte);
        }
        bytes.push(0x0b);
        ConstExpr(bytes)
    }

    fn type_with_results(results: Vec<ValueType>) -> FunctionType {
        FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(results),
        }
    }

    fn function(function_type: u32, body: Vec<u8>) -> Function {
        Function {
            type_index: function_type,
            locals: Vec::new(),
            body,
        }
    }

    fn export(name: &str, desc: ExportDesc) -> Export {
        Export {
            name: name.into(),
            desc,
        }
    }

    fn import(module: &str, name: &str, desc: ImportDesc) -> Import {
        Import {
            module: module.into(),
            name: name.into(),
            desc,
        }
    }

    struct AnswerHost;

    impl HostFunction for AnswerHost {
        fn function_type(&self) -> FunctionType {
            i32_result_type()
        }

        fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
            Ok(vec![Value::I32(42)])
        }
    }

    struct FailingHost;

    impl HostFunction for FailingHost {
        fn function_type(&self) -> FunctionType {
            i32_result_type()
        }

        fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
            Err("host failure".into())
        }
    }

    struct WrongResultHost;

    impl HostFunction for WrongResultHost {
        fn function_type(&self) -> FunctionType {
            i32_result_type()
        }

        fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
            Ok(vec![Value::I64(42)])
        }
    }

    fn host_calling_module() -> Module {
        Module {
            types: vec![i32_result_type()],
            imports: vec![Import {
                module: "env".into(),
                name: "answer".into(),
                desc: ImportDesc::Function(0),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x10, 0x00, 0x0b],
            }],
            exports: vec![Export {
                name: "run".into(),
                desc: ExportDesc::Function(1),
            }],
            ..Module::default()
        }
    }

    fn direct_host_export_module() -> Module {
        Module {
            types: vec![i32_result_type()],
            imports: vec![Import {
                module: "env".into(),
                name: "answer".into(),
                desc: ImportDesc::Function(0),
            }],
            exports: vec![Export {
                name: "answer".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        }
    }

    #[test]
    fn instantiates_and_calls_an_exported_function() {
        let module = Module {
            types: vec![i32_result_type()],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x41, 0x2a, 0x0b],
            }],
            exports: vec![Export {
                name: "answer".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        };
        let engine = Engine::new(Config::default()).unwrap();
        let mut instance = engine.instantiate(module).unwrap();
        assert_eq!(
            instance.call("answer", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    #[test]
    fn executes_imported_host_calls_from_wasm_and_direct_exports() {
        let mut engine = Engine::new(Config::default()).unwrap();
        engine
            .linker_mut()
            .define_function("env", "answer", AnswerHost);
        let mut indirect = engine.instantiate(host_calling_module()).unwrap();
        assert_eq!(
            indirect.call("run", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );

        let mut direct = engine.instantiate(direct_host_export_module()).unwrap();
        assert_eq!(
            direct.call("answer", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    #[test]
    fn host_callback_failures_and_result_types_are_checked() {
        let mut failing_engine = Engine::new(Config::default()).unwrap();

        failing_engine
            .linker_mut()
            .define_function("env", "answer", FailingHost);
        let mut failing = failing_engine.instantiate(host_calling_module()).unwrap();
        assert!(matches!(
            failing.call("run", Vec::new()),
            Err(RuntimeError::HostFunction { .. })
        ));

        let mut wrong_engine = Engine::new(Config::default()).unwrap();
        wrong_engine
            .linker_mut()
            .define_function("env", "answer", WrongResultHost);
        let mut wrong = wrong_engine.instantiate(host_calling_module()).unwrap();
        assert!(matches!(
            wrong.call("run", Vec::new()),
            Err(RuntimeError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn missing_and_mismatched_host_imports_are_rejected() {
        let engine = Engine::new(Config::default()).unwrap();
        assert!(matches!(
            engine.instantiate(host_calling_module()),
            Err(RuntimeError::MissingImport { .. })
        ));

        let mut mismatch = Engine::new(Config::default()).unwrap();
        mismatch
            .linker_mut()
            .define_function("env", "answer", HostWithWrongSignature);
        assert!(matches!(
            mismatch.instantiate(host_calling_module()),
            Err(RuntimeError::TypeMismatch { .. })
        ));
    }

    struct HostWithWrongSignature;

    impl HostFunction for HostWithWrongSignature {
        fn function_type(&self) -> FunctionType {
            FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I64]),
            }
        }

        fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
            Ok(vec![Value::I64(42)])
        }
    }

    #[test]
    fn rejects_invalid_call_argument_types() {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(vec![ValueType::I32]),
                results: ResultType(Vec::new()),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x0b],
            }],
            exports: vec![Export {
                name: "consume".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        };
        let engine = Engine::new(Config::default()).unwrap();
        let mut instance = engine.instantiate(module).unwrap();
        assert!(matches!(
            instance.call("consume", vec![Value::I64(1)]),
            Err(RuntimeError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn instantiates_from_bytes_and_file_and_runs_start() {
        let module = Module {
            types: vec![
                i32_result_type(),
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(Vec::new()),
                },
            ],
            functions: vec![
                Function {
                    type_index: 0,
                    locals: Vec::new(),
                    body: vec![0x41, 0x2a, 0x0b],
                },
                Function {
                    type_index: 1,
                    locals: Vec::new(),
                    body: vec![0x0b],
                },
            ],
            start: Some(1),
            exports: vec![Export {
                name: "answer".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        };
        let bytes = encode(&module).unwrap();
        let engine = Engine::new(Config::default()).unwrap();
        let mut instance = engine.instantiate_bytes(&bytes).unwrap();
        assert_eq!(
            instance.call("answer", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );

        let path = std::env::temp_dir().join(format!(
            "tpt-wasm-runtime-{}-start.wasm",
            std::process::id()
        ));
        fs::write(&path, &bytes).unwrap();
        let mut from_file = engine.instantiate_file(&path).unwrap();
        assert_eq!(
            from_file.call("answer", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn links_exported_wasm_functions_across_instances() {
        let signature = i32_result_type();
        let provider = Module {
            types: vec![signature.clone()],
            functions: vec![function(0, vec![0x41, 42, 0x0b])],
            exports: vec![export("answer", ExportDesc::Function(0))],
            ..Module::default()
        };
        let consumer = Module {
            types: vec![signature.clone()],
            imports: vec![import("provider", "answer", ImportDesc::Function(0))],
            functions: vec![function(0, vec![0x10, 0, 0x0b])],
            exports: vec![export("run", ExportDesc::Function(1))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        let mut consumer = engine.instantiate(consumer).unwrap();
        assert_eq!(
            consumer.call("run", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    #[test]
    fn imported_mutable_global_is_shared_by_both_instances() {
        let void = type_with_results(Vec::new());
        let i32 = i32_result_type();
        let global_type = GlobalType {
            value_type: ValueType::I32,
            mutable: true,
        };
        let provider = Module {
            types: vec![i32.clone()],
            globals: vec![Global {
                global_type,
                init: const_i32(1),
            }],
            functions: vec![function(0, vec![0x23, 0, 0x0b])],
            exports: vec![
                export("counter", ExportDesc::Global(0)),
                export("get", ExportDesc::Function(0)),
            ],
            ..Module::default()
        };
        let consumer = Module {
            types: vec![void],
            imports: vec![import(
                "provider",
                "counter",
                ImportDesc::Global(global_type),
            )],
            functions: vec![function(0, vec![0x23, 0, 0x41, 5, 0x6a, 0x24, 0, 0x0b])],
            exports: vec![export("bump", ExportDesc::Function(0))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let mut provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        let mut consumer = engine.instantiate(consumer).unwrap();
        consumer.call("bump", Vec::new()).unwrap();
        assert_eq!(
            provider.call("get", Vec::new()).unwrap(),
            vec![Value::I32(6)]
        );
    }

    #[test]
    fn imported_memory_is_shared_by_both_instances() {
        let memory_type = MemoryType {
            limits: Limits {
                min: 1,
                max: Some(2),
            },
            memory64: false,
        };
        let provider = Module {
            types: vec![i32_result_type(), type_with_results(Vec::new())],
            memories: vec![Memory { memory_type }],
            functions: vec![
                function(0, vec![0x41, 0, 0x28, 2, 0, 0x0b]),
                function(1, vec![0x41, 99, 0x41, 0, 0x36, 2, 0, 0x0b]),
            ],
            data: vec![DataSegment {
                mode: DataMode::Active {
                    memory_index: 0,
                    offset: const_i32(0),
                },
                data: vec![7, 0, 0, 0],
            }],
            exports: vec![
                export("memory", ExportDesc::Memory(0)),
                export("load", ExportDesc::Function(0)),
            ],
            ..Module::default()
        };
        let consumer = Module {
            types: vec![i32_result_type()],
            imports: vec![import(
                "provider",
                "memory",
                ImportDesc::Memory(memory_type),
            )],
            functions: vec![function(
                0,
                vec![0x41, 0, 0x41, 42, 0x36, 2, 0, 0x41, 0, 0x28, 2, 0, 0x0b],
            )],
            exports: vec![export("write_and_load", ExportDesc::Function(0))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let mut provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        let mut consumer = engine.instantiate(consumer).unwrap();
        assert_eq!(
            provider.call("load", Vec::new()).unwrap(),
            vec![Value::I32(7)]
        );
        let consumer_result = consumer.call("write_and_load", Vec::new()).unwrap();
        assert!(Arc::ptr_eq(
            &provider.context.store,
            &consumer.context.store
        ));
        assert_eq!(consumer_result, vec![Value::I32(42)]);
        assert_eq!(
            provider.call("load", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    #[test]
    fn imported_table_supports_cross_instance_indirect_calls() {
        let table_type = TableType {
            element_type: RefType::FuncRef,
            limits: Limits {
                min: 1,
                max: Some(1),
            },
        };
        let provider = Module {
            types: vec![i32_result_type()],
            tables: vec![Table {
                table_type,
                init: None,
            }],
            functions: vec![function(0, vec![0x41, 42, 0x0b])],
            elements: vec![tpt_wasm_format::Element {
                element_type: RefType::FuncRef,
                mode: tpt_wasm_format::ElementMode::Active {
                    table_index: 0,
                    offset: const_i32(0),
                },
                init: vec![0],
            }],
            exports: vec![export("table", ExportDesc::Table(0))],
            ..Module::default()
        };
        let consumer = Module {
            types: vec![i32_result_type()],
            imports: vec![import("provider", "table", ImportDesc::Table(table_type))],
            functions: vec![function(0, vec![0x41, 0, 0x11, 0, 0, 0x0b])],
            exports: vec![export("run", ExportDesc::Function(0))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        let mut consumer = engine.instantiate(consumer).unwrap();
        assert_eq!(
            consumer.call("run", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    #[test]
    fn linker_rejects_duplicate_instance_exports() {
        let module = Module {
            types: vec![i32_result_type()],
            functions: vec![function(0, vec![0x41, 1, 0x0b])],
            exports: vec![export("answer", ExportDesc::Function(0))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let instance = engine.instantiate(module).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &instance)
            .unwrap();
        assert!(matches!(
            engine.linker_mut().define_instance("provider", &instance),
            Err(RuntimeError::DuplicateDefinition { .. })
        ));
    }

    #[test]
    fn linker_rejects_incompatible_memory_limits() {
        let actual = MemoryType {
            limits: Limits {
                min: 1,
                max: Some(2),
            },
            memory64: false,
        };
        let expected = MemoryType {
            limits: Limits {
                min: 1,
                max: Some(1),
            },
            memory64: false,
        };
        let provider = Module {
            types: vec![i32_result_type()],
            memories: vec![Memory {
                memory_type: actual,
            }],
            exports: vec![export("memory", ExportDesc::Memory(0))],
            ..Module::default()
        };
        let consumer = Module {
            types: vec![i32_result_type()],
            imports: vec![import("provider", "memory", ImportDesc::Memory(expected))],
            functions: vec![function(0, vec![0x41, 0, 0x0b])],
            exports: vec![export("run", ExportDesc::Function(0))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        let result = engine.instantiate(consumer);
        assert!(matches!(
            result,
            Err(RuntimeError::IncompatibleImport { .. })
        ));
    }

    #[test]
    fn linker_rejects_instances_from_another_store() {
        let module = Module {
            types: vec![i32_result_type()],
            functions: vec![function(0, vec![0x41, 1, 0x0b])],
            exports: vec![export("answer", ExportDesc::Function(0))],
            ..Module::default()
        };
        let consumer = Module {
            types: vec![i32_result_type()],
            imports: vec![import("provider", "answer", ImportDesc::Function(0))],
            functions: vec![function(0, vec![0x10, 0, 0x0b])],
            exports: vec![export("run", ExportDesc::Function(0))],
            ..Module::default()
        };
        let source = Engine::new(Config::default()).unwrap();
        let source = source.instantiate(module).unwrap();
        let mut target = Engine::new(Config::default()).unwrap();
        target
            .linker_mut()
            .define_instance("provider", &source)
            .unwrap();
        assert!(matches!(
            target.instantiate(consumer),
            Err(RuntimeError::ForeignStore)
        ));
    }

    /// A module exercising the constructs the baseline supports: a direct call,
    /// a global, and a memory with a data segment.
    fn baseline_module() -> Module {
        Module {
            types: vec![
                i32_result_type(),
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(vec![ValueType::I32]),
                },
                FunctionType {
                    params: ResultType(vec![ValueType::I32]),
                    results: ResultType(vec![ValueType::I32]),
                },
            ],
            functions: vec![
                // 0: (param i32) (result i32) call 1; local.get 0; i32.add
                Function {
                    type_index: 2,
                    locals: Vec::new(),
                    body: vec![0x10, 0x01, 0x20, 0x00, 0x6a, 0x0b],
                },
                // 1: (result i32) i32.const 0; i32.load
                Function {
                    type_index: 1,
                    locals: Vec::new(),
                    body: vec![0x41, 0x00, 0x28, 0x02, 0x00, 0x0b],
                },
            ],
            memories: vec![Memory {
                memory_type: MemoryType {
                    limits: Limits { min: 1, max: None },
                    memory64: false,
                },
            }],
            globals: vec![Global {
                global_type: GlobalType {
                    value_type: ValueType::I32,
                    mutable: false,
                },
                init: ConstExpr(vec![0x41, 0x00, 0x0b]),
            }],
            data: vec![DataSegment {
                mode: DataMode::Active {
                    memory_index: 0,
                    offset: ConstExpr(vec![0x41, 0x00, 0x0b]),
                },
                data: vec![0x2a, 0x00, 0x00, 0x00],
            }],
            exports: vec![Export {
                name: "run".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        }
    }

    fn baseline_config() -> Config {
        Config {
            engine_mode: EngineMode::Baseline,
            ..Config::default()
        }
    }

    #[test]
    fn baseline_mode_runs_a_module_through_the_public_api() {
        let engine = Engine::new(baseline_config()).expect("baseline mode should be accepted");
        let mut instance = engine
            .instantiate(baseline_module())
            .expect("the module should compile to baseline code");
        // The callee reads the data segment through a global, so a wrong path
        // in any one of memory, globals, calls, or exports shows up here.
        assert_eq!(
            instance.call("run", vec![Value::I32(100)]).unwrap(),
            vec![Value::I32(142)]
        );
    }

    #[test]
    fn baseline_and_micro_agree_on_the_same_module() {
        let module = baseline_module();
        let mut baseline = Engine::new(baseline_config())
            .unwrap()
            .instantiate(module.clone())
            .unwrap();
        let mut micro = Engine::new(Config::default())
            .unwrap()
            .instantiate(module)
            .unwrap();
        for argument in [0, 1, -3, 7] {
            let args = vec![Value::I32(argument)];
            assert_eq!(
                baseline.call("run", args.clone()).unwrap(),
                micro.call("run", args).unwrap(),
                "argument {argument}"
            );
        }
    }

    #[test]
    fn baseline_state_persists_across_calls() {
        // The module's memory is owned by the compiled instance, so a store in
        // one call must be visible to the next.
        let engine = Engine::new(baseline_config()).unwrap();
        let mut instance = engine.instantiate(baseline_module()).unwrap();
        let first = instance.call("run", vec![Value::I32(5)]).unwrap();
        let second = instance.call("run", vec![Value::I32(5)]).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn baseline_mode_runs_a_host_calling_module() {
        // A `CallHost` crosses the same capability-mediated path Micro uses, so
        // the compiled module and the interpreter agree on the result.
        let mut baseline = Engine::new(baseline_config()).unwrap();
        baseline
            .linker_mut()
            .define_function("env", "answer", AnswerHost);
        let mut micro = Engine::new(Config::default()).unwrap();
        micro
            .linker_mut()
            .define_function("env", "answer", AnswerHost);
        let expected = vec![Value::I32(42)];
        let mut compiled = baseline.instantiate(host_calling_module()).unwrap();
        let mut interpreted = micro.instantiate(host_calling_module()).unwrap();
        assert_eq!(compiled.call("run", Vec::new()).unwrap(), expected);
        assert_eq!(interpreted.call("run", Vec::new()).unwrap(), expected);
    }

    #[test]
    fn a_baseline_host_call_needs_a_resolved_import() {
        // Instantiating without defining the import fails at link time, in both
        // backends, rather than at call time.
        for engine in [
            Engine::new(baseline_config()).unwrap(),
            Engine::new(Config::default()).unwrap(),
        ] {
            assert!(
                matches!(
                    engine.instantiate(host_calling_module()),
                    Err(RuntimeError::MissingImport { .. })
                ),
                "an undefined import should fail at link time"
            );
        }
    }

    #[test]
    fn a_failing_host_call_reports_identically_in_both_backends() {
        // A refused capability is not a module trap, and it is not reported as
        // one. The two backends must produce the same error, name and message
        // included, so an embedder can treat them interchangeably.
        let mut baseline = Engine::new(baseline_config()).unwrap();
        baseline
            .linker_mut()
            .define_function("env", "answer", FailingHost);
        let mut micro = Engine::new(Config::default()).unwrap();
        micro
            .linker_mut()
            .define_function("env", "answer", FailingHost);
        let mut compiled = baseline.instantiate(host_calling_module()).unwrap();
        let mut interpreted = micro.instantiate(host_calling_module()).unwrap();
        let from_baseline = compiled.call("run", Vec::new()).unwrap_err();
        let from_micro = interpreted.call("run", Vec::new()).unwrap_err();
        assert!(
            matches!(&from_baseline, RuntimeError::HostFunction { .. }),
            "a refused capability should be a host failure, not a trap: {from_baseline:?}"
        );
        // `RuntimeError` is not `PartialEq`, so the two are compared as the
        // text an embedder would log. Same shape, same name, same message.
        assert_eq!(
            from_baseline.to_string(),
            from_micro.to_string(),
            "the two backends reported the host failure differently"
        );
    }

    #[test]
    fn a_host_returning_the_wrong_type_is_rejected_in_both_backends() {
        // The host is outside the verified module, so its return is checked
        // against the declared signature in both backends.
        let mut baseline = Engine::new(baseline_config()).unwrap();
        baseline
            .linker_mut()
            .define_function("env", "answer", WrongResultHost);
        let mut micro = Engine::new(Config::default()).unwrap();
        micro
            .linker_mut()
            .define_function("env", "answer", WrongResultHost);
        let mut compiled = baseline.instantiate(host_calling_module()).unwrap();
        let mut interpreted = micro.instantiate(host_calling_module()).unwrap();
        assert!(compiled.call("run", Vec::new()).is_err());
        assert!(interpreted.call("run", Vec::new()).is_err());
    }

    #[test]
    fn an_unknown_export_is_reported_in_baseline_mode() {
        let engine = Engine::new(baseline_config()).unwrap();
        let mut instance = engine.instantiate(baseline_module()).unwrap();
        assert!(matches!(
            instance.call("missing", Vec::new()),
            Err(RuntimeError::UnknownExport(_))
        ));
    }

    #[test]
    fn the_optimizing_engine_is_still_refused() {
        let error = Engine::new(Config {
            engine_mode: EngineMode::Optimizing,
            ..Config::default()
        })
        .expect_err("the optimizing engine is not implemented");
        assert!(
            matches!(
                error,
                RuntimeError::UnsupportedFeature("optimizing engine mode")
            ),
            "unexpected error: {error:?}"
        );
    }
}
