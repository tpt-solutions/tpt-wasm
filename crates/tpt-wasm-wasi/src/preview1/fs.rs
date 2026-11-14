// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! An in-memory filesystem for Preview 1, granted one preopened directory at a time.
//!
//! The embedder builds a [`Directory`] tree in memory and passes it as a [`Preopen`]. The
//! module sees that tree as a directory with a file descriptor and a guest name, and
//! nothing else. Nothing here touches the host filesystem: the tree is the whole of what
//! a module can name, which keeps a run deterministic and makes the grant explicit.
//!
//! Paths are resolved one component at a time. An absolute path, or one that climbs above
//! the directory it is resolved from with `..`, is `ENOTCAPABLE`, so a granted directory
//! cannot be left. The tree has no symbolic links, so there is nothing else to follow out
//! of it.
//!
//! A read-only grant refuses every operation that would write, create, or truncate. A
//! file cannot grow past [`MAX_FILE_BYTES`], so a seek and write far past the end is
//! refused rather than allocating what it asks for.

use std::collections::BTreeMap;

use tpt_wasm_types::GuestMemory;

use super::{errno, read_iovec, write_u32};

/// The first descriptor a preopened directory takes. Descriptors 0 to 2 are the standard
/// streams, and every descriptor from here up belongs to this module.
pub(super) const FIRST_PREOPEN: u32 = 3;

/// The largest size a file in the tree may grow to, in bytes.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// `path_open` open flags, as the ABI gives them.
const OFLAG_CREAT: u64 = 1 << 0;
const OFLAG_DIRECTORY: u64 = 1 << 1;
const OFLAG_EXCL: u64 = 1 << 2;
const OFLAG_TRUNC: u64 = 1 << 3;

/// The `fd_write` right, which a `path_open` asks for when the module means to write.
const RIGHT_FD_WRITE: u64 = 1 << 6;

/// `fd_seek` whence values.
const WHENCE_SET: u64 = 0;
const WHENCE_CUR: u64 = 1;
const WHENCE_END: u64 = 2;

/// A directory tree in memory: the files and directories a module may name.
#[derive(Default)]
pub struct Directory {
    entries: BTreeMap<String, Node>,
}

enum Node {
    File(Vec<u8>),
    Directory(Directory),
}

impl Directory {
    /// An empty directory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a file with `contents` at `path`, creating the directories above it. The path
    /// is relative and `/`-separated, and must not leave the directory.
    pub fn with_file(
        mut self,
        path: &str,
        contents: impl Into<Vec<u8>>,
    ) -> Result<Self, &'static str> {
        let mut parts = resolve(&[], path).map_err(|_| "the path leaves the directory")?;
        let name = parts.pop().ok_or("the path names no file")?;
        let parent = self.ensure_dir(&parts)?;
        if parent.entries.contains_key(&name) {
            return Err("the path is already taken");
        }
        parent.entries.insert(name, Node::File(contents.into()));
        Ok(self)
    }

    /// Add an empty directory at `path`, creating the directories above it.
    pub fn with_directory(mut self, path: &str) -> Result<Self, &'static str> {
        let parts = resolve(&[], path).map_err(|_| "the path leaves the directory")?;
        self.ensure_dir(&parts)?;
        Ok(self)
    }

    fn ensure_dir(&mut self, path: &[String]) -> Result<&mut Directory, &'static str> {
        let mut current = self;
        for name in path {
            let node = current
                .entries
                .entry(name.clone())
                .or_insert_with(|| Node::Directory(Directory::new()));
            current = match node {
                Node::Directory(dir) => dir,
                Node::File(_) => return Err("a path component is a file"),
            };
        }
        Ok(current)
    }
}

/// One directory the embedder grants to the module.
pub struct Preopen {
    guest_name: String,
    root: Directory,
    writable: bool,
}

impl Preopen {
    /// Grant `root` to the module under `guest_name`. A read-only grant refuses every
    /// write, create, and truncate beneath it.
    pub fn new(guest_name: impl Into<String>, root: Directory, writable: bool) -> Self {
        Self {
            guest_name: guest_name.into(),
            root,
            writable,
        }
    }
}

/// An open descriptor the module holds: a directory, or a file with its position.
enum Open {
    Directory {
        preopen: usize,
        path: Vec<String>,
    },
    File {
        preopen: usize,
        path: Vec<String>,
        offset: u64,
    },
}

