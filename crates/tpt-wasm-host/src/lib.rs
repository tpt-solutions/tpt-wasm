// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! TPT capability-oriented host.
//!
//! Core principle: No ambient authority.
//!
//! Wasm programs may only interact with host resources through explicitly
//! granted capabilities. The host does not expose filesystem, sockets, or
//! POSIX by default.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use tpt_wasm_capability::{
    Capability, CapabilityDescriptor, CapabilityError, CapabilityGrant, CapabilityId,
    CapabilitySet, Effect, HostError as CapabilityHostError, Operation,
};
use tpt_wasm_resource::ResourceTable;
use tpt_wasm_types::{FunctionType, Value};

/// Execution mode for the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    #[default]
    HostDependent,
    Deterministic,
    RecordReplay,
}

/// A checked virtual clock measured in deterministic ticks.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VirtualClock(u64);

impl VirtualClock {
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn now(self) -> u64 {
        self.0
    }

    pub fn advance(&mut self, ticks: u64) -> Result<u64, HostError> {
        self.0 = self.0.checked_add(ticks).ok_or(HostError::ClockOverflow)?;
        Ok(self.0)
    }
}

/// A deterministic xorshift64* random stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// Network behavior selected by the host execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    #[default]
    Prohibited,
    Recorded,
    HostDependent,
}

/// An in-memory filesystem snapshot; no path escapes this map.
#[derive(Debug, Default)]
pub struct ControlledFilesystem {
    files: BTreeMap<String, Vec<u8>>,
}

