// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! The executable-memory boundary: the only module in this crate that uses `unsafe`.
//!
//! Each block below states the invariant it relies on. The protocol is write-then-execute
//! and never both at once: a page is mapped read-write, the code is copied in, and the
//! page is then changed to read-execute. No page is ever writable and executable together,
//! so generated code cannot be patched after it is installed.

use crate::JitError;

#[cfg(all(target_arch = "x86_64", windows))]
mod sys {
    use std::ffi::c_void;

    pub(super) const MEM_COMMIT: u32 = 0x1000;
    pub(super) const MEM_RESERVE: u32 = 0x2000;
    pub(super) const MEM_RELEASE: u32 = 0x8000;
    pub(super) const PAGE_READWRITE: u32 = 0x04;
    pub(super) const PAGE_EXECUTE_READ: u32 = 0x20;

    #[link(name = "kernel32")]
    extern "system" {
        pub(super) fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            allocation_type: u32,
            protect: u32,
        ) -> *mut c_void;
        pub(super) fn VirtualProtect(
            address: *mut c_void,
            size: usize,
            new_protect: u32,
            old_protect: *mut u32,
        ) -> i32;
        pub(super) fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
    }
}

#[cfg(all(
    unix,
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]
mod sys {
    use std::ffi::c_void;

    pub(super) const PROT_READ: i32 = 1;
    pub(super) const PROT_WRITE: i32 = 2;
    pub(super) const PROT_EXEC: i32 = 4;
    pub(super) const MAP_PRIVATE: i32 = 2;
    #[cfg(target_os = "macos")]
    pub(super) const MAP_ANONYMOUS: i32 = 0x1000;
    #[cfg(not(target_os = "macos"))]
    pub(super) const MAP_ANONYMOUS: i32 = 0x20;

    extern "C" {
        pub(super) fn mmap(
            address: *mut c_void,
            length: usize,
            protection: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        pub(super) fn munmap(address: *mut c_void, length: usize) -> i32;
        pub(super) fn mprotect(address: *mut c_void, length: usize, protection: i32) -> i32;
    }
}

/// The page size the mappings are rounded to. Every platform maps whole pages.
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
const PAGE: usize = 4096;

/// Machine code installed in executable memory, freed when dropped.
pub(crate) struct ExecutableCode {
    /// The first byte of the mapping. Null only if the platform has no code generator,
    /// in which case no `ExecutableCode` is ever constructed.
    #[cfg(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    ))]
    address: *mut u8,
    /// The length of the mapping, rounded up to whole pages.
    #[cfg(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    ))]
    mapped: usize,
    /// A copy of the code, kept in ordinary memory so it can be inspected without
    /// reading the executable mapping.
    bytes: Vec<u8>,
}

// SAFETY: the mapping is read-execute once constructed. It is never written again, and
// nothing else holds a pointer into it, so sharing or moving the handle between threads
// cannot create a data race on the memory it names.
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
unsafe impl Send for ExecutableCode {}
// SAFETY: as for `Send`: the mapping is immutable after construction.
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
unsafe impl Sync for ExecutableCode {}

impl ExecutableCode {
    /// Copy `code` into fresh executable memory.
    #[cfg(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    ))]
    pub(crate) fn new(code: &[u8]) -> Result<Self, JitError> {
        if code.is_empty() {
            return Err(JitError::Unsupported("no code to install".into()));
        }
        let mapped = code.len().div_ceil(PAGE) * PAGE;
        let address = allocate_writable(mapped)?;
        // SAFETY: `address` is a fresh, writable mapping of `mapped >= code.len()` bytes
        // returned by the allocator, so the destination range is valid, writable, and does
        // not overlap `code`, which is an ordinary Rust slice.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), address, code.len()) };
        if let Err(error) = make_executable(address, mapped) {
            // SAFETY: `address` and `mapped` are exactly what `allocate_writable` returned,
            // and nothing has been handed out that could still refer to the mapping.
            unsafe { release(address, mapped) };
            return Err(error);
        }
        flush_instruction_cache(address, code.len());
        Ok(Self {
            address,
            mapped,
            bytes: code.to_vec(),
        })
    }

    #[cfg(not(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )))]
    pub(crate) fn new(_code: &[u8]) -> Result<Self, JitError> {
        Err(JitError::UnsupportedTarget)
    }

    /// Run the code. `args` must point at as many `i32` values as the function takes, and
    /// `context` must stay valid for the whole call; the code reads and writes memory only
    /// through the context's base and length, after checking them.
    #[cfg(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    ))]
    pub(crate) fn call(&self, args: *const i64, context: *mut crate::Context) -> u64 {
        type Entry = extern "C" fn(*const i64, *mut crate::Context) -> u64;
        // SAFETY: `self.address` is the start of a read-execute mapping holding code that
        // `x86::encode` generated for exactly this signature (`extern "C"`, an argument
        // pointer and a context pointer, one `i64` result, on the host's convention). The
        // mapping lives as long as `self`, and `&self` keeps it alive for the whole call. The
        // generated code reads exactly `arity` values through `args`, which the caller has
        // checked against the function's arity, and reaches memory only through `context`,
        // after a bounds check against `context.memory_len`. It keeps its frame below `rsp`,
        // and restores `rsp` and `rbp` before returning.
        let entry: Entry = unsafe { std::mem::transmute::<*mut u8, Entry>(self.address) };
        entry(args, context)
    }

    #[cfg(not(any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )))]
    pub(crate) fn call(&self, _args: *const i64, _context: *mut crate::Context) -> u64 {
        unreachable!("no ExecutableCode exists on this target")
    }

