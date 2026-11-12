//! Ownership and access checks for the native cache on Windows.
//!
//! A directory is trusted when the current user owns it and no other account holds a right that
//! could change what is in it. An entry is trusted when the current user owns it and no other account
//! can change it. The threat is code injection, which needs a write right, so a read-only grant to
//! another account is tolerated; Unix refuses read access by others too, which is stricter.
//!
//! The accounts that always count as trusted are the current user, the creator of a file (which is the
//! current user for every file this process makes), LocalSystem and the built-in Administrators. They
//! can already change any file the user owns, so they add no risk.
//!
//! A missing access control list grants everyone access, so it is refused. So is an allow entry of a
//! kind this check does not read, since it cannot be shown to grant nothing to anyone else. Deny and
//! audit entries grant nothing and are ignored.

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{addr_of, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    AclSizeInformation, CreateWellKnownSid, EqualSid, GetAce, GetAclInformation, GetLengthSid,
    GetTokenInformation, TokenUser, WinBuiltinAdministratorsSid, WinCreatorOwnerSid,
    WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, WELL_KNOWN_SID_TYPE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The ACE type of an allow entry, which is the only kind that grants access.
const ACCESS_ALLOWED: u8 = 0;

/// The ACE types that grant nothing: deny, audit and alarm entries, and the integrity label.
const NON_GRANTING: [u8; 13] = [1, 2, 3, 6, 7, 8, 10, 12, 13, 14, 15, 16, 17];

/// The rights that let a holder change a file or directory, or what is in it: write data, append,
/// write attributes and extended attributes, delete child, delete, change the access list and the
/// owner, and the generic write and all bits.
const CHANGE_RIGHTS: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x4000_0000 | 0x1000_0000;

/// The current user, whose SID is the owner of every entry this process may load.
#[derive(Debug)]
pub(super) struct Owner(Vec<u8>);

/// Check the directory at `dir` and return the current user as its owner. Refused when the directory
/// is not owned by the current user, or when another account can change its contents.
pub(super) fn open_directory(dir: &Path) -> io::Result<Owner> {
    let user = current_user()?;
    let trusted = trusted_accounts(&user)?;
    let security = Security::of(dir)?;
    let refused = security.owner != user
        || security.dacl.is_null()
        || grants_to_others(security.dacl, &trusted, CHANGE_RIGHTS)?;
    if refused {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the native cache directory is not owned by the current user, or other accounts can change it",
        ));
    }
    Ok(Owner(user))
}

/// Whether `path` is a regular file owned by `owner` that no other account can access. A file that
/// cannot be checked is not trusted.
pub(super) fn entry_is_trusted(path: &Path, owner: &Owner) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_file() {
        return false;
    }
    let Ok(security) = Security::of(path) else {
        return false;
    };
    let Ok(trusted) = trusted_accounts(&owner.0) else {
        return false;
    };
    if security.owner != owner.0 || security.dacl.is_null() {
        return false;
    }
    matches!(
        grants_to_others(security.dacl, &trusted, CHANGE_RIGHTS),
        Ok(false)
    )
}

/// Create a new file for a temporary entry. It inherits the directory's access list, which has already
/// been checked.
pub(super) fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// The current user's SID, from the process token.
fn current_user() -> io::Result<Vec<u8>> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing, and `token` is a valid out
    // pointer. The token is closed below on every path once it has been opened.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let user = token_user(token);
    // SAFETY: `token` was opened above and is closed exactly once.
    unsafe {
        CloseHandle(token);
    }
    user
}

/// The user SID inside a token, copied out.
fn token_user(token: HANDLE) -> io::Result<Vec<u8>> {
    let mut needed = 0u32;
    // SAFETY: with no buffer this call fails by design and reports the size needed in `needed`.
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
    }
    // A u64 buffer keeps the TOKEN_USER structure aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: the buffer holds `needed` bytes and is aligned for TOKEN_USER.
    unsafe {
        if GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        Ok(sid_bytes(user.User.Sid))
    }
}