impl ControlledFilesystem {
    pub fn read(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    pub fn write(&mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        self.files.insert(path.into(), bytes.into());
    }

    pub fn remove(&mut self, path: &str) -> bool {
        self.files.remove(path).is_some()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// A deterministic task queue ordered by priority and insertion sequence.
#[derive(Debug, Default)]
pub struct DeterministicScheduler {
    next_sequence: u64,
    tasks: BTreeMap<(u64, u64), String>,
}

impl DeterministicScheduler {
    pub fn schedule(&mut self, priority: u64, task: impl Into<String>) -> Result<(), HostError> {
        let sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(HostError::SchedulerOverflow)?;
        self.tasks.insert((priority, sequence), task.into());
        self.next_sequence = sequence;
        Ok(())
    }

    pub fn pop(&mut self) -> Option<String> {
        let key = *self.tasks.keys().next()?;
        self.tasks.remove(&key)
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

/// A successful host effect and its deterministic replay record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRecord {
    pub capability: CapabilityId,
    pub operation: Operation,
    pub effect: Effect,
    pub result: Vec<Value>,
}

/// Errors raised by host registration, authorization, invocation, or replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Authorization(CapabilityError),
    NotRegistered(CapabilityId),
    DescriptorMismatch(CapabilityId),
    ReplayExhausted,
    ReplayMismatch { expected: usize, actual: usize },
    Capability(CapabilityHostError),
    ClockOverflow,
    SchedulerOverflow,
    NetworkProhibited,
    DeterministicServiceUnavailable(ExecutionMode),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for HostError {}

/// The TPT host mediates all Wasm ↔ host interaction.
pub struct Host {
    capabilities: CapabilitySet,
    implementations: HashMap<CapabilityId, Box<dyn Capability>>,
    resources: ResourceTable,
    mode: ExecutionMode,
    effects: Vec<EffectRecord>,
    replay: Vec<EffectRecord>,
    replay_cursor: usize,
    clock: VirtualClock,
    rng: DeterministicRng,
    filesystem: ControlledFilesystem,
    network_policy: NetworkPolicy,
    scheduler: DeterministicScheduler,
}

/// The standard module name used by Preview 1 WASI imports.
pub const WASI_SNAPSHOT_PREVIEW1_MODULE: &str = "wasi_snapshot_preview1";

/// One explicit WASI import-to-capability binding.
///
/// The adapter carries no capability grant of its own. The referenced host
/// must already contain the capability descriptor and grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasiBinding {
    pub import: String,
    pub capability: CapabilityId,
    pub operation: Operation,
    pub function_type: FunctionType,
}

impl WasiBinding {
    pub fn new(
        import: impl Into<String>,
        capability: CapabilityId,
        operation: Operation,
        function_type: FunctionType,
    ) -> Result<Self, WasiAdapterError> {
        let import = import.into();
        if import.is_empty() {
            return Err(WasiAdapterError::EmptyImportName);
        }
        Ok(Self {
            import,
            capability,
            operation,
            function_type,
        })
    }
}

/// Errors raised while constructing a WASI capability adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasiAdapterError {
    EmptyImportName,
    DuplicateImport(String),
}

/// Installs explicitly bound WASI imports into a runtime linker.
///
/// This is an adapter skeleton, not a WASI implementation. It deliberately
/// has no default imports: a module receives only the bindings configured by
/// the embedder, and each call is still authorized by its host capability.
pub struct WasiAdapter {
    host: Arc<Mutex<Host>>,
    bindings: BTreeMap<String, WasiBinding>,
}

impl WasiAdapter {
    pub fn new(host: Arc<Mutex<Host>>) -> Self {
        Self {
            host,
            bindings: BTreeMap::new(),
        }
    }

    pub fn bind(
        &mut self,
        import: impl Into<String>,
        capability: CapabilityId,
        operation: Operation,
        function_type: FunctionType,
    ) -> Result<(), WasiAdapterError> {
        let binding = WasiBinding::new(import, capability, operation, function_type)?;
        if self.bindings.contains_key(&binding.import) {
            return Err(WasiAdapterError::DuplicateImport(binding.import));
        }
        self.bindings.insert(binding.import.clone(), binding);
        Ok(())
    }

    pub fn binding(&self, import: &str) -> Option<&WasiBinding> {
        self.bindings.get(import)
    }

    pub fn bindings(&self) -> impl Iterator<Item = &WasiBinding> {
        self.bindings.values()
    }

    /// Defines the configured bindings in the standard WASI module namespace.
    pub fn define(&self, linker: &mut tpt_wasm_runtime::Linker) {
        for binding in self.bindings.values() {
            linker.define_function(
                WASI_SNAPSHOT_PREVIEW1_MODULE,
                &binding.import,
                CapabilityHostFunction::new(
                    Arc::clone(&self.host),
                    binding.capability.clone(),
                    binding.operation.clone(),
                    binding.function_type.clone(),
                ),
            );
        }
    }
}

/// A runtime host import backed by one explicitly authorized capability.
///
/// The adapter does not grant authority. Every invocation goes through
/// [`Host::invoke`], so callers receive the same authorization, registration,
/// replay, and effect-tracking checks as direct host users.
pub struct CapabilityHostFunction {
    host: Arc<Mutex<Host>>,
    capability: CapabilityId,
    operation: Operation,
    function_type: FunctionType,
}

impl CapabilityHostFunction {
    pub fn new(
        host: Arc<Mutex<Host>>,
        capability: CapabilityId,
        operation: Operation,
        function_type: FunctionType,
    ) -> Self {
        Self {
            host,
            capability,
            operation,
            function_type,
        }
    }
}

impl tpt_wasm_runtime::HostFunction for CapabilityHostFunction {
    fn function_type(&self) -> FunctionType {
        self.function_type.clone()
    }

    fn call(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        let mut host = self
            .host
            .lock()
            .map_err(|_| "capability host lock is poisoned".to_owned())?;
        host.invoke(&self.capability, &self.operation, args)
            .map_err(|error| error.to_string())
    }
}

impl Host {
    pub fn new(mode: ExecutionMode) -> Self {
        Self::new_with_seed(mode, 0)
    }

    pub fn new_with_seed(mode: ExecutionMode, seed: u64) -> Self {
        let network_policy = match mode {
            ExecutionMode::HostDependent => NetworkPolicy::HostDependent,
            ExecutionMode::Deterministic | ExecutionMode::RecordReplay => NetworkPolicy::Prohibited,
        };
        Self {
            capabilities: CapabilitySet::default(),
            implementations: HashMap::new(),
            resources: ResourceTable::new(),
            mode,
            effects: Vec::new(),
            replay: Vec::new(),
            replay_cursor: 0,
            clock: VirtualClock::new(),
            rng: DeterministicRng::new(seed),
            filesystem: ControlledFilesystem::default(),
            network_policy,
            scheduler: DeterministicScheduler::default(),
        }
    }

    pub fn register(
        &mut self,
        implementation: Box<dyn Capability>,
        descriptor: CapabilityDescriptor,
    ) -> Result<(), HostError> {
        if implementation.id() != &descriptor.id {
            return Err(HostError::DescriptorMismatch(descriptor.id));
        }
        let id = descriptor.id.clone();
        self.capabilities
            .register(descriptor)
            .map_err(HostError::Authorization)?;
        self.implementations.insert(id, implementation);
        Ok(())
    }

    pub fn grant(&mut self, grant: CapabilityGrant) -> Result<(), HostError> {
        self.capabilities
            .grant(grant)
            .map_err(HostError::Authorization)
    }

    pub fn revoke(&mut self, id: &CapabilityId) -> bool {
        self.capabilities.revoke(id)
    }

    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    pub fn resources(&self) -> &ResourceTable {
        &self.resources
    }

    pub fn resources_mut(&mut self) -> &mut ResourceTable {
        &mut self.resources
    }

    pub fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn virtual_time(&self) -> u64 {
        self.clock.now()
    }

    pub fn advance_virtual_time(&mut self, ticks: u64) -> Result<u64, HostError> {
        self.require_deterministic_mode()?;
        self.clock.advance(ticks)
    }

    pub fn next_random_u64(&mut self) -> Result<u64, HostError> {
        self.require_deterministic_mode()?;
        Ok(self.rng.next_u64())
    }

    pub fn filesystem(&self) -> &ControlledFilesystem {
        &self.filesystem
    }

    pub fn filesystem_mut(&mut self) -> Result<&mut ControlledFilesystem, HostError> {
        self.require_deterministic_mode()?;
        Ok(&mut self.filesystem)
    }

    pub fn network_policy(&self) -> NetworkPolicy {
        self.network_policy
    }

    pub fn set_network_policy(&mut self, policy: NetworkPolicy) -> Result<(), HostError> {
        if self.mode != ExecutionMode::HostDependent && policy == NetworkPolicy::HostDependent {
            return Err(HostError::DeterministicServiceUnavailable(self.mode));
        }
        self.network_policy = policy;
        Ok(())
    }

    pub fn check_network(&self) -> Result<(), HostError> {
        if self.network_policy == NetworkPolicy::Prohibited {
            Err(HostError::NetworkProhibited)
        } else {
            Ok(())
        }
    }

    pub fn scheduler(&self) -> &DeterministicScheduler {
        &self.scheduler
    }

    pub fn scheduler_mut(&mut self) -> Result<&mut DeterministicScheduler, HostError> {
        self.require_deterministic_mode()?;
        Ok(&mut self.scheduler)
    }

    fn require_deterministic_mode(&self) -> Result<(), HostError> {
        if self.mode == ExecutionMode::HostDependent {
            Err(HostError::DeterministicServiceUnavailable(self.mode))
        } else {
            Ok(())
        }
    }

    pub fn effects(&self) -> &[EffectRecord] {
        &self.effects
    }

    pub fn install_replay(&mut self, records: Vec<EffectRecord>) {
        self.replay = records;
        self.replay_cursor = 0;
    }

    pub fn invoke(
        &mut self,
        id: &CapabilityId,
        operation: &Operation,
        args: &[Value],
    ) -> Result<Vec<Value>, HostError> {
        self.capabilities
            .authorize(id, operation)
            .map_err(HostError::Authorization)?;
        if self.mode == ExecutionMode::RecordReplay {
            let record = self
                .replay
                .get(self.replay_cursor)
                .ok_or(HostError::ReplayExhausted)?;
            self.replay_cursor += 1;
            if record.capability != *id || record.operation != *operation {
                return Err(HostError::ReplayMismatch {
                    expected: self.replay_cursor - 1,
                    actual: self.effects.len(),
                });
            }
            return Ok(record.result.clone());
        }
        let implementation = self
            .implementations
            .get_mut(id)
            .ok_or_else(|| HostError::NotRegistered(id.clone()))?;
        let result = implementation
            .invoke(operation, args)
            .map_err(HostError::Capability)?;
        self.effects.push(EffectRecord {
            capability: id.clone(),
            operation: operation.clone(),
            effect: Effect::for_operation(id, operation),
            result: result.clone(),
        });
        Ok(result)
    }
}

/// Stub adapter for the future tpt-system integration.
pub mod tpt_system_adapter {
    pub fn is_available() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{
        CapabilityHostFunction, EffectRecord, ExecutionMode, Host, HostError, WasiAdapter,
        WasiAdapterError, WASI_SNAPSHOT_PREVIEW1_MODULE,
    };
    use tpt_wasm_capability::{
        Capability, CapabilityDescriptor, CapabilityError, CapabilityGrant, CapabilityId,
        HostError as CapabilityHostError, Operation, Permissions,
    };
    use tpt_wasm_format::{Export, ExportDesc, Function, Import, ImportDesc, Module};
    use tpt_wasm_runtime::{Config, Engine, RuntimeError};
    use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

    struct CountingCapability {
        id: CapabilityId,
        calls: Arc<AtomicUsize>,
    }

    impl Capability for CountingCapability {
        fn id(&self) -> &CapabilityId {
            &self.id
        }

        fn invoke(
            &mut self,
            _operation: &Operation,
            _args: &[Value],
        ) -> Result<Vec<Value>, CapabilityHostError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![Value::I32(42)])
        }
    }