    /// The generated machine code.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]
impl Drop for ExecutableCode {
    fn drop(&mut self) {
        // SAFETY: `address` and `mapped` are the values `allocate_writable` returned for
        // this handle, and the handle is being dropped, so no call can still be running
        // through a pointer into the mapping: `call` borrows `self`.
        unsafe { release(self.address, self.mapped) };
    }
}

/// Map `size` bytes of read-write memory.
#[cfg(all(target_arch = "x86_64", windows))]
fn allocate_writable(size: usize) -> Result<*mut u8, JitError> {
    // SAFETY: a null address asks the system to choose one. The call has no pointer
    // arguments to validate.
    let address = unsafe {
        sys::VirtualAlloc(
            std::ptr::null_mut(),
            size,
            sys::MEM_COMMIT | sys::MEM_RESERVE,
            sys::PAGE_READWRITE,
        )
    };
    if address.is_null() {
        return Err(JitError::ExecutableMemory("VirtualAlloc failed".into()));
    }
    Ok(address.cast())
}

/// Map `size` bytes of read-write memory.
#[cfg(all(
    unix,
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]
fn allocate_writable(size: usize) -> Result<*mut u8, JitError> {
    // SAFETY: an anonymous private mapping with a null hint has no pointer arguments to
    // validate; the result is checked against MAP_FAILED below.
    let address = unsafe {
        sys::mmap(
            std::ptr::null_mut(),
            size,
            sys::PROT_READ | sys::PROT_WRITE,
            sys::MAP_PRIVATE | sys::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if address as isize == -1 {
        return Err(JitError::ExecutableMemory("mmap failed".into()));
    }
    Ok(address.cast())
}

/// Change a mapping from writable to read-execute.
#[cfg(all(target_arch = "x86_64", windows))]
fn make_executable(address: *mut u8, size: usize) -> Result<(), JitError> {
    let mut previous = 0u32;
    // SAFETY: `address` and `size` describe a mapping this module allocated and has not
    // released; `previous` is a valid location for the old protection.
    let ok =
        unsafe { sys::VirtualProtect(address.cast(), size, sys::PAGE_EXECUTE_READ, &mut previous) };
    if ok == 0 {
        return Err(JitError::ExecutableMemory("VirtualProtect failed".into()));
    }
    Ok(())
}

/// Change a mapping from writable to read-execute.
#[cfg(all(
    unix,
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]
fn make_executable(address: *mut u8, size: usize) -> Result<(), JitError> {
    // SAFETY: `address` and `size` describe a mapping this module allocated and has not
    // released.
    let ok = unsafe { sys::mprotect(address.cast(), size, sys::PROT_READ | sys::PROT_EXEC) };
    if ok != 0 {
        return Err(JitError::ExecutableMemory("mprotect failed".into()));
    }
    Ok(())
}

/// Return a mapping to the system.
///
/// # Safety
///
/// `address` and `size` must be exactly the values an allocation in this module returned,
/// and the mapping must not be used again.
#[cfg(all(target_arch = "x86_64", windows))]
unsafe fn release(address: *mut u8, _size: usize) {
    // SAFETY: the caller guarantees `address` is the base of an allocation made by
    // VirtualAlloc, which MEM_RELEASE requires to be passed with size 0.
    unsafe {
        sys::VirtualFree(address.cast(), 0, sys::MEM_RELEASE);
    }
}

/// Return a mapping to the system.
///
/// # Safety
///
/// As for the Windows version: `address` and `size` must be exactly what an allocation
/// returned, and the mapping must not be used again.
#[cfg(all(
    unix,
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]
unsafe fn release(address: *mut u8, size: usize) {
    // SAFETY: the caller guarantees the range is a mapping this module created.
    unsafe {
        sys::munmap(address.cast(), size);
    }
}

/// Make the instruction cache see the code just written.
///
/// On x86-64 the instruction cache is coherent with stores, so nothing is needed. AArch64
/// does not guarantee that, so the data cache lines are cleaned to the point of unification and
/// the instruction cache lines invalidated, with barriers between the two. RISC-V needs
/// `fence.i` after the stores.
#[cfg(all(unix, target_arch = "x86_64"))]
fn flush_instruction_cache(_address: *mut u8, _length: usize) {}

#[cfg(all(unix, target_arch = "aarch64"))]
fn flush_instruction_cache(address: *mut u8, length: usize) {
    // 64 bytes is no larger than any AArch64 cache line, so stepping by it touches every line.
    const LINE: usize = 64;
    let start = address as usize & !(LINE - 1);
    let end = address as usize + length;
    // SAFETY: the operations take only an address, and the addresses lie in the mapping this
    // module just wrote. They change cache state, not memory the program reads as data.
    unsafe {
        let mut line = start;
        while line < end {
            core::arch::asm!("dc cvau, {0}", in(reg) line);
            line += LINE;
        }
        core::arch::asm!("dsb ish");
        let mut line = start;
        while line < end {
            core::arch::asm!("ic ivau, {0}", in(reg) line);
            line += LINE;
        }
        core::arch::asm!("dsb ish", "isb");
    }
}

#[cfg(all(unix, target_arch = "riscv64"))]
fn flush_instruction_cache(_address: *mut u8, _length: usize) {
    // SAFETY: `fence.i` orders the instruction stream after the stores that wrote the code.
    unsafe { core::arch::asm!("fence.i") };
}

/// x86-64 keeps its instruction cache coherent with stores, so Windows needs no flush.
#[cfg(all(windows, target_arch = "x86_64"))]
fn flush_instruction_cache(_address: *mut u8, _length: usize) {}
