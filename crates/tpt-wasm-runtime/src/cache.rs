// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The module compilation cache.
//!
//! The expensive part of compiling a module is not the lowering but the optimizer
//! and the contract check that follows it. The cache keeps the *optimized IR* for a
//! module that has been compiled before, so instantiating the same bytes again
//! skips those passes. It deliberately does not keep the compiled backend: a
//! `BaselineModule` owns per-instance memory, table, and global handles, so reusing
//! one would share an instance's state with the next. The IR carries none of that.
//!
//! ## What the key must cover
//!
//! A cached entry is only correct for a request that would have produced the same
//! IR. The key therefore names every input that can change that IR:
//!
//! * the module bytes, compared exactly, so two modules cannot share an entry by
//!   colliding on a hash;
//! * the engine mode and feature set, which select the pipeline;
//! * the engine, semantics, and compiler versions, so an upgrade of any of them
//!   misses rather than replaying IR that the new code would not produce;
//! * the target architecture, so a future native backend cannot be handed another
//!   architecture's artifact.
//!
//! The digest is a 64-bit FNV-1a hash of the bytes. It is for identifying a module
//! in logs and reports only. It is never what decides a hit.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tpt_wasm_ir::IrModule;

use crate::{EngineMode, Features};

/// The version of the Wasm semantics the cached IR is correct for. It is kept in
/// step with `[workspace.metadata.tpt-wasm] semantics_version` in the workspace
/// manifest.
pub const SEMANTICS_VERSION: &str = "0.1";

/// The runtime's own version. A new release of the engine misses every entry.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of the optimizer that produced the cached IR.
pub const COMPILER_VERSION: &str = tpt_wasm_opt::COMPILER_VERSION;

/// The architecture the artifact is for. The portable executor is the same on
/// every target, but the key names the target so that a native backend added later
/// does not inherit an entry from another one.
pub const TARGET_ARCH: &str = std::env::consts::ARCH;

/// How many compiled modules an engine keeps before it starts evicting the oldest.
pub const DEFAULT_CAPACITY: usize = 32;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 64-bit FNV-1a over `bytes`.
pub fn module_digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Everything that decides which IR a module compiles to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompilationKey {
    digest: u64,
    module: Vec<u8>,
    engine_mode: EngineMode,
    features: Features,
    engine_version: &'static str,
    semantics_version: &'static str,
    compiler_version: &'static str,
    target_arch: &'static str,
}

impl CompilationKey {
    /// The key for `bytes` compiled under `engine_mode` and `features`, with the
    /// versions and target this build was made for.
    pub fn new(bytes: &[u8], engine_mode: EngineMode, features: Features) -> Self {
        Self {
            digest: module_digest(bytes),
            module: bytes.to_vec(),
            engine_mode,
            features,
            engine_version: ENGINE_VERSION,
            semantics_version: SEMANTICS_VERSION,
            compiler_version: COMPILER_VERSION,
            target_arch: TARGET_ARCH,
        }
    }

    /// The module digest, for identifying the entry in logs.
    pub fn digest(&self) -> u64 {
        self.digest
    }
}

/// Counters the embedder can read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Lookups answered from the cache.
    pub hits: u64,
    /// Lookups that found nothing and so forced a compilation.
    pub misses: u64,
    /// Entries currently held.
    pub entries: usize,
}

#[derive(Debug, Default)]
struct Entries {
    map: HashMap<CompilationKey, Arc<IrModule>>,
    /// Insertion order, oldest first, for eviction.
    order: VecDeque<CompilationKey>,
}

/// A bounded, thread-safe map from compilation key to optimized IR.
///
/// A poisoned lock is treated as an empty cache rather than a panic: the cache is
/// only ever an optimization, so losing it must not fail an instantiation.
#[derive(Debug)]
pub struct CompilationCache {
    entries: Mutex<Entries>,
    capacity: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CompilationCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            capacity,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// The cached IR for `key`, counting the lookup as a hit or a miss.
    pub fn get(&self, key: &CompilationKey) -> Option<Arc<IrModule>> {
        let found = self
            .entries
            .lock()
            .ok()
            .and_then(|entries| entries.map.get(key).cloned());
        let counter = if found.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        counter.fetch_add(1, Ordering::Relaxed);
        found
    }

