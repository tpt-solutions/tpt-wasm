// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Host resource table and opaque resource handles.
//!
//! Resources are accessed through opaque u64 handles.
//! Rust pointers and object addresses are never exposed to Wasm.
//!
//! Wasm → ResourceId → ResourceTable → Host Resource

use std::collections::HashMap;

/// An opaque handle to a host resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceId(u64);

impl ResourceId {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// A table mapping resource handles to boxed host resources.
pub struct ResourceTable {
    next_id: u64,
    entries: HashMap<ResourceId, Box<dyn std::any::Any + Send + Sync>>,
}

impl ResourceTable {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            entries: HashMap::new(),
        }
    }

    /// Insert a resource and return its opaque handle.
    pub fn insert<T: std::any::Any + Send + Sync + 'static>(&mut self, resource: T) -> ResourceId {
        let id = ResourceId(self.next_id);
        self.next_id += 1;
        self.entries.insert(id, Box::new(resource));
        id
    }

    /// Get a reference to a resource by handle.
    pub fn get<T: std::any::Any + 'static>(&self, id: ResourceId) -> Option<&T> {
        self.entries.get(&id)?.downcast_ref()
    }

    /// Get a mutable reference to a resource by handle.
    pub fn get_mut<T: std::any::Any + 'static>(&mut self, id: ResourceId) -> Option<&mut T> {
        self.entries.get_mut(&id)?.downcast_mut()
    }

    /// Remove and return a resource by handle.
    pub fn remove<T: std::any::Any + 'static>(&mut self, id: ResourceId) -> Option<T> {
        let boxed = self.entries.remove(&id)?;
        match boxed.downcast::<T>() {
            Ok(v) => Some(*v),
            Err(b) => {
                self.entries.insert(id, b);
                None
            }
        }
    }

    pub fn contains(&self, id: ResourceId) -> bool {
        self.entries.contains_key(&id)
    }
}

impl Default for ResourceTable {
    fn default() -> Self {
        Self::new()
    }
}