    fn module_calling_import() -> Module {
        Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            }],
            imports: vec![Import {
                module: "env".into(),
                name: "call".into(),
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

    fn count_calls(counter: &Arc<AtomicUsize>) -> usize {
        counter.load(Ordering::SeqCst)
    }

    fn capability_descriptor() -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: CapabilityId::new("secrets"),
            operations: vec![Operation::execute("read")],
        }
    }

    #[test]
    fn wasi_adapter_installs_only_explicit_bindings() {
        let calls = Arc::new(AtomicUsize::new(0));
        let host = Arc::new(Mutex::new(Host::new(ExecutionMode::Deterministic)));
        {
            let mut host = host.lock().unwrap();
            host.register(
                Box::new(CountingCapability {
                    id: CapabilityId::new("secrets"),
                    calls: Arc::clone(&calls),
                }),
                capability_descriptor(),
            )
            .unwrap();
            host.grant(CapabilityGrant {
                capability: CapabilityId::new("secrets"),
                permissions: all_permissions(),
            })
            .unwrap();
        }
        let function_type = FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        };
        let mut adapter = WasiAdapter::new(Arc::clone(&host));
        assert_eq!(
            adapter.bind(
                "clock_time_get",
                CapabilityId::new("secrets"),
                Operation::execute("read"),
                function_type.clone(),
            ),
            Ok(())
        );
        assert_eq!(
            adapter.bind(
                "clock_time_get",
                CapabilityId::new("secrets"),
                Operation::execute("read"),
                function_type.clone(),
            ),
            Err(WasiAdapterError::DuplicateImport("clock_time_get".into()))
        );
        assert_eq!(
            adapter.bind(
                "",
                CapabilityId::new("secrets"),
                Operation::execute("read"),
                function_type.clone(),
            ),
            Err(WasiAdapterError::EmptyImportName)
        );
        assert_eq!(adapter.bindings().count(), 1);
        assert!(adapter.binding("clock_time_get").is_some());
        assert!(adapter.binding("random_get").is_none());

        let mut engine = Engine::new(Config::default()).unwrap();
        adapter.define(engine.linker_mut());
        let mut module = module_calling_import();
        module.imports[0].module = WASI_SNAPSHOT_PREVIEW1_MODULE.into();
        module.imports[0].name = "clock_time_get".into();
        let mut missing_module = module.clone();
        missing_module.imports[0].name = "random_get".into();
        assert!(matches!(
            engine.instantiate(missing_module),
            Err(RuntimeError::MissingImport { .. })
        ));
        let mut instance = engine.instantiate(module).unwrap();
        assert_eq!(
            instance.call("run", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
        assert_eq!(count_calls(&calls), 1);
    }

    #[test]
    fn ungranted_wasi_binding_does_not_reach_capability() {
        let calls = Arc::new(AtomicUsize::new(0));
        let host = Arc::new(Mutex::new(Host::new(ExecutionMode::Deterministic)));
        {
            let mut host = host.lock().unwrap();
            host.register(
                Box::new(CountingCapability {
                    id: CapabilityId::new("secrets"),
                    calls: Arc::clone(&calls),
                }),
                capability_descriptor(),
            )
            .unwrap();
        }
        let function_type = FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        };
        let mut adapter = WasiAdapter::new(Arc::clone(&host));
        adapter
            .bind(
                "clock_time_get",
                CapabilityId::new("secrets"),
                Operation::execute("read"),
                function_type,
            )
            .unwrap();
        let mut engine = Engine::new(Config::default()).unwrap();
        adapter.define(engine.linker_mut());
        let mut module = module_calling_import();
        module.imports[0].module = WASI_SNAPSHOT_PREVIEW1_MODULE.into();
        module.imports[0].name = "clock_time_get".into();
        let mut instance = engine.instantiate(module).unwrap();
        assert!(matches!(
            instance.call("run", Vec::new()),
            Err(RuntimeError::HostFunction { .. })
        ));
        assert_eq!(count_calls(&calls), 0);
        assert!(host.lock().unwrap().effects().is_empty());
    }

    #[test]
    fn malicious_module_cannot_use_ungranted_capability_and_hosts_are_isolated() {
        let left_calls = Arc::new(AtomicUsize::new(0));
        let right_calls = Arc::new(AtomicUsize::new(0));
        let left_host = Arc::new(Mutex::new(Host::new(ExecutionMode::Deterministic)));
        let right_host = Arc::new(Mutex::new(Host::new(ExecutionMode::Deterministic)));
        {
            let mut host = left_host.lock().unwrap();
            host.register(
                Box::new(CountingCapability {
                    id: CapabilityId::new("secrets"),
                    calls: Arc::clone(&left_calls),
                }),
                capability_descriptor(),
            )
            .unwrap();
        }
        {
            let mut host = right_host.lock().unwrap();
            host.register(
                Box::new(CountingCapability {
                    id: CapabilityId::new("secrets"),
                    calls: Arc::clone(&right_calls),
                }),
                capability_descriptor(),
            )
            .unwrap();
            host.grant(CapabilityGrant {
                capability: CapabilityId::new("secrets"),
                permissions: all_permissions(),
            })
            .unwrap();
        }

        let function_type = FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        };
        let mut denied_engine = Engine::new(Config::default()).unwrap();
        denied_engine.linker_mut().define_function(
            "env",
            "call",
            CapabilityHostFunction::new(
                Arc::clone(&left_host),
                CapabilityId::new("secrets"),
                Operation::execute("read"),
                function_type.clone(),
            ),
        );
        let mut denied = denied_engine.instantiate(module_calling_import()).unwrap();
        assert!(matches!(
            denied.call("run", Vec::new()),
            Err(RuntimeError::HostFunction { .. })
        ));
        assert_eq!(count_calls(&left_calls), 0);
        assert!(left_host.lock().unwrap().effects().is_empty());

        let mut allowed_engine = Engine::new(Config::default()).unwrap();
        allowed_engine.linker_mut().define_function(
            "env",
            "call",
            CapabilityHostFunction::new(
                Arc::clone(&right_host),
                CapabilityId::new("secrets"),
                Operation::execute("read"),
                function_type,
            ),
        );
        let mut allowed = allowed_engine.instantiate(module_calling_import()).unwrap();
        assert_eq!(
            allowed.call("run", Vec::new()).unwrap(),
            vec![Value::I32(42)]
        );
        assert_eq!(count_calls(&right_calls), 1);
        assert_eq!(count_calls(&left_calls), 0);
        assert_eq!(right_host.lock().unwrap().effects().len(), 1);
        assert!(left_host.lock().unwrap().effects().is_empty());
    }

    struct FakeCapability {
        id: CapabilityId,
        calls: usize,
    }

    impl FakeCapability {
        fn new(id: CapabilityId) -> Self {
            Self { id, calls: 0 }
        }
    }

    impl Capability for FakeCapability {
        fn id(&self) -> &CapabilityId {
            &self.id
        }

        fn invoke(
            &mut self,
            operation: &Operation,
            _args: &[Value],
        ) -> Result<Vec<Value>, CapabilityHostError> {
            self.calls += 1;
            if operation.as_str() == "fail" {
                return Err(CapabilityHostError("capability failure".into()));
            }
            Ok(vec![Value::I32(self.calls as i32)])
        }
    }

    fn id() -> CapabilityId {
        CapabilityId::new("math")
    }

    fn descriptor() -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: id(),
            operations: vec![
                Operation::read("read"),
                Operation::write("write"),
                Operation::execute("execute"),
                Operation::execute("fail"),
            ],
        }
    }

    fn all_permissions() -> Permissions {
        Permissions {
            read: true,
            write: true,
            execute: true,
        }
    }

    #[test]
    fn ungranted_and_undergranted_operations_cannot_invoke() {
        let mut host = Host::new(ExecutionMode::Deterministic);
        host.register(Box::new(FakeCapability::new(id())), descriptor())
            .unwrap();
        assert!(matches!(
            host.invoke(&id(), &Operation::read("read"), &[]),
            Err(HostError::Authorization(
                CapabilityError::UnknownCapability(_)
            ))
        ));
        host.grant(CapabilityGrant {
            capability: id(),
            permissions: Permissions {
                read: true,
                ..Permissions::default()
            },
        })
        .unwrap();
        assert!(matches!(
            host.invoke(&id(), &Operation::write("write"), &[]),
            Err(HostError::Authorization(
                CapabilityError::PermissionDenied { .. }
            ))
        ));
        assert!(host.effects().is_empty());
    }

    #[test]
    fn matching_grant_invokes_and_revocation_removes_authority() {
        let mut host = Host::new(ExecutionMode::Deterministic);
        host.register(Box::new(FakeCapability::new(id())), descriptor())
            .unwrap();
        let operation = Operation::execute("execute");
        host.grant(CapabilityGrant {
            capability: id(),
            permissions: all_permissions(),
        })
        .unwrap();
        assert_eq!(host.invoke(&id(), &operation, &[]), Ok(vec![Value::I32(1)]));
        assert_eq!(host.effects().len(), 1);
        assert!(host.revoke(&id()));
        assert!(matches!(
            host.invoke(&id(), &operation, &[]),
            Err(HostError::Authorization(
                CapabilityError::UnknownCapability(_)
            ))
        ));
    }

    #[test]
    fn replay_is_ordered_and_does_not_call_live_capability() {
        let operation = Operation::execute("execute");
        let records = vec![EffectRecord {
            capability: id(),
            operation: operation.clone(),
            effect: tpt_wasm_capability::Effect::Execute(id()),
            result: vec![Value::I32(41)],
        }];
        let mut host = Host::new(ExecutionMode::RecordReplay);
        host.register(Box::new(FakeCapability::new(id())), descriptor())
            .unwrap();
        host.grant(CapabilityGrant {
            capability: id(),
            permissions: all_permissions(),
        })
        .unwrap();
        host.install_replay(records);
        assert_eq!(
            host.invoke(&id(), &operation, &[]),
            Ok(vec![Value::I32(41)])
        );
        assert_eq!(
            host.invoke(&id(), &operation, &[]),
            Err(HostError::ReplayExhausted)
        );
    }

    #[test]
    fn implementation_errors_do_not_create_effect_records() {
        let mut host = Host::new(ExecutionMode::Deterministic);
        host.register(Box::new(FakeCapability::new(id())), descriptor())
            .unwrap();
        host.grant(CapabilityGrant {
            capability: id(),
            permissions: all_permissions(),
        })
        .unwrap();
        assert!(matches!(
            host.invoke(&id(), &Operation::execute("fail"), &[]),
            Err(HostError::Capability(_))
        ));
        assert!(host.effects().is_empty());
    }
}

