/// sys.rs — low-level FFI and platform helpers.
///
/// Keeps unsafe libc calls in one place so the rest of the codebase stays clean.
use std::os::unix::io::{AsFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

// ── memfd_create ─────────────────────────────────────────────────────────────

/// Create an anonymous in-memory file and fill it with `data`.
/// Used for sending the XKB keymap to clients over the Wayland protocol.
pub fn memfd_create(data: &[u8]) -> std::io::Result<OwnedFd> {
    use std::io::Write;

    let name = std::ffi::CString::new("axiom-memfd").expect("CString");
    // SAFETY: memfd_create is a pure syscall with no aliasing concerns.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    f.write_all(data)?;
    // Transfer ownership back to OwnedFd
    use std::os::unix::io::IntoRawFd;
    Ok(unsafe { OwnedFd::from_raw_fd(f.into_raw_fd()) })
}

// ── pipe2 ─────────────────────────────────────────────────────────────────────

/// Create a non-blocking, close-on-exec pipe. Returns (read_end, write_end).
pub fn pipe_cloexec() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [RawFd; 2] = [-1, -1];
    let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

// ── dup ──────────────────────────────────────────────────────────────────────

/// Duplicate a file descriptor with O_CLOEXEC.
pub fn dup_cloexec(fd: BorrowedFd<'_>) -> std::io::Result<OwnedFd> {
    use std::os::unix::io::AsRawFd;
    let new = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if new < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(new) })
}

// ── set_cloexec ───────────────────────────────────────────────────────────────

/// Ensure a file descriptor has the FD_CLOEXEC flag set.
pub fn set_cloexec(fd: BorrowedFd<'_>) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD, 0) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ret = unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ── mmap / munmap helpers ─────────────────────────────────────────────────────

/// Map a file descriptor read-only. Returns a `MmapGuard` that unmaps on drop.
pub struct MmapGuard {
    ptr: *mut libc::c_void,
    len: usize,
}

impl MmapGuard {
    /// # Safety
    /// `fd` must remain valid and the region [offset, offset+len) must be
    /// within the file for the lifetime of this guard.
    pub unsafe fn new(fd: BorrowedFd<'_>, offset: i64, len: usize) -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            offset,
        );
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { ptr, len })
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr is valid for `len` bytes for our lifetime.
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }
}

impl Drop for MmapGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() && self.ptr != libc::MAP_FAILED {
            unsafe { libc::munmap(self.ptr, self.len) };
        }
    }
}

// SAFETY: the mapping is read-only and not aliased mutably anywhere.
unsafe impl Send for MmapGuard {}

// ── Spawn helper ──────────────────────────────────────────────────────────────

/// Spawn a shell command, double-forking so we don't adopt zombie children.
/// Equivalent to Hyprland's `execl("/bin/sh", "-c", cmd, NULL)` approach.
pub fn spawn(cmd: &str) {
    let cmd = cmd.to_owned();
    // Double-fork: parent returns immediately, grandchild is reparented to init.
    match unsafe { libc::fork() } {
        0 => {
            // First child: fork again then exit
            match unsafe { libc::fork() } {
                0 => {
                    // Grandchild: exec the command
                    let sh = std::ffi::CString::new("/bin/sh").unwrap();
                    let flag = std::ffi::CString::new("-c").unwrap();
                    let cmd_c = std::ffi::CString::new(cmd.as_str()).unwrap_or_default();
                    unsafe {
                        // Close all fds > 2 to avoid leaking compositor fds.
                        let max = libc::sysconf(libc::_SC_OPEN_MAX).max(256) as i32;
                        for fd in 3..max {
                            libc::close(fd);
                        }
                        libc::execl(
                            sh.as_ptr(),
                            sh.as_ptr(),
                            flag.as_ptr(),
                            cmd_c.as_ptr(),
                            std::ptr::null::<libc::c_char>(),
                        );
                        libc::_exit(127);
                    }
                }
                _ => unsafe { libc::_exit(0) },
            }
        }
        child if child > 0 => {
            // Parent: reap the first child immediately.
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(child, &mut status, 0) };
        }
        _ => {} // fork failed, silently ignore
    }
}

// ── Drm page-flip event handling ──────────────────────────────────────────────

/// Read and discard all pending DRM events on `fd` (page-flip acks etc).
/// Called after page_flip() to prevent the DRM event queue from filling up.
pub fn drain_drm_events(fd: BorrowedFd<'_>) {
    use std::os::unix::io::AsRawFd;
    // drmHandleEvent expects a drmEventContext; we do a raw read instead since
    // we don't need the timestamps.
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n <= 0 {
            break;
        }
    }
}
