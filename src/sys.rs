// src/sys.rs — raw syscall shims used across the crate.

use std::ffi::c_void;

extern "C" {
    pub fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut c_void;
    pub fn munmap(addr: *mut c_void, len: usize) -> i32;
}

pub const PROT_READ: i32 = 0x1;
pub const PROT_WRITE: i32 = 0x2;
pub const MAP_SHARED: i32 = 0x01;
pub const MAP_FAILED: *mut c_void = !0usize as *mut c_void;