    /// Store `ir` under `key`, evicting the oldest entry if the cache is full.
    /// Storing under a key already present replaces its IR without changing its
    /// place in the eviction order.
    pub fn insert(&self, key: CompilationKey, ir: Arc<IrModule>) {
        if self.capacity == 0 {
            return;
        }
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.map.insert(key.clone(), ir).is_some() {
            return;
        }
        entries.order.push_back(key);
        while entries.order.len() > self.capacity {
            if let Some(oldest) = entries.order.pop_front() {
                entries.map.remove(&oldest);
            }
        }
    }

    pub fn stats(&self) -> CacheStats {
        let entries = self
            .entries
            .lock()
            .map(|entries| entries.map.len())
            .unwrap_or(0);
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features() -> Features {
        Features::default()
    }

    #[test]
    fn the_digest_is_fnv1a_and_distinguishes_inputs() {
        // The empty input is the offset basis, and one byte changes the hash.
        assert_eq!(module_digest(b""), FNV_OFFSET_BASIS);
        assert_ne!(module_digest(b"\0asm"), module_digest(b"\0asn"));
    }

    #[test]
    fn the_same_inputs_give_equal_keys_and_any_change_gives_a_different_key() {
        let base = CompilationKey::new(b"\0asm", EngineMode::Optimizing, features());
        assert_eq!(
            base,
            CompilationKey::new(b"\0asm", EngineMode::Optimizing, features())
        );
        assert_ne!(
            base,
            CompilationKey::new(b"\0asx", EngineMode::Optimizing, features())
        );
        assert_ne!(
            base,
            CompilationKey::new(b"\0asm", EngineMode::Baseline, features())
        );
        let mut other = features();
        other.bulk_memory = !other.bulk_memory;
        assert_ne!(
            base,
            CompilationKey::new(b"\0asm", EngineMode::Optimizing, other)
        );
    }

    #[test]
    fn a_hit_returns_the_stored_ir_and_counts_both_outcomes() {
        let cache = CompilationCache::new(4);
        let key = CompilationKey::new(b"\0asm", EngineMode::Optimizing, features());
        assert!(cache.get(&key).is_none());
        cache.insert(key.clone(), Arc::new(IrModule::default()));
        assert!(cache.get(&key).is_some());
        assert_eq!(
            cache.stats(),
            CacheStats {
                hits: 1,
                misses: 1,
                entries: 1
            }
        );
    }

    #[test]
    fn the_oldest_entry_is_evicted_first_and_replacement_keeps_its_place() {
        let cache = CompilationCache::new(2);
        let key = |bytes: &[u8]| CompilationKey::new(bytes, EngineMode::Optimizing, features());
        let (a, b, c) = (key(b"a"), key(b"b"), key(b"c"));
        let ir = || Arc::new(IrModule::default());
        cache.insert(a.clone(), ir());
        cache.insert(b.clone(), ir());
        // Replacing `a` must not make it the newest, so inserting `c` still evicts it.
        cache.insert(a.clone(), ir());
        cache.insert(c.clone(), ir());
        assert!(
            cache.get(&a).is_none(),
            "the oldest entry should be evicted"
        );
        assert!(cache.get(&b).is_some());
        assert!(cache.get(&c).is_some());
        assert_eq!(cache.stats().entries, 2);
    }

    #[test]
    fn a_zero_capacity_cache_stores_nothing() {
        let cache = CompilationCache::new(0);
        let key = CompilationKey::new(b"a", EngineMode::Optimizing, features());
        cache.insert(key.clone(), Arc::new(IrModule::default()));
        assert!(cache.get(&key).is_none());
    }
}