/// The descriptor table, and the grants it was built from.
pub(super) struct FsState {
    preopens: Vec<Preopen>,
    open: BTreeMap<u32, Open>,
    next_fd: u32,
}

impl FsState {
    pub(super) fn new(preopens: Vec<Preopen>) -> Self {
        let mut open = BTreeMap::new();
        for index in 0..preopens.len() {
            open.insert(
                FIRST_PREOPEN + index as u32,
                Open::Directory {
                    preopen: index,
                    path: Vec::new(),
                },
            );
        }
        let next_fd = FIRST_PREOPEN + preopens.len() as u32;
        Self {
            preopens,
            open,
            next_fd,
        }
    }

    /// The contents of a file in a granted directory, read after the run.
    pub(super) fn read_file(&self, preopen: usize, path: &str) -> Option<Vec<u8>> {
        let root = &self.preopens.get(preopen)?.root;
        let parts = resolve(&[], path).ok()?;
        file_at(root, &parts).cloned()
    }
}

/// The descriptor a guest number names, if it is one this module can own: the standard
/// streams are handled elsewhere, and so is any number that does not fit.
pub(super) fn file_descriptor(fd: u64) -> Option<u32> {
    u32::try_from(fd).ok().filter(|fd| *fd >= FIRST_PREOPEN)
}

/// Resolve `path` against `base`, which is the path of the directory it starts from.
///
/// `.` and empty components are dropped. `..` removes the last component, but only one
/// the path itself added: climbing above `base` is `ENOTCAPABLE`, as is an absolute path.
pub(super) fn resolve(base: &[String], path: &str) -> Result<Vec<String>, i32> {
    if path.starts_with('/') {
        return Err(errno::NOTCAPABLE);
    }
    let mut out = base.to_vec();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.len() == base.len() {
                    return Err(errno::NOTCAPABLE);
                }
                out.pop();
            }
            name => out.push(name.to_owned()),
        }
    }
    Ok(out)
}

/// The guest's path string, read from its memory and checked as UTF-8.
fn read_path(memory: &dyn GuestMemory, at: u64, length: u64) -> Result<String, i32> {
    // Checked before allocating: a length past the memory can never be read.
    if length > memory.size() {
        return Err(errno::FAULT);
    }
    let bytes = memory.read(at, length).ok_or(errno::FAULT)?;
    String::from_utf8(bytes).map_err(|_| errno::INVAL)
}

enum Kind {
    Directory,
    File,
    Missing,
}

fn kind(root: &Directory, path: &[String]) -> Kind {
    let Some((name, parent)) = path.split_last() else {
        return Kind::Directory;
    };
    let Some(parent) = dir_at(root, parent) else {
        return Kind::Missing;
    };
    match parent.entries.get(name) {
        Some(Node::Directory(_)) => Kind::Directory,
        Some(Node::File(_)) => Kind::File,
        None => Kind::Missing,
    }
}

fn dir_at<'a>(root: &'a Directory, path: &[String]) -> Option<&'a Directory> {
    let mut current = root;
    for name in path {
        match current.entries.get(name)? {
            Node::Directory(dir) => current = dir,
            Node::File(_) => return None,
        }
    }
    Some(current)
}

fn dir_at_mut<'a>(root: &'a mut Directory, path: &[String]) -> Option<&'a mut Directory> {
    let mut current = root;
    for name in path {
        match current.entries.get_mut(name)? {
            Node::Directory(dir) => current = dir,
            Node::File(_) => return None,
        }
    }
    Some(current)
}

fn file_at<'a>(root: &'a Directory, path: &[String]) -> Option<&'a Vec<u8>> {
    let (name, parent) = path.split_last()?;
    match dir_at(root, parent)?.entries.get(name)? {
        Node::File(contents) => Some(contents),
        Node::Directory(_) => None,
    }
}

fn file_at_mut<'a>(root: &'a mut Directory, path: &[String]) -> Option<&'a mut Vec<u8>> {
    let (name, parent) = path.split_last()?;
    match dir_at_mut(root, parent)?.entries.get_mut(name)? {
        Node::File(contents) => Some(contents),
        Node::Directory(_) => None,
    }
}

/// The arguments of `path_open`, grouped so the function takes one request.
pub(super) struct OpenRequest {
    pub dirfd: u64,
    pub path_at: u64,
    pub path_len: u64,
    pub oflags: u64,
    pub rights: u64,
    pub opened_at: u64,
}