/// The accounts whose access to a cache file or directory is not a risk: the current user, the creator
/// owner (the current user for files this process makes), LocalSystem and the built-in Administrators.
fn trusted_accounts(user: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    Ok(vec![
        user.to_vec(),
        well_known_sid(WinCreatorOwnerSid)?,
        well_known_sid(WinLocalSystemSid)?,
        well_known_sid(WinBuiltinAdministratorsSid)?,
    ])
}

/// A well-known SID, built without a domain.
fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut length = SECURITY_MAX_SID_SIZE;
    // SAFETY: the buffer is SECURITY_MAX_SID_SIZE bytes, the largest SID, and `length` gives its size.
    unsafe {
        if CreateWellKnownSid(
            kind,
            null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut length,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    buffer.truncate(length as usize);
    Ok(buffer)
}

/// Copy a SID out of memory that another call owns.
fn sid_bytes(sid: PSID) -> Vec<u8> {
    // SAFETY: `sid` points to a valid SID, whose length GetLengthSid reports.
    unsafe {
        let length = GetLengthSid(sid) as usize;
        std::slice::from_raw_parts(sid.cast::<u8>(), length).to_vec()
    }
}

/// The owner SID and access list of a file or directory, held while they are read. The security
/// descriptor they point into is freed on drop.
struct Security {
    owner: Vec<u8>,
    dacl: *mut ACL,
    descriptor: PSECURITY_DESCRIPTOR,
}

impl Security {
    fn of(path: &Path) -> io::Result<Self> {
        let name: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut owner: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `name` is a NUL-terminated wide string, and each out pointer is valid. The owner and
        // access list point into `descriptor`, which the returned value frees.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let owner = if owner.is_null() {
            Vec::new()
        } else {
            sid_bytes(owner)
        };
        Ok(Self {
            owner,
            dacl,
            descriptor,
        })
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: the descriptor was allocated by GetNamedSecurityInfoW and is freed once.
            unsafe {
                LocalFree(self.descriptor as HLOCAL);
            }
        }
    }
}

/// Whether an allow entry in `dacl` grants any of `rights` to an account outside `trusted`. An allow
/// entry of a kind this check does not read counts as granting, so it is refused.
fn grants_to_others(dacl: *mut ACL, trusted: &[Vec<u8>], rights: u32) -> io::Result<bool> {
    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    // SAFETY: `dacl` is a non-null access list, and `info` is the structure the class asks for.
    unsafe {
        if GetAclInformation(
            dacl,
            addr_of_mut_info(&mut info),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = null_mut();
        // SAFETY: `index` is below the entry count the list reports, and `ace` is a valid out pointer.
        unsafe {
            if GetAce(dacl, index, &mut ace) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: GetAce returns a pointer to an entry that begins with its header.
        let kind = unsafe { (*(ace as *const ACE_HEADER)).AceType };
        if kind == ACCESS_ALLOWED {
            // SAFETY: an allow entry has the ACCESS_ALLOWED_ACE layout, with the SID starting at `SidStart`.
            let grants = unsafe {
                let allowed = ace as *const ACCESS_ALLOWED_ACE;
                let sid = addr_of!((*allowed).SidStart) as *const c_void as PSID;
                if (*allowed).Mask & rights == 0 {
                    None
                } else {
                    Some(sid)
                }
            };
            if let Some(sid) = grants {
                let is_trusted = trusted
                    .iter()
                    // SAFETY: both SIDs are valid for the call.
                    .any(|account| unsafe { EqualSid(account.as_ptr() as PSID, sid) } != 0);
                if !is_trusted {
                    return Ok(true);
                }
            }
        } else if !NON_GRANTING.contains(&kind) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The address of an ACL size information structure, as the untyped pointer the call takes.
fn addr_of_mut_info(info: &mut ACL_SIZE_INFORMATION) -> *mut c_void {
    (info as *mut ACL_SIZE_INFORMATION).cast::<c_void>()
}
