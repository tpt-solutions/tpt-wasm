// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Capability types and trait.
//!
//! The host has no ambient authority. All host interactions go through
//! explicit capability grants.
//!
//! Authority(WasmInstance) ⊆ GrantedCapabilities(WasmInstance)

use tpt_wasm_types::Value;

/// An opaque capability identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityId(String);

impl CapabilityId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An operation that a capability can perform.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Operation(String);

impl Operation {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// Describes what operations a capability supports.
#[derive(Debug, Clone)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub operations: Vec<Operation>,
}

/// A grant of a capability with specific permissions.
#[derive(Debug, Clone)]
pub struct CapabilityGrant {
    pub capability: CapabilityId,
    pub permissions: Permissions,
}

/// Permissions associated with a capability grant.
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

/// An error returned by a host capability.
#[derive(Debug, Clone)]
pub struct HostError(pub String);

/// The set of capabilities available to a host instance.
#[derive(Debug, Default)]
pub struct CapabilitySet {
    grants: Vec<CapabilityGrant>,
}

impl CapabilitySet {
    pub fn grant(&mut self, grant: CapabilityGrant) {
        self.grants.push(grant);
    }

    pub fn has(&self, id: &CapabilityId) -> bool {
        self.grants.iter().any(|g| &g.capability == id)
    }
}

/// A capability that can be invoked by WebAssembly programs.
pub trait Capability {
    fn id(&self) -> &CapabilityId;

    fn invoke(
        &mut self,
        operation: &Operation,
        args: &[Value],
    ) -> Result<Vec<Value>, HostError>;
}

/// Explicit host effects for observability and verification.
#[derive(Debug, Clone)]
pub enum Effect {
    Pure,
    Read(crate::CapabilityId),
    Write(crate::CapabilityId),
    Execute(crate::CapabilityId),
    External(crate::CapabilityId),
}
