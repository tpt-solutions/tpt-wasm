// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Capability types and trait.
//!
//! The host has no ambient authority. All host interactions go through
//! explicit capability grants.
//!
//! Authority(WasmInstance) ⊆ GrantedCapabilities(WasmInstance)

use std::{collections::HashMap, fmt};

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
pub struct Operation {
    name: String,
    permission: Permission,
}

impl Operation {
    /// Creates an execute operation.
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_permission(name, Permission::Execute)
    }

    pub fn read(name: impl Into<String>) -> Self {
        Self::with_permission(name, Permission::Read)
    }

    pub fn write(name: impl Into<String>) -> Self {
        Self::with_permission(name, Permission::Write)
    }

    pub fn execute(name: impl Into<String>) -> Self {
        Self::with_permission(name, Permission::Execute)
    }

    pub fn with_permission(name: impl Into<String>, permission: Permission) -> Self {
        Self {
            name: name.into(),
            permission,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }

    pub fn permission(&self) -> Permission {
        self.permission
    }
}

/// Describes what operations a capability supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub operations: Vec<Operation>,
}

impl CapabilityDescriptor {
    pub fn supports(&self, operation: &Operation) -> bool {
        self.operations
            .iter()
            .any(|candidate| candidate == operation)
    }
}

/// A grant of a capability with specific permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGrant {
    pub capability: CapabilityId,
    pub permissions: Permissions,
}

/// Permissions associated with a capability grant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    Read,
    Write,
    Execute,
}

impl Permissions {
    pub fn allows(&self, permission: Permission) -> bool {
        match permission {
            Permission::Read => self.read,
            Permission::Write => self.write,
            Permission::Execute => self.execute,
        }
    }
}

/// Errors produced while registering or authorizing a capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    DuplicateCapability(CapabilityId),
    UnknownCapability(CapabilityId),
    UnknownOperation {
        capability: CapabilityId,
        operation: Operation,
    },
    PermissionDenied {
        capability: CapabilityId,
        operation: Operation,
        permission: Permission,
    },
    DescriptorMismatch(CapabilityId),
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CapabilityError {}

/// An error returned by a host capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

/// The set of capabilities available to a host instance.
#[derive(Debug, Default)]
pub struct CapabilitySet {
    descriptors: HashMap<CapabilityId, CapabilityDescriptor>,
    grants: HashMap<CapabilityId, CapabilityGrant>,
}

impl CapabilitySet {
    pub fn register(&mut self, descriptor: CapabilityDescriptor) -> Result<(), CapabilityError> {
        if self.descriptors.contains_key(&descriptor.id) {
            return Err(CapabilityError::DuplicateCapability(descriptor.id));
        }
        self.descriptors.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }

    pub fn grant(&mut self, grant: CapabilityGrant) -> Result<(), CapabilityError> {
        if !self.descriptors.contains_key(&grant.capability) {
            return Err(CapabilityError::UnknownCapability(grant.capability));
        }
        self.grants.insert(grant.capability.clone(), grant);
        Ok(())
    }

    pub fn revoke(&mut self, id: &CapabilityId) -> bool {
        self.grants.remove(id).is_some()
    }

    pub fn has(&self, id: &CapabilityId) -> bool {
        self.grants.contains_key(id)
    }

    pub fn descriptor(&self, id: &CapabilityId) -> Option<&CapabilityDescriptor> {
        self.descriptors.get(id)
    }

    pub fn authorize(
        &self,
        id: &CapabilityId,
        operation: &Operation,
    ) -> Result<(), CapabilityError> {
        let descriptor = self
            .descriptors
            .get(id)
            .ok_or_else(|| CapabilityError::UnknownCapability(id.clone()))?;
        if !descriptor.supports(operation) {
            return Err(CapabilityError::UnknownOperation {
                capability: id.clone(),
                operation: operation.clone(),
            });
        }
        let grant = self
            .grants
            .get(id)
            .ok_or_else(|| CapabilityError::UnknownCapability(id.clone()))?;
        if !grant.permissions.allows(operation.permission()) {
            return Err(CapabilityError::PermissionDenied {
                capability: id.clone(),
                operation: operation.clone(),
                permission: operation.permission(),
            });
        }
        Ok(())
    }
}

/// A capability that can be invoked by WebAssembly programs.
pub trait Capability: Send + Sync {
    fn id(&self) -> &CapabilityId;

    fn invoke(&mut self, operation: &Operation, args: &[Value]) -> Result<Vec<Value>, HostError>;
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilityDescriptor, CapabilityError, CapabilityGrant, CapabilityId, CapabilitySet,
        Operation, Permission, Permissions,
    };

    fn id() -> CapabilityId {
        CapabilityId::new("filesystem")
    }

    #[test]
    fn registration_grant_revocation_and_permissions_are_explicit() {
        let mut set = CapabilitySet::default();
        let read = Operation::read("read");
        let write = Operation::write("write");
        set.register(CapabilityDescriptor {
            id: id(),
            operations: vec![read.clone(), write.clone()],
        })
        .unwrap();
        assert_eq!(
            set.grant(CapabilityGrant {
                capability: id(),
                permissions: Permissions {
                    read: true,
                    ..Permissions::default()
                },
            }),
            Ok(())
        );
        assert_eq!(set.authorize(&id(), &read), Ok(()));
        assert_eq!(
            set.authorize(&id(), &write),
            Err(CapabilityError::PermissionDenied {
                capability: id(),
                operation: write,
                permission: Permission::Write,
            })
        );
        assert!(set.revoke(&id()));
        assert_eq!(
            set.authorize(&id(), &read),
            Err(CapabilityError::UnknownCapability(id()))
        );
    }

    #[test]
    fn unsupported_operations_and_duplicate_registration_are_rejected() {
        let mut set = CapabilitySet::default();
        let descriptor = CapabilityDescriptor {
            id: id(),
            operations: vec![Operation::execute("run")],
        };
        set.register(descriptor.clone()).unwrap();
        assert_eq!(
            set.register(descriptor),
            Err(CapabilityError::DuplicateCapability(id()))
        );
        assert_eq!(
            set.authorize(&id(), &Operation::read("read")),
            Err(CapabilityError::UnknownOperation {
                capability: id(),
                operation: Operation::read("read"),
            })
        );
    }
}

/// Explicit host effects for observability and verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Pure,
    Read(crate::CapabilityId),
    Write(crate::CapabilityId),
    Execute(crate::CapabilityId),
    External(crate::CapabilityId),
}

impl Effect {
    pub fn for_operation(id: &CapabilityId, operation: &Operation) -> Self {
        match operation.permission() {
            Permission::Read => Self::Read(id.clone()),
            Permission::Write => Self::Write(id.clone()),
            Permission::Execute => Self::Execute(id.clone()),
        }
    }
}
