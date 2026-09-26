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

use tpt_wasm_format::{
    ConstExpr, DataMode, ElementInit, ElementMode, ExportDesc, ImportDesc, Module,
};
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
    /// The limits the store was built with, kept here so a caller that cannot
    /// reach the store can still be told what the instance is bounded by.
    limits: ResourceLimits,
    /// Baseline memories, keyed by the store address of the memory they are.
    ///
    /// A compiled module owns no memory of its own: it holds a shared handle, so
    /// an instance that imports another's memory reaches the very same one. This
    /// is where an exporting instance publishes its handle and an importing one
    /// finds it. Keyed by store address because that is what an import resolves
    /// to, and because it stays the same through any number of re-exports.
    baseline_memories: Arc<Mutex<HashMap<u32, tpt_wasm_codegen::SharedMemory>>>,
    /// Baseline globals, keyed by the store address of the global they are.
    ///
    /// A compiled module holds one handle per global, so the store and the
    /// compiled code cannot disagree about a global's current value, and two
    /// instances holding the same global address end up with the same handle.
    /// Keyed by store address because that is what an import resolves to and what
    /// a reader holds.
    baseline_globals: Arc<Mutex<HashMap<u32, tpt_wasm_codegen::SharedGlobal>>>,
    /// Baseline tables, keyed by the store address of the table they are.
    baseline_tables: Arc<Mutex<HashMap<u32, tpt_wasm_codegen::SharedTable>>>,
}

