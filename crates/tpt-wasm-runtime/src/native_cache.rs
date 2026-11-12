// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! An on-disk cache of compiled native code, so that a module compiled once is not compiled again
//! by a later process.
//!
//! A cached entry is a module artifact (see `tpt-wasm-aot`), and loading one installs machine code,
//! so an entry is trusted exactly as far as the directory holding it is. The rules, which the
//! threat model records:
//!
//! - The directory must belong to the current user, and no other user may be able to change what is
//!   in it.
//! - An entry must be a regular file belonging to the current user, and no other user may have access
//!   to it. An entry that fails the check is ignored, and the module is compiled again; the new entry
//!   then replaces it, so a refused file does not stay in use.
//! - An entry is named by the module's SHA-256, the call-depth limit it was compiled against, and
//!   the target. The artifact records the same, and a mismatch is refused.
//! - Entries are written to a temporary file of the user's own and renamed into place, so a reader
//!   never sees a partial one.
//!
//! The ownership and access checks are per platform: Unix permission bits in `native_cache/unix.rs`,
//! and access control lists in `native_cache/windows.rs`. Other platforms do not open the cache.

use std::path::{Path, PathBuf};

use crate::RuntimeError;

#[cfg(all(feature = "jit", unix))]
#[path = "native_cache/unix.rs"]
mod platform;

#[cfg(all(feature = "jit", windows))]
#[path = "native_cache/windows.rs"]
mod platform;

/// The cache key of a module: its SHA-256, in hexadecimal.
pub(crate) fn module_key(bytes: &[u8]) -> String {
    crate::hash_bytes(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A per-user directory of compiled module artifacts.
#[cfg(all(feature = "jit", any(unix, windows)))]
#[derive(Debug)]
pub(crate) struct NativeCache {
    dir: PathBuf,
    /// The user the directory belongs to, and so the owner of every entry that may be loaded.
    owner: platform::Owner,
    hits: std::sync::atomic::AtomicU64,
}

#[cfg(all(feature = "jit", any(unix, windows)))]
impl NativeCache {
    /// Open the cache in `dir`, creating it if needed, after checking that the current user owns it
    /// and that no other user can change it.
    pub(crate) fn open(dir: &Path) -> Result<Self, RuntimeError> {
        use std::io;

        std::fs::create_dir_all(dir).map_err(RuntimeError::Io)?;
        let meta = std::fs::symlink_metadata(dir).map_err(RuntimeError::Io)?;
        if !meta.file_type().is_dir() {
            return Err(RuntimeError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the native cache path is not a directory",
            )));
        }
        let owner = platform::open_directory(dir).map_err(RuntimeError::Io)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            owner,
            hits: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// How many loads have come from the cache rather than from compilation.
    pub(crate) fn hits(&self) -> u64 {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The entry for a module key, call-depth limit and host target.
    fn entry(&self, key: &str, max_call_depth: u64) -> PathBuf {
        self.dir.join(format!(
            "{key}-{max_call_depth}-{}.tptaot",
            tpt_wasm_jit::target_id()
        ))
    }

    /// The module compiled earlier under `key`, if the cache holds a trusted entry for it that matches
    /// the module's function count. `None` otherwise, and the caller compiles the module.
    pub(crate) fn load(
        &self,
        key: &str,
        function_count: usize,
        max_call_depth: u64,
    ) -> Option<tpt_wasm_jit::CompiledModule> {
        let path = self.entry(key, max_call_depth);
        if !platform::entry_is_trusted(&path, &self.owner) {
            return None;
        }
        let artifact =
            tpt_wasm_aot::ModuleArtifact::from_bytes(&std::fs::read(&path).ok()?).ok()?;
        if artifact.max_call_depth() != max_call_depth
            || artifact.functions().len() != function_count
        {
            return None;
        }
        let module = artifact.load().ok()?;
        self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(module)
    }

    /// Store a compiled module under `key`. Written to a temporary file of the user's own, then
    /// renamed over the entry.
    pub(crate) fn store(
        &self,
        key: &str,
        module: &tpt_wasm_jit::CompiledModule,
        max_call_depth: u64,
    ) -> std::io::Result<()> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        static TEMPORARY: AtomicU64 = AtomicU64::new(0);
        let bytes = tpt_wasm_aot::ModuleArtifact::from_module(module).to_bytes();
        let temporary = self.dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = platform::create_private(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, self.entry(key, max_call_depth))
    }
}

/// Without a native cache, a stand-in that never opens.
#[cfg(not(all(feature = "jit", any(unix, windows))))]
#[derive(Debug)]
pub(crate) struct NativeCache;

#[cfg(not(all(feature = "jit", any(unix, windows))))]
impl NativeCache {
    /// Refused: the cache needs the `jit` feature, and on this platform ownership and access cannot
    /// be checked.
    pub(crate) fn open(_dir: &Path) -> Result<Self, RuntimeError> {
        Err(RuntimeError::UnsupportedFeature(if cfg!(feature = "jit") {
            "the native cache needs Unix or Windows ownership and access checks"
        } else {
            "the native cache needs the jit feature"
        }))
    }

    pub(crate) fn hits(&self) -> u64 {
        0
    }
}

/// The per-user directory the native cache belongs in, on platforms that have one. Pass it as
/// `Config::native_cache_dir` to enable the cache.
pub fn default_native_cache_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
        Some(base.join("tpt-wasm").join("native"))
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("tpt-wasm").join("native"))
    }
}