#[test]
fn deterministic_services_repeat_and_isolate() {
    let mut left = Host::new_with_seed(ExecutionMode::Deterministic, 42);
    let mut right = Host::new_with_seed(ExecutionMode::Deterministic, 42);
    assert_eq!(left.advance_virtual_time(7), Ok(7));
    assert_eq!(right.advance_virtual_time(7), Ok(7));
    assert_eq!(left.next_random_u64(), right.next_random_u64());
    left.filesystem_mut()
        .unwrap()
        .write("input.txt", b"same".to_vec());
    assert_eq!(left.filesystem().read("input.txt"), Some(&b"same"[..]));
    assert_eq!(right.filesystem().read("input.txt"), None);
    assert_eq!(left.network_policy(), NetworkPolicy::Prohibited);
    assert_eq!(left.check_network(), Err(HostError::NetworkProhibited));
}

#[test]
fn scheduler_orders_by_priority_then_insertion() {
    let mut host = Host::new(ExecutionMode::Deterministic);
    let scheduler = host.scheduler_mut().unwrap();
    scheduler.schedule(1, "low").unwrap();
    scheduler.schedule(0, "first").unwrap();
    scheduler.schedule(0, "second").unwrap();
    assert_eq!(scheduler.pop().as_deref(), Some("first"));
    assert_eq!(scheduler.pop().as_deref(), Some("second"));
    assert_eq!(scheduler.pop().as_deref(), Some("low"));
}

#[test]
fn host_dependent_mode_does_not_expose_deterministic_services() {
    let mut host = Host::new(ExecutionMode::HostDependent);
    assert_eq!(
        host.advance_virtual_time(1),
        Err(HostError::DeterministicServiceUnavailable(
            ExecutionMode::HostDependent
        ))
    );
    assert_eq!(
        host.next_random_u64(),
        Err(HostError::DeterministicServiceUnavailable(
            ExecutionMode::HostDependent
        ))
    );
    assert!(matches!(
        host.filesystem_mut(),
        Err(HostError::DeterministicServiceUnavailable(_))
    ));
    assert!(matches!(
        host.scheduler_mut(),
        Err(HostError::DeterministicServiceUnavailable(_))
    ));
    assert_eq!(
        host.set_network_policy(NetworkPolicy::HostDependent),
        Ok(())
    );
    assert_eq!(host.check_network(), Ok(()));
}
