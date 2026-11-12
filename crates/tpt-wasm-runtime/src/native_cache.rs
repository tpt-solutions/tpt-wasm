// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! An on-disk cache of compiled native code, so that a module compiled once is not compiled again
//! by a later process.
//!
//! A cached entry is a module artifact (see `tpt-wasm-aot`), and loading one installs machine code,
//! so an entry is trusted exactly as far as the directory holding it is. The rules, which the
//! threat model records:
//!
//! - The directory must be owned by the current user and must not be writable by anyone else.
//! - An entry must be a regular file owned by the current user, with no group or other access.
//!   An entry that fails the check is ignored, and the module is compiled again; the new entry then
//!   replaces it, so a refused file does not stay in use.
//! - An entry is named by the module's SHA-256, the call-depth limit it was compiled against, and
//!   the target. The artifact records the same, and a mismatch is refused.
//! - Entries are written to a temporary file of the user's own and renamed into place, so a reader
//!   never sees a partial one.
//!
//! Ownership and permissions are checked on Unix only. The standard library does not expose the
//! owner of a file on other platforms, so on those the cache does not open at all.

use std::path::{Path, PathBuf};

use crate::RuntimeError;

/// The cache key of a module: its SHA-256, in hexadecimal.
pub(crate) fn module_key(bytes: &[u8]) -> String {
    crate::hash_bytes(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A per-user directory of compiled module artifacts.
#[cfg(all(feature = "jit", unix))]
#[derive(Debug)]
pub(crate) struct NativeCache {
    dir: PathBuf,
    /// The user the directory belongs to, and so the owner of every entry that may be loaded.
    owner: u32,
    hits: std::sync::atomic::AtomicU64,
}

#[cfg(all(feature = "jit", unix))]
impl NativeCache {
    /// Open the cache in `dir`, creating it if needed, after checking that the current user owns it
    /// and that no other user can write to it.
    pub(crate) fn open(dir: &Path) -> Result<Self, RuntimeError> {
        use std::fs::{self, OpenOptions};
        use std::io;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        fs::create_dir_all(dir).map_err(RuntimeError::Io)?;
        let meta = fs::symlink_metadata(dir).map_err(RuntimeError::Io)?;
        if !meta.file_type().is_dir() {
            return Err(RuntimeError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the native cache path is not a directory",
            )));
        }
        if meta.permissions().mode() & 0o022 != 0 {
            return Err(RuntimeError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the native cache directory is writable by users other than its owner",
            )));
        }
        // The current user's id is the owner of a file this process creates, so a probe gives it
        // without a platform call.
        let probe = dir.join(format!(".probe-{}", std::process::id()));
        let _ = fs::remove_file(&probe);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)
            .map_err(RuntimeError::Io)?;
        let owner = file.metadata().map_err(RuntimeError::Io)?.uid();
        drop(file);
        fs::remove_file(&probe).map_err(RuntimeError::Io)?;
        if meta.uid() != owner {
            return Err(RuntimeError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the native cache directory belongs to another user",
            )));
        }
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
        use std::fs;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let path = self.entry(key, max_call_depth);
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.file_type().is_file()
            || meta.uid() != self.owner
            || meta.permissions().mode() & 0o077 != 0
        {
            return None;
        }
        let artifact = tpt_wasm_aot::ModuleArtifact::from_bytes(&fs::read(&path).ok()?).ok()?;
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
        use std::fs::{self, OpenOptions};
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::sync::atomic::{AtomicU64, Ordering};

        static TEMPORARY: AtomicU64 = AtomicU64::new(0);
        let bytes = tpt_wasm_aot::ModuleArtifact::from_module(module).to_bytes();
        let temporary = self.dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, self.entry(key, max_call_depth))
    }
}

/// Without a native cache, a stand-in that never opens.
#[cfg(not(all(feature = "jit", unix)))]
#[derive(Debug)]
pub(crate) struct NativeCache;

#[cfg(not(all(feature = "jit", unix)))]
impl NativeCache {
    /// Refused: the cache needs the `jit` feature, and on this platform ownership and permissions
    /// cannot be checked.
    pub(crate) fn open(_dir: &Path) -> Result<Self, RuntimeError> {
        Err(RuntimeError::UnsupportedFeature(if cfg!(feature = "jit") {
            "the native cache needs Unix ownership and permission checks"
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