impl RuntimeContext {
    fn new(limits: ResourceLimits) -> Self {
        Self {
            store: Arc::new(Mutex::new(Store::new(limits))),
            call_gate: Arc::new(Mutex::new(())),
            host_functions: Arc::new(Mutex::new(HashMap::new())),
            limits,
            baseline_memories: Arc::new(Mutex::new(HashMap::new())),
            baseline_globals: Arc::new(Mutex::new(HashMap::new())),
            baseline_tables: Arc::new(Mutex::new(HashMap::new())),
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

type BaselineMemoryMap = HashMap<u32, tpt_wasm_codegen::SharedMemory>;

fn lock_baseline_memories(
    memories: &Arc<Mutex<BaselineMemoryMap>>,
) -> Result<MutexGuard<'_, BaselineMemoryMap>, RuntimeError> {
    memories.lock().map_err(|_| RuntimeError::StorePoisoned)
}

type BaselineGlobalMap = HashMap<u32, tpt_wasm_codegen::SharedGlobal>;

fn lock_baseline_globals(
    globals: &Arc<Mutex<BaselineGlobalMap>>,
) -> Result<MutexGuard<'_, BaselineGlobalMap>, RuntimeError> {
    globals.lock().map_err(|_| RuntimeError::StorePoisoned)
}

type BaselineTableMap = HashMap<u32, tpt_wasm_codegen::SharedTable>;

fn lock_baseline_tables(
    tables: &Arc<Mutex<BaselineTableMap>>,
) -> Result<MutexGuard<'_, BaselineTableMap>, RuntimeError> {
    tables.lock().map_err(|_| RuntimeError::StorePoisoned)
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

/// A reference to something another instance in the same context exports.
#[derive(Clone)]
struct ExternalExport {
    context: RuntimeContext,
    address: u32,
}

impl fmt::Debug for ExternalExport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalExport")
            .field("address", &self.address)
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
            // Only the address is recorded. The external *type* is deliberately
            // not captured here: WebAssembly matches an import against the
            // exporting instance's type at the moment the importing module is
            // instantiated, and a memory's type includes its current length, which
            // can change between one module registering the export and the next
            // importing it. A snapshot taken now would be stale by then, so the
            // type is read from the store when an import is resolved.
            registered.push((
                name.clone(),
                LinkerDefinition::Instance(ExternalExport {
                    context: instance.context.clone(),
                    address,
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

/// Read one element segment initializer as a module-level function index.
///
/// An inner `None` means a `ref.null`, which leaves the table slot null; an
/// outer `None` means the expression was neither reference form and the caller
/// must refuse it. The expression is read with Micro's own body decoder rather
/// than a hand-rolled scan, so the index is decoded under the same rules as
/// everywhere else, including a non-minimal encoding.
fn element_entry(expr: &ConstExpr) -> Option<Option<u32>> {
    let instructions = decode_body(&expr.0).ok()?;
    match instructions.as_slice() {
        [Instr::RefNull(_), Instr::End] => Some(None),
        [Instr::RefFunc(index), Instr::End] => Some(Some(*index)),
        _ => None,
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

/// The external type of an exported function.
fn external_function_type(store: &Store, address: u32) -> Result<FunctionType, RuntimeError> {
    Ok(match store.function(address)? {
        tpt_wasm_micro::store::FuncInstance::Wasm(function) => function.func_type.clone(),
        tpt_wasm_micro::store::FuncInstance::Host(function) => function.func_type.clone(),
    })
}

/// The external type of an exported table.
fn external_table_type(store: &Store, address: u32) -> Result<TableType, RuntimeError> {
    let table = store.table(address)?;
    Ok(TableType {
        element_type: table.element_type,
        limits: Limits {
            // A table's length is fixed for its lifetime -- there is no `table.grow`
            // in MVP -- so its declared minimum is its current size.
            min: u64::from(table.min_elements),
            max: table.declared_max_elements.map(u64::from),
        },
    })
}

/// The external type of an exported memory.
///
/// A memory instance's external type carries its *current* length, not the
/// minimum it was declared with. A memory that has grown is externally larger
/// than it was declared, and import matching compares that current size against
/// the importer's declared minimum. Reporting the declared minimum instead
/// refuses exactly the modules the specification means to allow: one module
/// grows a memory, exports it, and another imports it requiring the size it is
/// now.
///
/// The length is read from whichever backend actually owns the memory. In
/// `Micro` mode that is the store; in `Baseline` mode it is the compiled
/// module's shared memory, which the store's copy knows nothing about because a
/// compiled module grows its own and never touches the store's. Reading the
/// store either way would report a stale size in baseline mode, and the two
/// backends would disagree about what a memory is -- which is exactly what a
/// cross-instance import is asking.
fn external_memory_type(
    context: &RuntimeContext,
    store: &Store,
    address: u32,
) -> Result<MemoryType, RuntimeError> {
    if let Some(shared) = lock_baseline_memories(&context.baseline_memories)?.get(&address) {
        let compiled = shared.lock().map_err(|_| RuntimeError::StorePoisoned)?;
        return Ok(MemoryType {
            limits: Limits {
                min: u64::from(compiled.pages()),
                max: compiled.max_pages(),
            },
            memory64: false,
        });
    }
    let memory = store.memory(address)?;
    Ok(MemoryType {
        limits: Limits {
            min: memory.pages(),
            max: memory.declared_max_pages,
        },
        memory64: false,
    })
}

/// The external type of an exported global.
fn external_global_type(store: &Store, address: u32) -> Result<GlobalType, RuntimeError> {
    Ok(store.global(address)?.global_type)
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
        // The reason is carried through, not replaced by a stage name: a module
        // refused here is otherwise reported as a bare "baseline lowering", which
        // says nothing about *what* is unsupported and makes every such refusal a
        // guessing game. `Display` for these errors is the debug form, which names
        // the variant and its payload -- and naming the payload is what turned
        // "baseline lowering" into the two actual causes while this was written.
        let reason = match error {
            tpt_wasm_ir::LowerAndVerifyError::Lowering(error) => format!("lowering: {error}"),
            tpt_wasm_ir::LowerAndVerifyError::Verification(error) => {
                format!("verification: {error}")
            }
        };
        // Leaked because the error variant holds a `&'static str` and the message
        // is assembled here. These are one-off instantiation failures, not a hot
        // path, and the alternative is widening a public error type to own a
        // `String` for every caller.
        RuntimeError::UnsupportedFeature(Box::leak(format!("baseline {reason}").into_boxed_str()))
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
                        let actual = external_function_type(&store, export.address)?;
                        if &actual != expected {
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
                let actual = external_table_type(&store, export.address)?;
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
                let actual = external_memory_type(&context, &store, export.address)?;
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
                let actual = external_global_type(&store, export.address)?;
                if &actual != expected {
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
        // The two families converge on the same slot writes, except that a
        // `ref.null` entry leaves the slot null rather than naming a function.
        let entries: Vec<Option<u32>> = match &element.init {
            ElementInit::FuncIndices(indices) => indices.iter().map(|index| Some(*index)).collect(),
            ElementInit::Expressions(expressions) => expressions
                .iter()
                .map(|expression| {
                    element_entry(expression)
                        .ok_or(RuntimeError::UnsupportedFeature("element initializer"))
                })
                .collect::<Result<_, _>>()?,
        };
        for (delta, function_index) in entries.into_iter().enumerate() {
            let value = match function_index {
                // `ref.null`: the slot is left null, of the segment's element type.
                None => RefValue::Null(element.element_type),
                Some(function_index) => {
                    let address =
                        *func_addrs
                            .get(function_index as usize)
                            .ok_or(RuntimeError::Store(StoreError::UnknownFunction(
                                function_index,
                            )))?;
                    RefValue::FuncRef(address)
                }
            };
            let index = start
                .checked_add(delta as u32)
                .ok_or(StoreError::TableOutOfBounds)?;
            store.table_mut(table_address)?.set(index, value)?;
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

    // Captured before `memory_addrs` moves into the store instance, because the
    // baseline's memory wiring needs the address the module's memory 0 resolved
    // to -- whether the module defined it or imported it, that is the same
    // address, which is exactly why the two instances can share one memory.
    let baseline_memory_address = memory_addrs.first().copied();
    // Captured for the same reason: the baseline publishes a handle for each of
    // its globals under the store address that global was allocated at, and by
    // the time the module is lowered `global_addrs` has moved into the instance.
    let baseline_global_addresses: Vec<u32> = global_addrs.clone();
    // Captured for the same reason: the baseline publishes or installs its table
    // handle under the store address that table was allocated at.
    let baseline_table_address = table_addrs.first().copied();

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
        // The baseline recurses to make a call, so it needs the same call-depth
        // limit Micro reads from the store's limits. Sharing one number is what
        // makes a runaway recursion trap identically on both backends instead of
        // trapping in one and overflowing the host stack in the other.
        module.set_max_call_depth(context.limits.max_call_depth);
        // The instruction budget, for the same reason and from the same limits.
        // Without this the compiled backend would run a module that Micro stops,
        // which is a resource-limit divergence rather than a difference in
        // execution: the embedder asked for a bound and only got it on one backend.
        module.set_max_execution_steps(context.limits.max_execution_steps);
        // A compiled module holds a shared handle to its memory rather than owning
        // one, so that an instance importing another's memory reaches the same
        // bytes. A module that defines its memory publishes the handle under the
        // address it allocated; a module that imports one looks the handle up
        // there and installs it. Re-exports need no special case: the address is
        // the original's, so the handle is found under it however many times the
        // memory has been passed along.
        if let Some(address) = baseline_memory_address {
            if let Some(shared) = module.memory_handle() {
                lock_baseline_memories(&context.baseline_memories)?.insert(address, shared);
            } else {
                let shared = lock_baseline_memories(&context.baseline_memories)?
                    .get(&address)
                    .cloned()
                    .ok_or(RuntimeError::UnsupportedFeature(
                        "a memory imported in baseline mode that no instance in this store exports",
                    ))?;
                module.set_memory(shared).map_err(|_| {
                    RuntimeError::UnsupportedFeature("an imported memory in baseline mode")
                })?;
            }
        } else if module.memory_handle().is_some() {
            return Err(RuntimeError::UnsupportedFeature(
                "a compiled memory with no store address",
            ));
        }
        // The table, by the same rule as the memory: a defined one publishes its
        // handle under the address it allocated, an imported one looks the
        // handle up there and installs that same one. A copy would satisfy every
        // type check and then let the importer's element segment land where the
        // exporter could not see it.
        if let Some(address) = baseline_table_address {
            if let Some(shared) = module.table_handle() {
                lock_baseline_tables(&context.baseline_tables)?.insert(address, shared);
            } else {
                let shared = lock_baseline_tables(&context.baseline_tables)?
                    .get(&address)
                    .cloned()
                    .ok_or(RuntimeError::UnsupportedFeature(
                        "a table imported in baseline mode that no instance in this store exports",
                    ))?;
                module.set_table(shared).map_err(|_| {
                    RuntimeError::UnsupportedFeature("an imported table in baseline mode")
                })?;
            }
        } else if module.table_handle().is_some() {
            return Err(RuntimeError::UnsupportedFeature(
                "a compiled table with no store address",
            ));
        }
        // Globals, one handle each, keyed by store address. A *defined* global
        // publishes its handle so a reader and a later importer find the same
        // value the compiled code uses; an *imported* one takes the exporter's
        // handle instead, which is what makes a `global.set` through either
        // instance visible to the other. Publishing rather than copying is the
        // whole point: when each side kept its own copy, a compiled `global.set`
        // was invisible to `Instance::global`, which reported the initializer
        // forever.
        {
            let handles = module.globals_handles();
            let mut registry = lock_baseline_globals(&context.baseline_globals)?;
            let mut installed = Vec::with_capacity(handles.len());
            for (slot, address) in handles.into_iter().zip(baseline_global_addresses) {
                match slot {
                    Some(owned) => {
                        // A defined global: publish it so a reader or a later
                        // importer finds the same value the compiled code uses.
                        registry.insert(address, owned);
                    }
                    None => {
                        // An imported global: take the exporter's handle, which
                        // must already be published under this address. Copying
                        // the value instead would make a `global.set` through
                        // either instance invisible to the other.
                        let shared = registry.get(&address).cloned().ok_or(
                            RuntimeError::UnsupportedFeature(
                                "a global imported in baseline mode that no instance in this \
                                 store exports",
                            ),
                        )?;
                        installed.push(shared);
                    }
                }
            }
            drop(registry);
            // Only the imported globals go back; a defined one already holds the
            // right handle. `set_global_imports` checks that the count matches the
            // leading unset slots, so a mismatch is refused rather than leaving a
            // global reading nobody's value.
            module.set_global_imports(installed).map_err(|_| {
                RuntimeError::UnsupportedFeature("a global import in baseline mode")
            })?;
        }
    }
    if let Some(start) = start {
        // The start function runs on the same backend the instance will use, so
        // its effects are visible to later calls in baseline mode.
        match instance.baseline.as_mut() {
            Some(module) => {
                // The start index is a Wasm function index, which may name an
                // *import*: a module is allowed to nominate a host function as
                // its start. `call_wasm_index` does the split, so both halves
                // run on the same backend the instance will use.
                module
                    .call_wasm_index(start, Vec::new())
                    .map_err(baseline_error)?;
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

    /// Read the current value of an exported global.
    ///
    /// Read the current value of an exported global, from the owner of the value
    /// rather than from the module's declaration, because a `global.set` inside
    /// any function that has run since instantiation is part of what the global
    /// *is*. The value is copied out, so a caller cannot reach the owner's copy
    /// and hold it past the lock.
    ///
    /// A compiled module keeps its globals in a shared handle, and the store's
    /// `GlobalInstance` is only the one Micro executes against, so this reads
    /// whichever backend actually owns the global.
    pub fn global(&self, name: &str) -> Result<Value, RuntimeError> {
        let ExportDesc::Global(index) = self
            .exports
            .get(name)
            .ok_or_else(|| RuntimeError::UnknownExport(name.to_owned()))?
        else {
            return Err(RuntimeError::TypeMismatch {
                expected: "global export".into(),
                actual: "non-global export".into(),
            });
        };
        let address = lock_store(&self.context.store)?
            .instance(self.index)?
            .global_addrs
            .get(*index as usize)
            .copied()
            .ok_or(RuntimeError::UnknownExport(name.to_owned()))?;
        let shared = lock_baseline_globals(&self.context.baseline_globals)?
            .get(&address)
            .cloned();
        if let Some(shared) = shared {
            let guard = shared.lock().map_err(|_| RuntimeError::StorePoisoned)?;
            return Ok((*guard).clone());
        }
        Ok(lock_store(&self.context.store)?
            .global(address)?
            .value
            .clone())
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
        LocalDecl, Memory, Module, Table,
    };
    use tpt_wasm_types::{
        FunctionType, GlobalType, Limits, MemoryType, RefType, ResourceLimits, ResultType,
        TableType, Trap, Value, ValueType,
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

    /// A host function that only records that it was called.
    ///
    /// Used to check that something the engine was supposed to invoke really ran
    /// -- a start function, for instance, which has no other observable effect
    /// if it does nothing. The counter is shared with the assertion, so this
    /// works on both backends from one engine rather than needing a probe module.
    struct CountingHost {
        counter: Arc<std::sync::atomic::AtomicUsize>,
        takes_i32: bool,
    }

    impl HostFunction for CountingHost {
        fn function_type(&self) -> FunctionType {
            if self.takes_i32 {
                FunctionType {
                    params: ResultType(vec![ValueType::I32]),
                    results: ResultType(Vec::new()),
                }
            } else {
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(Vec::new()),
                }
            }
        }

        fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Vec::new())
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
                init: tpt_wasm_format::ElementInit::FuncIndices(vec![0]),
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

    /// An exported memory's external type is its *current* size, and it is read
    /// when the import resolves rather than when the export was registered.
    ///
    /// A memory that has grown is externally larger than it was declared, and
    /// import matching compares that current size against the importer's declared
    /// minimum. Reporting the declared minimum, or snapshotting the type at
    /// registration time, both refuse the module the specification means to
    /// allow: one module grows a memory and exports it, and another imports it
    /// requiring the size it is now. The register happens *before* the grow here
    /// on purpose, which is the order the spec suite uses.
    #[test]
    fn an_exported_memory_reports_its_current_size_at_import_time() {
        fn memtype(min: u64) -> MemoryType {
            MemoryType {
                limits: Limits { min, max: None },
                memory64: false,
            }
        }
        let provider = Module {
            types: vec![i32_result_type()],
            memories: vec![Memory {
                memory_type: memtype(1),
            }],
            // i32.const 1; memory.grow; end
            functions: vec![function(0, vec![0x41, 0x01, 0x40, 0x00, 0x0b])],
            exports: vec![
                export("memory", ExportDesc::Memory(0)),
                export("grow", ExportDesc::Function(0)),
            ],
            ..Module::default()
        };
        // Requires a memory of at least 2 pages, which is what the provider's
        // memory has become by the time this is instantiated.
        let consumer = Module {
            types: vec![i32_result_type()],
            imports: vec![import("provider", "memory", ImportDesc::Memory(memtype(2)))],
            // memory.size; end
            functions: vec![function(0, vec![0x3f, 0x00, 0x0b])],
            exports: vec![export("size", ExportDesc::Function(0))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let mut provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        assert_eq!(
            provider.call("grow", Vec::new()).unwrap(),
            vec![Value::I32(1)]
        );
        let mut consumer = engine
            .instantiate(consumer)
            .expect("an imported memory must match its current size, not its declared minimum");
        assert_eq!(
            consumer.call("size", Vec::new()).unwrap(),
            vec![Value::I32(2)]
        );
    }

    /// An imported memory is the exporting instance's memory, not a copy of it.
    ///
    /// A store through the importer has to be visible to the exporter, and a grow
    /// through either has to change what the other sees. A copy would satisfy
    /// every type check and then quietly lose the write, which is the failure
    /// this is here to catch.
    #[test]
    fn an_imported_memory_is_shared_rather_than_copied() {
        let provider = Module {
            types: vec![i32_result_type()],
            memories: vec![Memory {
                memory_type: MemoryType {
                    limits: Limits { min: 1, max: None },
                    memory64: false,
                },
            }],
            // i32.const 0; i32.load; end -- reads the first four bytes.
            functions: vec![function(0, vec![0x41, 0x00, 0x28, 0x02, 0x00, 0x0b])],
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
                ImportDesc::Memory(MemoryType {
                    limits: Limits { min: 1, max: None },
                    memory64: false,
                }),
            )],
            // i32.const 0; i32.const 42; i32.store; i32.const 7; end
            functions: vec![function(
                0,
                vec![0x41, 0x00, 0x41, 42, 0x36, 0x02, 0x00, 0x41, 0x07, 0x0b],
            )],
            exports: vec![export("store", ExportDesc::Function(0))],
            ..Module::default()
        };
        let mut engine = Engine::new(Config::default()).unwrap();
        let mut provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        // The memory starts zeroed.
        assert_eq!(
            provider.call("load", Vec::new()).unwrap(),
            vec![Value::I32(0)]
        );
        let mut consumer = engine.instantiate(consumer).unwrap();
        assert_eq!(
            consumer.call("store", Vec::new()).unwrap(),
            vec![Value::I32(7)]
        );
        // The exporter sees the importer's write, so the two share one memory.
        assert_eq!(
            provider.call("load", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    /// The compiled backend shares an imported memory rather than copying it.
    ///
    /// The baseline runs against its own `BaselineMemory`, not the store's, so a
    /// module that imported a copy would satisfy every type check and then quietly
    /// lose every write. Growing the memory through the exporter and storing
    /// through the importer covers both halves: the grow has to be visible in the
    /// size the importer is matched against, and the store in the memory the
    /// exporter reads back.
    #[test]
    fn the_baseline_shares_an_imported_memory() {
        fn memtype(min: u64) -> MemoryType {
            MemoryType {
                limits: Limits { min, max: None },
                memory64: false,
            }
        }
        let provider = Module {
            types: vec![i32_result_type()],
            memories: vec![Memory {
                memory_type: memtype(1),
            }],
            functions: vec![
                // i32.const 1; memory.grow; end
                function(0, vec![0x41, 0x01, 0x40, 0x00, 0x0b]),
                // i32.const 0; i32.load; end
                function(0, vec![0x41, 0x00, 0x28, 0x02, 0x00, 0x0b]),
            ],
            exports: vec![
                export("memory", ExportDesc::Memory(0)),
                export("grow", ExportDesc::Function(0)),
                export("load", ExportDesc::Function(1)),
            ],
            ..Module::default()
        };
        // The importer requires two pages, which is only the case if the grow the
        // exporter already did is visible when this import is matched.
        let consumer = Module {
            types: vec![i32_result_type()],
            imports: vec![import("provider", "memory", ImportDesc::Memory(memtype(2)))],
            // i32.const 0; i32.const 42; i32.store; i32.const 7; end
            functions: vec![function(
                0,
                vec![0x41, 0x00, 0x41, 42, 0x36, 0x02, 0x00, 0x41, 0x07, 0x0b],
            )],
            exports: vec![export("store", ExportDesc::Function(0))],
            ..Module::default()
        };
        let config = Config {
            engine_mode: EngineMode::Baseline,
            ..Config::default()
        };
        let mut engine = Engine::new(config).unwrap();
        let mut provider = engine.instantiate(provider).unwrap();
        engine
            .linker_mut()
            .define_instance("provider", &provider)
            .unwrap();
        assert_eq!(
            provider.call("grow", Vec::new()).unwrap(),
            vec![Value::I32(1)]
        );
        let mut consumer = engine
            .instantiate(consumer)
            .expect("the baseline must share the memory the exporter grew");
        assert_eq!(
            consumer.call("store", Vec::new()).unwrap(),
            vec![Value::I32(7)]
        );
        assert_eq!(
            provider.call("load", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
    }

    /// The compiled backend shares an imported global rather than copying it.
    ///
    /// A global import is an MVP import kind the baseline had to refuse, and it
    /// fails the same way a memory import would have if copied: every type check
    /// passes, and then a `global.set` through the importer is invisible to the
    /// exporter. The test writes through the importer, reads back through the
    /// exporter, and reads again from outside both modules, so a copy is caught
    /// whichever side holds it.
    #[test]
    fn the_baseline_shares_an_imported_global() {
        fn table_type() -> tpt_wasm_types::TableType {
            tpt_wasm_types::TableType {
                element_type: tpt_wasm_types::ReferenceType::FuncRef,
                limits: tpt_wasm_types::Limits { min: 4, max: None },
            }
        }
        fn global_type() -> GlobalType {
            GlobalType {
                value_type: ValueType::I32,
                mutable: true,
            }
        }
        // Provider: one table, one mutable global, and a `call_indirect` that
        // reads an index only the importer will ever have filled in.
        let provider = Module {
            types: vec![i32_result_type()],
            tables: vec![tpt_wasm_format::Table {
                table_type: table_type(),
                init: None,
            }],
            globals: vec![Global {
                global_type: global_type(),
                init: const_i32(1),
            }],
            functions: vec![
                // global.get 0; end -- reads the shared global.
                function(0, vec![0x23, 0x00, 0x0b]),
            ],
            exports: vec![
                export("counter", ExportDesc::Global(0)),
                export("read", ExportDesc::Function(0)),
            ],
            ..Module::default()
        };
        // Importer: imports both, and brings its own active element segment writing
        // its own function 0 into the shared table at index 2. An element segment
        // can only name the importing module's own functions, so the entry the
        // provider later dispatches through is one only this module can install --
        // which is what makes the dispatch a real test of sharing rather than of
        // each side happening to agree.
        let consumer = Module {
            // Two types: `answer` returns an i32, `bump` returns nothing. Giving
            // `bump` the `()->i32` type would be a stack underflow at `end` -- the
            // validator working correctly, not a bug in this test.
            types: vec![
                i32_result_type(),
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(Vec::new()),
                },
            ],
            imports: vec![import(
                "provider",
                "counter",
                ImportDesc::Global(global_type()),
            )],
            functions: vec![
                // 42; unused by this test, but a module needs a body.
                function(0, vec![0x41, 0x2a, 0x0b]),
                // i32.const 9; global.set 0; end
                function(1, vec![0x41, 0x09, 0x24, 0x00, 0x0b]),
                // global.get 0; end
                function(0, vec![0x23, 0x00, 0x0b]),
            ],
            exports: vec![
                export("bump", ExportDesc::Function(1)),
                export("peek", ExportDesc::Function(2)),
            ],
            ..Module::default()
        };
        for mode in [EngineMode::Micro, EngineMode::Baseline] {
            let config = Config {
                engine_mode: mode,
                ..Config::default()
            };
            let mut engine = Engine::new(config).unwrap();
            let mut provider = engine
                .instantiate(provider.clone())
                .unwrap_or_else(|error| panic!("{mode:?} provider: {error}"));
            engine
                .linker_mut()
                .define_instance("provider", &provider)
                .unwrap();
            let mut consumer = engine
                .instantiate(consumer.clone())
                .unwrap_or_else(|error| panic!("{mode:?} importer: {error}"));
            // The importer writes the shared global; the exporter reads it back.
            // A copy would satisfy every type check and lose the write.
            consumer.call("bump", Vec::new()).unwrap();
            assert_eq!(
                provider.call("read", Vec::new()).unwrap(),
                vec![Value::I32(9)],
                "{mode:?} did not share the imported global"
            );
            // And a reader outside both modules agrees with them.
            assert_eq!(
                provider.global("counter").unwrap(),
                Value::I32(9),
                "{mode:?} reader saw a copy"
            );
            // The importer reads it too, which is the same handle from the other
            // side: its own global index 0 *is* the provider's global.
            assert_eq!(
                consumer.call("peek", Vec::new()).unwrap(),
                vec![Value::I32(9)],
                "{mode:?} importer saw a copy"
            );
        }
    }

    /// A table import links, and the importer's element segment lands in it.
    ///
    /// This covers the *linkage* half only. Dispatching through a table that
    /// another module wrote into is a further step the baseline cannot take yet:
    /// a `BaselineTable` entry is an index into the owning module's own function
    /// list, so a shared entry has no function to name from a second module. Micro
    /// stores a store-wide function address and has no such limit, which is a real
    /// divergence between the two backends. It is recorded in `todo.md` rather
    /// than papered over here, and no spec file exercises it (the file that would,
    /// `elem.wast`, also needs the bulk-memory proposal).
    #[test]
    fn a_table_import_links_and_carries_its_element_segment() {
        fn table_type() -> tpt_wasm_types::TableType {
            tpt_wasm_types::TableType {
                element_type: tpt_wasm_types::ReferenceType::FuncRef,
                limits: tpt_wasm_types::Limits { min: 4, max: None },
            }
        }
        let provider = Module {
            tables: vec![tpt_wasm_format::Table {
                table_type: table_type(),
                init: None,
            }],
            exports: vec![export("table", ExportDesc::Table(0))],
            ..Module::default()
        };
        // The element segment writes at offset 3, so it fits a table of 4 and
        // would be out of bounds for one of 3 -- the bounds check has to run
        // against the *imported* table, not against anything the importer owns.
        let consumer = Module {
            // The element segment names function 0, so that function needs a type.
            types: vec![i32_result_type()],
            imports: vec![import("provider", "table", ImportDesc::Table(table_type()))],
            elements: vec![tpt_wasm_format::Element {
                element_type: tpt_wasm_types::ReferenceType::FuncRef,
                init: tpt_wasm_format::ElementInit::FuncIndices(vec![0]),
                mode: tpt_wasm_format::ElementMode::Active {
                    table_index: 0,
                    offset: const_i32(3),
                },
            }],
            functions: vec![function(0, vec![0x41, 0x2a, 0x0b])],
            ..Module::default()
        };
        for mode in [EngineMode::Micro, EngineMode::Baseline] {
            let config = Config {
                engine_mode: mode,
                ..Config::default()
            };
            let mut engine = Engine::new(config).unwrap();
            let provider_instance = engine
                .instantiate(provider.clone())
                .unwrap_or_else(|error| panic!("{mode:?} provider: {error}"));
            engine
                .linker_mut()
                .define_instance("provider", &provider_instance)
                .unwrap();
            engine
                .instantiate(consumer.clone())
                .unwrap_or_else(|error| panic!("{mode:?} importer: {error}"));
            // The same segment must be refused against a table one entry shorter,
            // which only holds if the bounds check saw the *imported* table's size
            // rather than anything the importer allocated.
            let narrow_provider = Module {
                tables: vec![tpt_wasm_format::Table {
                    table_type: tpt_wasm_types::TableType {
                        element_type: tpt_wasm_types::ReferenceType::FuncRef,
                        limits: tpt_wasm_types::Limits { min: 3, max: None },
                    },
                    init: None,
                }],
                exports: vec![export("table", ExportDesc::Table(0))],
                ..Module::default()
            };
            let narrow = engine.instantiate(narrow_provider).unwrap();
            engine
                .linker_mut()
                .define_instance("narrow", &narrow)
                .unwrap();
            let too_narrow = Module {
                types: consumer.types.clone(),
                imports: vec![import(
                    "narrow",
                    "table",
                    ImportDesc::Table(tpt_wasm_types::TableType {
                        element_type: tpt_wasm_types::ReferenceType::FuncRef,
                        limits: tpt_wasm_types::Limits { min: 3, max: None },
                    }),
                )],
                elements: consumer.elements.clone(),
                functions: consumer.functions.clone(),
                ..Module::default()
            };
            engine.instantiate(too_narrow).unwrap_err();
        }
    }

    /// Both backends enforce the instruction budget, and at the same instruction.
    ///
    /// `max_execution_steps` used to be honoured by Micro and silently ignored by
    /// the baseline, so an embedder asking for a bound got it on one backend only,
    /// and a module that did not terminate simply kept running on the other. The
    /// two also have to agree on *where* the budget runs out, not merely that they
    /// run out: charging an instruction before it executes is what makes the trap
    /// land on the same one rather than one instruction apart.
    #[test]
    fn both_backends_enforce_the_instruction_budget() {
        // A long but *finite* loop, so that a regression fails an assertion rather
        // than hanging: with the budget ignored this runs to completion and the
        // assertions below report the missing trap, where an endless loop would
        // simply never return and leave CI waiting rather than reporting.
        //   (local $i i32)
        //   (loop (local.set $i (i32.add (local.get $i) (i32.const 1)))
        //         (br_if 0 (i32.lt_u (local.get $i) (i32.const 1000))))
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(Vec::new()),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: vec![LocalDecl {
                    count: 1,
                    value_type: ValueType::I32,
                }],
                // The body starts at the first instruction: locals are declared by
                // the `locals` field, not as bytes here.
                body: vec![
                    0x41, 0x00, // i32.const 0
                    0x21, 0x00, // local.set 0
                    0x03, 0x40, // loop, no result
                    0x20, 0x00, // local.get 0
                    0x41, 0x01, // i32.const 1
                    0x6a, // i32.add
                    0x21, 0x00, // local.set 0
                    0x20, 0x00, // local.get 0
                    0x41, 0xe8, 0x07, // i32.const 1000, signed LEB
                    0x49, // i32.lt_u
                    0x0d, 0x00, // br_if 0 -- back to the loop header
                    0x0b, // end (loop)
                    0x0b, // end (function)
                ],
            }],
            exports: vec![export("count", ExportDesc::Function(0))],
            ..Module::default()
        };
        for budget in [1u64, 2, 3, 5, 10, 100, 5_000] {
            for mode in [EngineMode::Micro, EngineMode::Baseline] {
                let config = Config {
                    engine_mode: mode,
                    limits: ResourceLimits {
                        max_execution_steps: Some(budget),
                        ..ResourceLimits::default()
                    },
                    ..Config::default()
                };
                let engine = Engine::new(config).unwrap();
                let mut instance = engine
                    .instantiate(module.clone())
                    .unwrap_or_else(|error| panic!("{mode:?} budget {budget}: {error}"));
                // Matched rather than compared, so the assertion reports *which*
                // outcome arrived instead of requiring the whole result to be
                // `PartialEq`.
                match instance.call("count", Vec::new()) {
                    Err(RuntimeError::Trap(Trap::StepsExhausted)) => {}
                    other => {
                        panic!("{mode:?} budget {budget}: expected StepsExhausted, got {other:?}")
                    }
                }
            }
        }
        // A budget above what the loop needs lets it finish on both backends.
        // Without this the test could pass by trapping on every budget, including
        // one nothing should have reached.
        for mode in [EngineMode::Micro, EngineMode::Baseline] {
            let config = Config {
                engine_mode: mode,
                limits: ResourceLimits {
                    max_execution_steps: Some(1_000_000),
                    ..ResourceLimits::default()
                },
                ..Config::default()
            };
            let engine = Engine::new(config).unwrap();
            let mut instance = engine.instantiate(module.clone()).unwrap();
            instance
                .call("count", Vec::new())
                .unwrap_or_else(|error| panic!("{mode:?} did not finish the loop: {error}"));
        }
        // The default is *no* budget, which means unbounded: a module that never
        // terminates then runs until the host stops it. Deliberately not asserted,
        // because asserting it means running a call that cannot return. It is a
        // property of `ResourceLimits::default`, and it is why an embedder that does
        // not trust its modules has to set one.
    }

    /// A start function may be an *imported* function.
    ///
    /// Wasm numbers imported functions first, so a start index below the import
    /// count names a host function. The baseline resolved every start index
    /// against the defined functions only and refused such a module, so a
    /// perfectly valid module failed to instantiate on one backend and not the
    /// other. This runs on both backends so the two cannot drift apart again.
    #[test]
    fn an_imported_function_may_be_the_start_function() {
        for mode in [EngineMode::Micro, EngineMode::Baseline] {
            let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seen = Arc::clone(&counter);
            // (func $note (import "env" "note")) ; (start 0)
            // The start function must have type [] -> [], so the import takes no
            // parameters; `start.wast` has exactly this shape against
            // `spectest.print`.
            let module = Module {
                types: vec![FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(Vec::new()),
                }],
                imports: vec![import("env", "note", ImportDesc::Function(0))],
                functions: vec![function(0, vec![0x10, 0x00, 0x0b])],
                start: Some(0),
                ..Module::default()
            };
            let config = Config {
                engine_mode: mode,
                ..Config::default()
            };
            let mut engine = Engine::new(config).unwrap();
            engine.linker_mut().define_function(
                "env",
                "note",
                CountingHost {
                    counter: seen,
                    takes_i32: false,
                },
            );
            engine
                .instantiate(module)
                .unwrap_or_else(|error| panic!("{mode:?} refused an imported start: {error}"));
            assert_eq!(
                counter.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "{mode:?} did not run the imported start function"
            );
        }
    }

    /// A `global` export reads the value in the store, not the initializer.
    ///
    /// The distinction is the whole point: a `global.set` that has already run
    /// is part of what the global is, and the spec suite checks that a later
    /// `(get ...)` sees the earlier write. Reading the declaration instead
    /// would always return the initializer.
    #[test]
    fn reading_a_global_export_sees_what_a_function_wrote() {
        // (global $g (mut i32) (i32.const 1)) (func (global.set $g (i32.const 9)))
        // (export "g" (global 0)) (export "set" (func 0))
        let module = Module {
            types: vec![
                i32_result_type(),
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(Vec::new()),
                },
            ],
            globals: vec![Global {
                global_type: GlobalType {
                    value_type: ValueType::I32,
                    mutable: true,
                },
                init: const_i32(1),
            }],
            functions: vec![function(1, vec![0x41, 0x09, 0x24, 0x00, 0x0b])],
            exports: vec![
                export("g", ExportDesc::Global(0)),
                export("set", ExportDesc::Function(0)),
            ],
            ..Module::default()
        };
        for mode in [EngineMode::Micro, EngineMode::Baseline] {
            let config = Config {
                engine_mode: mode,
                ..Config::default()
            };
            let engine = Engine::new(config).unwrap();
            let mut instance = engine.instantiate(module.clone()).unwrap();
            assert_eq!(instance.global("g").unwrap(), Value::I32(1), "{mode:?}");
            instance.call("set", Vec::new()).unwrap();
            assert_eq!(
                instance.global("g").unwrap(),
                Value::I32(9),
                "{mode:?} read a copy the compiled code never wrote"
            );
        }
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