/// `path_open`: open or create a file or directory beneath a descriptor.
///
/// The rights bit `fd_write` and the create and truncate flags are a write, and a
/// read-only grant refuses a write with `ENOTCAPABLE`. The rights a module asks for
/// otherwise are not used to narrow the descriptor: a file descriptor is writable exactly
/// when its grant is.
pub(super) fn path_open(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    request: OpenRequest,
) -> i32 {
    let Ok(dirfd) = u32::try_from(request.dirfd) else {
        return errno::BADF;
    };
    let (preopen, base) = match fs.open.get(&dirfd) {
        Some(Open::Directory { preopen, path }) => (*preopen, path.clone()),
        Some(Open::File { .. }) => return errno::NOTDIR,
        None => return errno::BADF,
    };
    let path = match read_path(memory, request.path_at, request.path_len) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let parts = match resolve(&base, &path) {
        Ok(parts) => parts,
        Err(code) => return code,
    };
    let create = request.oflags & OFLAG_CREAT != 0;
    let truncate = request.oflags & OFLAG_TRUNC != 0;
    let writes = create || truncate || request.rights & RIGHT_FD_WRITE != 0;
    if writes && !fs.preopens[preopen].writable {
        return errno::NOTCAPABLE;
    }
    let root = &mut fs.preopens[preopen].root;
    let opened = match kind(root, &parts) {
        Kind::Directory => {
            if writes {
                return errno::ISDIR;
            }
            Open::Directory {
                preopen,
                path: parts,
            }
        }
        Kind::File => {
            if request.oflags & OFLAG_DIRECTORY != 0 {
                return errno::NOTDIR;
            }
            if create && request.oflags & OFLAG_EXCL != 0 {
                return errno::EXIST;
            }
            if truncate {
                if let Some(contents) = file_at_mut(root, &parts) {
                    contents.clear();
                }
            }
            Open::File {
                preopen,
                path: parts,
                offset: 0,
            }
        }
        Kind::Missing => {
            if !create {
                return errno::NOENT;
            }
            let Some((name, parent)) = parts.split_last() else {
                return errno::NOENT;
            };
            let Some(parent) = dir_at_mut(root, parent) else {
                return errno::NOENT;
            };
            parent.entries.insert(name.clone(), Node::File(Vec::new()));
            Open::File {
                preopen,
                path: parts,
                offset: 0,
            }
        }
    };
    let Some(next_fd) = fs.next_fd.checked_add(1) else {
        return errno::MFILE;
    };
    // A bad output pointer is found here, after the open, so a file created by this call
    // stays in the tree. The module can only learn of it from the errno.
    let fd = fs.next_fd;
    if !memory.write(request.opened_at, &fd.to_le_bytes()) {
        return errno::FAULT;
    }
    fs.open.insert(fd, opened);
    fs.next_fd = next_fd;
    errno::SUCCESS
}

