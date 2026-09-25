// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Capability-backed WASI adapter facade for tpt-wasm.
//!
//! This crate provides explicit Preview 1 import labels and a small adapter
//! facade. It does not provide a complete WASI ABI, POSIX access, or implicit
//! filesystem, clock, random, process, or network authority.

use std::sync::{Arc, Mutex};

use tpt_wasm_capability::{CapabilityId, Operation};
use tpt_wasm_host::Host;
pub use tpt_wasm_host::{WasiAdapterError, WasiBinding, WASI_SNAPSHOT_PREVIEW1_MODULE};
use tpt_wasm_runtime::Linker;
use tpt_wasm_types::FunctionType;

/// The module name used by Preview 1-style imports.
pub const PREVIEW1_MODULE: &str = WASI_SNAPSHOT_PREVIEW1_MODULE;

/// Common Preview 1 import labels. The embedder supplies the signature and
/// capability operation explicitly; this type does not grant authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview1Function {
    ArgsGet,
    ArgsSizesGet,
    ClockTimeGet,
    EnvironGet,
    EnvironSizesGet,
    FdClose,
    FdRead,
    FdWrite,
    ProcExit,
    RandomGet,
}

impl Preview1Function {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ArgsGet => "args_get",
            Self::ArgsSizesGet => "args_sizes_get",
            Self::ClockTimeGet => "clock_time_get",
            Self::EnvironGet => "environ_get",
            Self::EnvironSizesGet => "environ_sizes_get",
            Self::FdClose => "fd_close",
            Self::FdRead => "fd_read",
            Self::FdWrite => "fd_write",
            Self::ProcExit => "proc_exit",
            Self::RandomGet => "random_get",
        }
    }
}

/// Explicit Preview 1 bindings installed into a runtime linker.
pub struct WasiAdapter {
    inner: tpt_wasm_host::WasiAdapter,
}

impl WasiAdapter {
    pub fn new(host: Arc<Mutex<Host>>) -> Self {
        Self {
            inner: tpt_wasm_host::WasiAdapter::new(host),
        }
    }

    pub fn bind(
        &mut self,
        import: impl Into<String>,
        capability: CapabilityId,
        operation: Operation,
        function_type: FunctionType,
    ) -> Result<(), WasiAdapterError> {
        self.inner
            .bind(import, capability, operation, function_type)
    }

    pub fn bind_function(
        &mut self,
        function: Preview1Function,
        capability: CapabilityId,
        operation: Operation,
        function_type: FunctionType,
    ) -> Result<(), WasiAdapterError> {
        self.bind(function.name(), capability, operation, function_type)
    }

    pub fn binding(&self, import: &str) -> Option<&WasiBinding> {
        self.inner.binding(import)
    }

    pub fn bindings(&self) -> impl Iterator<Item = &WasiBinding> {
        self.inner.bindings()
    }

    pub fn define(&self, linker: &mut Linker) {
        self.inner.define(linker);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{Preview1Function, WasiAdapter};
    use tpt_wasm_capability::{
        Capability, CapabilityDescriptor, CapabilityGrant, CapabilityId,
        HostError as CapabilityHostError, Operation, Permissions,
    };
    use tpt_wasm_format::{Export, ExportDesc, Function, Import, ImportDesc, Module};
    use tpt_wasm_host::{ExecutionMode, Host};
    use tpt_wasm_runtime::{Config, Engine, RuntimeError};
    use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

    struct TestCapability {
        id: CapabilityId,
        calls: Arc<AtomicUsize>,
    }

    impl Capability for TestCapability {
        fn id(&self) -> &CapabilityId {
            &self.id
        }

        fn invoke(
            &mut self,
            _operation: &Operation,
            _args: &[Value],
        ) -> Result<Vec<Value>, CapabilityHostError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![Value::I32(7)])
        }
    }

    fn signature() -> FunctionType {
        FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn module(import: &str) -> Module {
        Module {
            types: vec![signature()],
            imports: vec![Import {
                module: super::PREVIEW1_MODULE.into(),
                name: import.into(),
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

    #[test]
    fn configured_preview1_import_is_explicit_and_callable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let host = Arc::new(Mutex::new(Host::new(ExecutionMode::Deterministic)));
        {
            let mut host = host.lock().unwrap();
            host.register(
                Box::new(TestCapability {
                    id: CapabilityId::new("preview1"),
                    calls: Arc::clone(&calls),
                }),
                CapabilityDescriptor {
                    id: CapabilityId::new("preview1"),
                    operations: vec![Operation::execute("call")],
                },
            )
            .unwrap();
            host.grant(CapabilityGrant {
                capability: CapabilityId::new("preview1"),
                permissions: Permissions {
                    execute: true,
                    ..Permissions::default()
                },
            })
            .unwrap();
        }

        let mut adapter = WasiAdapter::new(Arc::clone(&host));
        adapter
            .bind_function(
                Preview1Function::ClockTimeGet,
                CapabilityId::new("preview1"),
                Operation::execute("call"),
                signature(),
            )
            .unwrap();
        let mut engine = Engine::new(Config::default()).unwrap();
        adapter.define(engine.linker_mut());
        let mut instance = engine
            .instantiate(module(Preview1Function::ClockTimeGet.name()))
            .unwrap();
        assert_eq!(
            instance.call("run", Vec::new()).unwrap(),
            vec![Value::I32(7)]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unbound_preview1_import_is_not_installed() {
        let host = Arc::new(Mutex::new(Host::new(ExecutionMode::Deterministic)));
        let adapter = WasiAdapter::new(Arc::clone(&host));
        let mut engine = Engine::new(Config::default()).unwrap();
        adapter.define(engine.linker_mut());
        assert!(matches!(
            engine.instantiate(module("random_get")),
            Err(RuntimeError::MissingImport { .. })
        ));
    }
}
