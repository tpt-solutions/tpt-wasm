// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! TPT capability-oriented host.
//!
//! Core principle: No ambient authority.
//!
//! Wasm programs may only interact with host resources through
//! explicitly granted capabilities. The host does not expose
//! filesystem, sockets, or POSIX by default.

use tpt_wasm_capability::{CapabilitySet, CapabilityGrant};
use tpt_wasm_resource::ResourceTable;

/// Execution mode for the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    #[default]
    HostDependent,
    Deterministic,
    RecordReplay,
}

/// The TPT host — mediates all Wasm ↔ host interaction.
pub struct Host {
    capabilities: CapabilitySet,
    resources: ResourceTable,
    mode: ExecutionMode,
}

impl Host {
    pub fn new(mode: ExecutionMode) -> Self {
        Self {
            capabilities: CapabilitySet::default(),
            resources: ResourceTable::new(),
            mode,
        }
    }

    pub fn grant(&mut self, grant: CapabilityGrant) {
        self.capabilities.grant(grant);
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
}

/// Stub adapter for the future tpt-system integration.
///
/// tpt-wasm-host → tpt-system adapter → TPT system
///
/// tpt-system does not exist yet. This adapter prevents the core runtime
/// from ever taking a direct dependency on the TPT ecosystem.
pub mod tpt_system_adapter {
    pub fn is_available() -> bool {
        false
    }
}
