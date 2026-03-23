// src/sys.rs — Centralised Linux syscall / libc FFI declarations.
//
// Previously each module (session, seat, shm, screencopy, programs) had its
// own `mod libc` shim with overlapping extern "C" declarations.  Duplicate
// FFI declarations are harmless to the linker but confusing to read and easy
// to get wrong (e.g. the `off_t` type alias and `c_char` re-exports that
// crept into session.rs and seat.rs).  Everything lives here now.

use std::ffi::c_void;

extern "C" {
    // ── Memory mapping ────────────────────────────────────────────────────────
    pub fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut c_void;
    pub fn munmap(addr: *mut c_void, length: usize) -> i32;

    // ── File descriptors ──────────────────────────────────────────────────────
    pub fn memfd_create(name: *const std::ffi::c_char, flags: u32) -> i32;
    pub fn ftruncate(fd: i32, length: i64) -> i32;
    pub fn close(fd: i32) -> i32;
    pub fn dup(oldfd: i32) -> i32;

    // ── Time ──────────────────────────────────────────────────────────────────
    pub fn clock_gettime(clk: i32, tp: *mut Timespec) -> i32;

    // ── Errno ─────────────────────────────────────────────────────────────────
    pub fn __errno_location() -> *mut i32;
}

/// POSIX timespec, used for wall-clock queries in render/bar.rs.
#[repr(C)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

// ── mmap protection / flags constants ────────────────────────────────────────
pub const PROT_READ: i32 = 0x1;
pub const PROT_WRITE: i32 = 0x2;
pub const MAP_SHARED: i32 = 0x01;
pub const MAP_FAILED: *mut c_void = !0usize as *mut c_void;

// ── memfd flags ───────────────────────────────────────────────────────────────
pub const MFD_CLOEXEC: u32 = 0x0001;

/// Return the current errno value.
///
/// # Safety
/// Must only be called immediately after a syscall that sets errno.
pub unsafe fn errno() -> i32 {
    *__errno_location()
}
