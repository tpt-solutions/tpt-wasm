// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Reading and writing an instance's memory from a host function.
//!
//! A host function that a component lowers may need the bytes behind a string or a list it is
//! given. Those bytes live in the memory the canonical ABI names, which belongs to some instance
//! other than the caller. A [`MemoryView`] is that memory itself, not a copy: a write through it is
//! seen by the instance's later calls, as a write by the instance would be.
//!
//! A view does not wait for a memory. A memory that a running call holds is reported as out of
//! range rather than blocking, so a host function that reads a memory its own caller is running
//! with gets a clear error instead of a deadlock.

use std::sync::{Arc, Mutex};

use tpt_wasm_codegen::SharedMemory;
use tpt_wasm_micro::store::Store;
use tpt_wasm_types::GuestMemory;

use crate::{Instance, RuntimeError};

/// A memory of an instance, to be read and written from a host function.
#[derive(Clone)]
pub struct MemoryView {
    target: Target,
}

#[derive(Clone)]
enum Target {
    /// A memory of the store the interpreter runs on, by its address.
    Store {
        store: Arc<Mutex<Store>>,
        address: u32,
    },
    /// The memory of a module compiled to the baseline.
    Baseline(SharedMemory),
}

impl std::fmt::Debug for MemoryView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.target {
            Target::Store { address, .. } => write!(f, "MemoryView(store {address})"),
            Target::Baseline(_) => write!(f, "MemoryView(baseline)"),
        }
    }
}

impl GuestMemory for MemoryView {
    fn size(&self) -> u64 {
        match &self.target {
            Target::Store { store, address } => store
                .try_lock()
                .ok()
                .and_then(|store| store.memory(*address).ok().map(|m| m.pages() * 65_536))
                .unwrap_or(0),
            Target::Baseline(handle) => handle
                .try_lock()
                .map(|memory| GuestMemory::size(&*memory))
                .unwrap_or(0),
        }
    }

    fn read(&self, offset: u64, length: u64) -> Option<Vec<u8>> {
        let length = usize::try_from(length).ok()?;
        match &self.target {
            Target::Store { store, address } => {
                let store = store.try_lock().ok()?;
                let memory = store.memory(*address).ok()?;
                memory.read(offset, length).ok().map(<[u8]>::to_vec)
            }
            Target::Baseline(handle) => {
                let memory = handle.try_lock().ok()?;
                GuestMemory::read(&*memory, offset, length as u64)
            }
        }
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> bool {
        match &self.target {
            Target::Store { store, address } => {
                let Ok(mut store) = store.try_lock() else {
                    return false;
                };
                match store.memory_mut(*address) {
                    Ok(memory) => memory.write(offset, bytes).is_ok(),
                    Err(_) => false,
                }
            }
            Target::Baseline(handle) => match handle.try_lock() {
                Ok(mut memory) => GuestMemory::write(&mut *memory, offset, bytes),
                Err(_) => false,
            },
        }
    }
}

impl Instance {
    /// A view of the memory this instance exports as `name`, for a host function to read and
    /// write. See [`MemoryView`].
    pub fn memory_view(&self, name: &str) -> Result<MemoryView, RuntimeError> {
        let index = match self.exports.get(name) {
            Some(crate::ExportDesc::Memory(index)) => *index,
            Some(_) => {
                return Err(RuntimeError::TypeMismatch {
                    expected: "memory export".into(),
                    actual: "non-memory export".into(),
                })
            }
            None => return Err(RuntimeError::UnknownExport(name.to_owned())),
        };
        if let Some(module) = self.baseline.as_ref() {
            if index != 0 {
                return Err(RuntimeError::UnsupportedFeature(
                    "memory index other than 0",
                ));
            }
            let handle = module
                .memory_handle()
                .ok_or(RuntimeError::UnsupportedFeature("module has no memory"))?;
            return Ok(MemoryView {
                target: Target::Baseline(handle),
            });
        }
        let address = {
            let store = crate::lock_store(&self.context.store)?;
            *store
                .instance(self.index)
                .map_err(RuntimeError::Store)?
                .memory_addrs
                .get(index as usize)
                .ok_or(RuntimeError::Store(
                    tpt_wasm_micro::store::StoreError::UnknownMemory(index),
                ))?
        };
        Ok(MemoryView {
            target: Target::Store {
                store: Arc::clone(&self.context.store),
                address,
            },
        })
    }
}