/// `fd_read` on a file descriptor: fill the iovecs from the file at its position.
pub(super) fn read(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    fd: u32,
    iovs_at: u64,
    iovs_len: u64,
    read_at: u64,
) -> i32 {
    let FsState { preopens, open, .. } = fs;
    let (preopen, path, offset) = match open.get_mut(&fd) {
        Some(Open::File {
            preopen,
            path,
            offset,
        }) => (*preopen, path, offset),
        Some(Open::Directory { .. }) => return errno::ISDIR,
        None => return errno::BADF,
    };
    let Some(contents) = file_at(&preopens[preopen].root, path.as_slice()) else {
        return errno::BADF;
    };
    let mut total: u32 = 0;
    for index in 0..iovs_len {
        let Some((buffer, length)) = read_iovec(memory, iovs_at, index) else {
            return errno::FAULT;
        };
        if u64::from(length) > memory.size() {
            return errno::FAULT;
        }
        let start = usize::try_from(*offset)
            .unwrap_or(usize::MAX)
            .min(contents.len());
        let end = start.saturating_add(length as usize).min(contents.len());
        let chunk = &contents[start..end];
        if !memory.write(u64::from(buffer), chunk) {
            return errno::FAULT;
        }
        *offset += chunk.len() as u64;
        total = total.saturating_add(chunk.len() as u32);
        if chunk.len() < length as usize {
            break;
        }
    }
    if write_u32(memory, read_at, total) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_write` on a file descriptor: write the iovecs at its position, growing the file
/// with zeros where the position is past its end.
pub(super) fn write(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    fd: u32,
    iovs_at: u64,
    iovs_len: u64,
    written_at: u64,
) -> i32 {
    let FsState { preopens, open, .. } = fs;
    let (preopen, path, offset) = match open.get_mut(&fd) {
        Some(Open::File {
            preopen,
            path,
            offset,
        }) => (*preopen, path, offset),
        Some(Open::Directory { .. }) => return errno::ISDIR,
        None => return errno::BADF,
    };
    if !preopens[preopen].writable {
        return errno::BADF;
    }
    let mut total: u32 = 0;
    for index in 0..iovs_len {
        let Some((buffer, length)) = read_iovec(memory, iovs_at, index) else {
            return errno::FAULT;
        };
        let Some(bytes) = memory.read(u64::from(buffer), u64::from(length)) else {
            return errno::FAULT;
        };
        let Some(contents) = file_at_mut(&mut preopens[preopen].root, path.as_slice()) else {
            return errno::BADF;
        };
        let end = match offset.checked_add(bytes.len() as u64) {
            Some(end) if end <= MAX_FILE_BYTES => end,
            _ => return errno::FBIG,
        };
        let start = *offset as usize;
        let end = end as usize;
        if contents.len() < end {
            contents.resize(end, 0);
        }
        contents[start..end].copy_from_slice(&bytes);
        *offset = end as u64;
        total = total.saturating_add(length);
    }
    if write_u32(memory, written_at, total) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_seek` on a file descriptor. The new position is written to `at` as a `u64`.
pub(super) fn seek(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    fd: u32,
    offset: i64,
    whence: u64,
    at: u64,
) -> i32 {
    let FsState { preopens, open, .. } = fs;
    let (preopen, path, position) = match open.get_mut(&fd) {
        Some(Open::File {
            preopen,
            path,
            offset,
        }) => (*preopen, path, offset),
        Some(Open::Directory { .. }) => return errno::ISDIR,
        None => return errno::BADF,
    };
    let length = file_at(&preopens[preopen].root, path.as_slice())
        .map_or(0, |contents| contents.len() as u64);
    let base = match whence {
        WHENCE_SET => 0,
        WHENCE_CUR => i128::from(*position),
        WHENCE_END => i128::from(length),
        _ => return errno::INVAL,
    };
    let target = base + i128::from(offset);
    if target < 0 || target > i128::from(u64::MAX) {
        return errno::INVAL;
    }
    let target = target as u64;
    *position = target;
    if memory.write(at, &target.to_le_bytes()) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// The ABI's file types, as the metadata records give them.
pub(super) const FILETYPE_CHARACTER_DEVICE: u8 = 2;
const FILETYPE_DIRECTORY: u8 = 3;
const FILETYPE_REGULAR_FILE: u8 = 4;

/// Rights bits a descriptor reports, from the ABI's numbering.
const RIGHT_FD_READ: u64 = 1 << 1;
const RIGHT_FD_SEEK: u64 = 1 << 2;
const RIGHT_FD_TELL: u64 = 1 << 5;
const RIGHT_PATH_CREATE_DIRECTORY: u64 = 1 << 9;
const RIGHT_PATH_CREATE_FILE: u64 = 1 << 10;
const RIGHT_PATH_OPEN: u64 = 1 << 13;
const RIGHT_FD_READDIR: u64 = 1 << 14;
const RIGHT_PATH_FILESTAT_GET: u64 = 1 << 18;
const RIGHT_FD_FILESTAT_GET: u64 = 1 << 21;
const RIGHT_PATH_REMOVE_DIRECTORY: u64 = 1 << 25;
const RIGHT_PATH_UNLINK_FILE: u64 = 1 << 26;

/// The rights a file descriptor reports. A write right appears only on a writable grant.
fn file_rights(writable: bool) -> u64 {
    let base = RIGHT_FD_READ | RIGHT_FD_SEEK | RIGHT_FD_TELL | RIGHT_FD_FILESTAT_GET;
    if writable {
        base | RIGHT_FD_WRITE
    } else {
        base
    }
}

/// The rights a directory descriptor reports, with the create and remove rights only on a
/// writable grant.
fn directory_rights(writable: bool) -> u64 {
    let base = RIGHT_PATH_OPEN | RIGHT_FD_READDIR | RIGHT_PATH_FILESTAT_GET | RIGHT_FD_FILESTAT_GET;
    if writable {
        base | RIGHT_PATH_CREATE_DIRECTORY
            | RIGHT_PATH_CREATE_FILE
            | RIGHT_PATH_REMOVE_DIRECTORY
            | RIGHT_PATH_UNLINK_FILE
    } else {
        base
    }
}

/// A stable number for a path within a grant, so the same file reports the same inode for
/// the whole run. It is an FNV-1a hash of the grant and the path, not a host inode.
fn inode(preopen: usize, path: &[String]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(&(preopen as u64).to_le_bytes());
    for part in path {
        feed(part.as_bytes());
        feed(&[0]);
    }
    hash
}

/// Write a `fdstat` record: the file type, the rights the descriptor has, and the rights it
/// passes to the descriptors it opens.
fn write_fdstat(
    memory: &mut dyn GuestMemory,
    at: u64,
    filetype: u8,
    base: u64,
    inheriting: u64,
) -> i32 {
    let mut record = [0u8; 24];
    record[0] = filetype;
    record[8..16].copy_from_slice(&base.to_le_bytes());
    record[16..24].copy_from_slice(&inheriting.to_le_bytes());
    if memory.write(at, &record) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// Write a `filestat` record. The device, link count, and times are fixed: there is one
/// device, no hard links, and the tree keeps no times.
fn write_filestat(
    memory: &mut dyn GuestMemory,
    at: u64,
    inode: u64,
    filetype: u8,
    size: u64,
) -> i32 {
    let mut record = [0u8; 64];
    record[8..16].copy_from_slice(&inode.to_le_bytes());
    record[16] = filetype;
    record[24..32].copy_from_slice(&1u64.to_le_bytes());
    record[32..40].copy_from_slice(&size.to_le_bytes());
    if memory.write(at, &record) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_fdstat_get` on a file or directory descriptor.
pub(super) fn fdstat_get(fs: &FsState, memory: &mut dyn GuestMemory, fd: u32, at: u64) -> i32 {
    let (filetype, base, inheriting) = match fs.open.get(&fd) {
        Some(Open::File { preopen, .. }) => (
            FILETYPE_REGULAR_FILE,
            file_rights(fs.preopens[*preopen].writable),
            0,
        ),
        Some(Open::Directory { preopen, .. }) => {
            let rights = directory_rights(fs.preopens[*preopen].writable);
            (FILETYPE_DIRECTORY, rights, rights)
        }
        None => return errno::BADF,
    };
    write_fdstat(memory, at, filetype, base, inheriting)
}

/// `fd_filestat_get` on a file or directory descriptor.
pub(super) fn filestat_get(fs: &FsState, memory: &mut dyn GuestMemory, fd: u32, at: u64) -> i32 {
    match fs.open.get(&fd) {
        Some(Open::File { preopen, path, .. }) => {
            let root = &fs.preopens[*preopen].root;
            let Some(contents) = file_at(root, path) else {
                return errno::BADF;
            };
            write_filestat(
                memory,
                at,
                inode(*preopen, path),
                FILETYPE_REGULAR_FILE,
                contents.len() as u64,
            )
        }
        Some(Open::Directory { preopen, path }) => {
            write_filestat(memory, at, inode(*preopen, path), FILETYPE_DIRECTORY, 0)
        }
        None => errno::BADF,
    }
}

/// `fd_tell`: the position of a file. A directory has none to report.
pub(super) fn tell(fs: &FsState, memory: &mut dyn GuestMemory, fd: u32, at: u64) -> i32 {
    match fs.open.get(&fd) {
        Some(Open::File { offset, .. }) => {
            if memory.write(at, &offset.to_le_bytes()) {
                errno::SUCCESS
            } else {
                errno::FAULT
            }
        }
        Some(Open::Directory { .. }) => errno::ISDIR,
        None => errno::BADF,
    }
}

/// `fd_readdir`: write the entries of a directory from `cookie` on. Each entry is a 24-byte
/// `dirent` header (the next cookie, the inode, the name length, and the file type, padded),
/// then its name. The last entry may be cut short where the buffer ends, and `used`
/// reports how many bytes were written. `.` and `..` come first, then the children in name
/// order. A cookie is an index: an entry's next cookie is the index after it.
pub(super) fn readdir(
    fs: &FsState,
    memory: &mut dyn GuestMemory,
    fd: u64,
    buf: u64,
    buf_len: u64,
    cookie: u64,
    used_at: u64,
) -> i32 {
    let fd = match u32::try_from(fd) {
        Ok(fd) => fd,
        Err(_) => return errno::BADF,
    };
    let (preopen, path) = match fs.open.get(&fd) {
        Some(Open::Directory { preopen, path }) => (*preopen, path),
        Some(Open::File { .. }) => return errno::NOTDIR,
        None => return errno::BADF,
    };
    let Some(dir) = dir_at(&fs.preopens[preopen].root, path) else {
        return errno::NOENT;
    };
    let parent = &path[..path.len().saturating_sub(1)];
    let mut entries: Vec<(String, u64, u8)> = vec![
        (".".to_owned(), inode(preopen, path), FILETYPE_DIRECTORY),
        ("..".to_owned(), inode(preopen, parent), FILETYPE_DIRECTORY),
    ];
    for (name, node) in &dir.entries {
        let mut child = path.clone();
        child.push(name.clone());
        let filetype = match node {
            Node::Directory(_) => FILETYPE_DIRECTORY,
            Node::File(_) => FILETYPE_REGULAR_FILE,
        };
        entries.push((name.clone(), inode(preopen, &child), filetype));
    }
    // Stop once the buffer is full, so a large buffer over a small directory stays small.
    let capacity = usize::try_from(buf_len).unwrap_or(usize::MAX);
    let start = usize::try_from(cookie).unwrap_or(usize::MAX);
    let mut out: Vec<u8> = Vec::new();
    for (index, (name, inode, filetype)) in entries.iter().enumerate().skip(start) {
        if out.len() >= capacity {
            break;
        }
        out.extend_from_slice(&((index as u64) + 1).to_le_bytes());
        out.extend_from_slice(&inode.to_le_bytes());
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.push(*filetype);
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(name.as_bytes());
    }
    out.truncate(capacity);
    let used = out.len() as u32;
    if !memory.write(buf, &out) || !memory.write(used_at, &used.to_le_bytes()) {
        return errno::FAULT;
    }
    errno::SUCCESS
}

/// `fd_fdstat_get` on a standard stream, which is a character device.
pub(super) fn stream_fdstat(memory: &mut dyn GuestMemory, at: u64, base: u64) -> i32 {
    write_fdstat(memory, at, FILETYPE_CHARACTER_DEVICE, base, 0)
}

/// `fd_filestat_get` on a standard stream.
pub(super) fn stream_filestat(memory: &mut dyn GuestMemory, at: u64) -> i32 {
    write_filestat(memory, at, 0, FILETYPE_CHARACTER_DEVICE, 0)
}

/// The rights a standard input has.
pub(super) const STDIN_RIGHTS: u64 = RIGHT_FD_READ;
/// The rights a standard output or error has.
pub(super) const STDOUT_RIGHTS: u64 = RIGHT_FD_WRITE;

/// Resolve a directory descriptor and a path under it to a grant and the path within it.
fn locate(
    fs: &FsState,
    memory: &dyn GuestMemory,
    dirfd: u64,
    path_at: u64,
    path_len: u64,
) -> Result<(usize, Vec<String>), i32> {
    let dirfd = u32::try_from(dirfd).map_err(|_| errno::BADF)?;
    let (preopen, base) = match fs.open.get(&dirfd) {
        Some(Open::Directory { preopen, path }) => (*preopen, path.clone()),
        Some(Open::File { .. }) => return Err(errno::NOTDIR),
        None => return Err(errno::BADF),
    };
    let path = read_path(memory, path_at, path_len)?;
    Ok((preopen, resolve(&base, &path)?))
}

/// `path_filestat_get`. A symbolic link cannot exist in the tree, so the follow flag has
/// nothing to change.
pub(super) fn path_filestat_get(
    fs: &FsState,
    memory: &mut dyn GuestMemory,
    dirfd: u64,
    path_at: u64,
    path_len: u64,
    at: u64,
) -> i32 {
    let (preopen, parts) = match locate(fs, memory, dirfd, path_at, path_len) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let root = &fs.preopens[preopen].root;
    match kind(root, &parts) {
        Kind::Directory => {
            write_filestat(memory, at, inode(preopen, &parts), FILETYPE_DIRECTORY, 0)
        }
        Kind::File => {
            let size = file_at(root, &parts).map_or(0, |contents| contents.len() as u64);
            write_filestat(
                memory,
                at,
                inode(preopen, &parts),
                FILETYPE_REGULAR_FILE,
                size,
            )
        }
        Kind::Missing => errno::NOENT,
    }
}

/// `path_create_directory`: make an empty directory, whose parent must exist.
pub(super) fn create_directory(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    dirfd: u64,
    path_at: u64,
    path_len: u64,
) -> i32 {
    let (preopen, parts) = match locate(fs, memory, dirfd, path_at, path_len) {
        Ok(found) => found,
        Err(code) => return code,
    };
    if !fs.preopens[preopen].writable {
        return errno::NOTCAPABLE;
    }
    let root = &mut fs.preopens[preopen].root;
    let Some((name, parent)) = parts.split_last() else {
        return errno::EXIST;
    };
    if !matches!(kind(root, &parts), Kind::Missing) {
        return errno::EXIST;
    }
    let Some(parent) = dir_at_mut(root, parent) else {
        return errno::NOENT;
    };
    parent
        .entries
        .insert(name.clone(), Node::Directory(Directory::new()));
    errno::SUCCESS
}

/// `path_remove_directory`: remove an empty directory. Descriptors already open on it are
/// refused afterwards, as on an unlinked file.
pub(super) fn remove_directory(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    dirfd: u64,
    path_at: u64,
    path_len: u64,
) -> i32 {
    let (preopen, parts) = match locate(fs, memory, dirfd, path_at, path_len) {
        Ok(found) => found,
        Err(code) => return code,
    };
    if !fs.preopens[preopen].writable {
        return errno::NOTCAPABLE;
    }
    let root = &mut fs.preopens[preopen].root;
    let Some((name, parent)) = parts.split_last() else {
        return errno::NOTCAPABLE;
    };
    match kind(root, &parts) {
        Kind::Directory => {}
        Kind::File => return errno::NOTDIR,
        Kind::Missing => return errno::NOENT,
    }
    let empty = match dir_at(root, &parts) {
        Some(dir) => dir.entries.is_empty(),
        None => return errno::NOENT,
    };
    if !empty {
        return errno::NOTEMPTY;
    }
    if let Some(parent) = dir_at_mut(root, parent) {
        parent.entries.remove(name);
    }
    errno::SUCCESS
}

/// `path_unlink_file`: remove a file. Descriptors already open on it are refused
/// afterwards; the bytes are not kept alive for them.
pub(super) fn unlink_file(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    dirfd: u64,
    path_at: u64,
    path_len: u64,
) -> i32 {
    let (preopen, parts) = match locate(fs, memory, dirfd, path_at, path_len) {
        Ok(found) => found,
        Err(code) => return code,
    };
    if !fs.preopens[preopen].writable {
        return errno::NOTCAPABLE;
    }
    let root = &mut fs.preopens[preopen].root;
    match kind(root, &parts) {
        Kind::File => {}
        Kind::Directory => return errno::ISDIR,
        Kind::Missing => return errno::NOENT,
    }
    let Some((name, parent)) = parts.split_last() else {
        return errno::ISDIR;
    };
    if let Some(parent) = dir_at_mut(root, parent) {
        parent.entries.remove(name);
    }
    errno::SUCCESS
}

/// One path of a two-path call: a directory descriptor and a path under it.
pub(super) struct PathArg {
    pub dirfd: u64,
    pub path_at: u64,
    pub path_len: u64,
}

/// `path_rename`: move a file or directory to a new name inside one preopen. A target that
/// exists is replaced only when the two are alike: a file for a file, or an empty directory
/// for a directory. A directory is never moved into itself. Descriptors already open on the
/// old path are refused afterwards, as after an unlink, because descriptors name paths here.
pub(super) fn rename(
    fs: &mut FsState,
    memory: &mut dyn GuestMemory,
    from: PathArg,
    to: PathArg,
) -> i32 {
    let (preopen, old) = match locate(fs, memory, from.dirfd, from.path_at, from.path_len) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let (target, new) = match locate(fs, memory, to.dirfd, to.path_at, to.path_len) {
        Ok(found) => found,
        Err(code) => return code,
    };
    if preopen != target {
        return errno::XDEV;
    }
    if !fs.preopens[preopen].writable {
        return errno::NOTCAPABLE;
    }
    let root = &mut fs.preopens[preopen].root;
    let (Some((old_name, old_parent)), Some((new_name, new_parent))) =
        (old.split_last(), new.split_last())
    else {
        return errno::NOTCAPABLE;
    };
    let source = kind(root, &old);
    if matches!(source, Kind::Missing) {
        return errno::NOENT;
    }
    if old == new {
        return errno::SUCCESS;
    }
    if matches!(source, Kind::Directory) && new.starts_with(&old) {
        return errno::INVAL;
    }
    match kind(root, new_parent) {
        Kind::Directory => {}
        Kind::File => return errno::NOTDIR,
        Kind::Missing => return errno::NOENT,
    }
    match kind(root, &new) {
        Kind::Missing => {}
        Kind::File if matches!(source, Kind::File) => {}
        Kind::File => return errno::NOTDIR,
        Kind::Directory if matches!(source, Kind::File) => return errno::ISDIR,
        Kind::Directory => {
            if dir_at(root, &new).is_some_and(|dir| !dir.entries.is_empty()) {
                return errno::NOTEMPTY;
            }
        }
    }
    let Some(node) = dir_at_mut(root, old_parent).and_then(|dir| dir.entries.remove(old_name))
    else {
        return errno::NOENT;
    };
    if let Some(parent) = dir_at_mut(root, new_parent) {
        parent.entries.insert(new_name.clone(), node);
    }
    errno::SUCCESS
}

/// `fd_close` on a file or directory descriptor, preopened or opened.
pub(super) fn close(fs: &mut FsState, fd: u32) -> i32 {
    if fs.open.remove(&fd).is_some() {
        errno::SUCCESS
    } else {
        errno::BADF
    }
}

/// The preopened directory a descriptor names, if it is one. A directory opened by
/// `path_open` is not a preopen, even when it names the directory the grant was for.
fn preopen_index(fs: &FsState, fd: u64) -> Option<usize> {
    let fd = u32::try_from(fd).ok()?;
    match fs.open.get(&fd)? {
        Open::Directory { preopen, path }
            if path.is_empty() && fd == FIRST_PREOPEN + *preopen as u32 =>
        {
            Some(*preopen)
        }
        _ => None,
    }
}

/// `fd_prestat_get`: tell the module that a preopened descriptor is a directory, and how
/// long its guest name is. The record is a tag byte, three bytes of padding, and a `u32`.
pub(super) fn prestat_get(fs: &FsState, memory: &mut dyn GuestMemory, fd: u64, at: u64) -> i32 {
    let Some(index) = preopen_index(fs, fd) else {
        return errno::BADF;
    };
    let name_len = fs.preopens[index].guest_name.len() as u32;
    let mut record = [0u8; 8];
    record[4..8].copy_from_slice(&name_len.to_le_bytes());
    if memory.write(at, &record) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

/// `fd_prestat_dir_name`: write a preopened descriptor's guest name into the buffer.
pub(super) fn prestat_dir_name(
    fs: &FsState,
    memory: &mut dyn GuestMemory,
    fd: u64,
    at: u64,
    length: u64,
) -> i32 {
    let Some(index) = preopen_index(fs, fd) else {
        return errno::BADF;
    };
    let name = fs.preopens[index].guest_name.as_bytes();
    if length < name.len() as u64 {
        return errno::INVAL;
    }
    if memory.write(at, name) {
        errno::SUCCESS
    } else {
        errno::FAULT
    }
}

#[cfg(test)]
mod tests {
    use super::errno;
    use super::resolve;

    fn parts(base: &[&str], path: &str) -> Result<Vec<String>, i32> {
        let base: Vec<String> = base.iter().map(|part| (*part).to_owned()).collect();
        resolve(&base, path)
    }

    #[test]
    fn dot_and_empty_components_are_dropped() {
        assert_eq!(parts(&[], "./a//b/."), Ok(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn dotdot_climbs_only_within_what_the_path_added() {
        assert_eq!(parts(&[], "a/b/../c"), Ok(vec!["a".into(), "c".into()]));
        assert_eq!(
            parts(&["x", "z"], "a/../y"),
            Ok(vec!["x".into(), "z".into(), "y".into()])
        );
        assert_eq!(parts(&[], "a/../.."), Err(errno::NOTCAPABLE));
        // The descriptor's own directory is the limit, even when the grant is larger: from
        // "x/z" a ".." reaches "x", which the descriptor was opened to exclude.
        assert_eq!(parts(&["x", "z"], "../y"), Err(errno::NOTCAPABLE));
        assert_eq!(parts(&["x"], "../y"), Err(errno::NOTCAPABLE));
    }

    #[test]
    fn an_absolute_path_is_refused() {
        assert_eq!(parts(&[], "/etc/passwd"), Err(errno::NOTCAPABLE));
    }
}
