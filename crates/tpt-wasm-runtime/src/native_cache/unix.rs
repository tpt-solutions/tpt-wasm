//! Ownership and permission checks for the native cache on Unix.
//!
//! A directory is trusted when the current user owns it and no one but its owner can write to it. An
//! entry is trusted when the current user owns it and no group or other user has any access to it.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// The user id of the current user, which owns every entry this process may load.
pub(super) type Owner = u32;

/// Check the directory at `dir` and return the current user's id. Refused when another user owns it
/// or can write to it.
pub(super) fn open_directory(dir: &Path) -> io::Result<Owner> {
    let meta = fs::symlink_metadata(dir)?;
    if meta.permissions().mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the native cache directory is writable by users other than its owner",
        ));
    }
    // The current user's id is the owner of a file this process creates, so a probe gives it
    // without a platform call.
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    let _ = fs::remove_file(&probe);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe)?;
    let owner = file.metadata()?.uid();
    drop(file);
    fs::remove_file(&probe)?;
    if meta.uid() != owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the native cache directory belongs to another user",
        ));
    }
    Ok(owner)
}

/// Whether `path` is a regular file owned by `owner` with no group or other access. A file that
/// cannot be inspected is not trusted.
pub(super) fn entry_is_trusted(path: &Path, owner: &Owner) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_file() && meta.uid() == *owner && meta.permissions().mode() & 0o077 == 0
    })
}

/// Create a new file readable and writable by its owner only, for a temporary entry.
pub(super) fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}
