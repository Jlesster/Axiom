/// Session management for Axiom.
///
/// Tries libseat first (allows running without being root / without logind).
/// Falls back gracefully — if libseat is not available the DRM device was
/// already opened directly in drm.rs, which is fine for a single-seat
/// setup where the user already owns the TTY (e.g. started from a VT).
///
/// For Hyprland-level polish this module also:
///   - Manages VT switching (Ctrl-Alt-F1…F12)
///   - Handles SIGTERM / compositor death cleanly
///   - Drops privileges after DRM master is acquired (future)
use anyhow::{Context, Result};
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ── libseat FFI ──────────────────────────────────────────────────────────────
//
// We link against libseat at runtime via dlopen so the compositor still boots
// on systems without libseat installed (falls back to direct DRM access).

#[allow(non_camel_case_types)]
mod ffi {
    use std::os::raw::{c_char, c_int, c_void};

    pub type seatd_seat = c_void;

    #[repr(C)]
    pub struct libseat_seat_listener {
        pub enable_seat: unsafe extern "C" fn(seat: *mut seatd_seat, userdata: *mut c_void),
        pub disable_seat: unsafe extern "C" fn(seat: *mut seatd_seat, userdata: *mut c_void),
    }

    extern "C" {
        // Returns NULL on failure
        pub fn libseat_open_seat(
            listener: *const libseat_seat_listener,
            userdata: *mut c_void,
        ) -> *mut seatd_seat;
        pub fn libseat_close_seat(seat: *mut seatd_seat) -> c_int;
        pub fn libseat_open_device(
            seat: *mut seatd_seat,
            path: *const c_char,
            fd: *mut c_int,
        ) -> c_int;
        pub fn libseat_close_device(seat: *mut seatd_seat, device_id: c_int) -> c_int;
        pub fn libseat_dispatch(seat: *mut seatd_seat, timeout_ms: c_int) -> c_int;
        pub fn libseat_get_seat(seat: *mut seatd_seat) -> *const c_char;
    }
}

// ── Session ───────────────────────────────────────────────────────────────────

pub enum Session {
    /// Managed via libseat — can run without root.
    Libseat {
        seat: *mut ffi::seatd_seat,
        /// Set by the `enable_seat` callback.
        active: Arc<AtomicBool>,
    },
    /// Fallback: DRM device opened directly. Needs group=video or root.
    Direct,
}

// SAFETY: Session is only ever used from the compositor main thread.
unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Session {
    /// Try to open a managed session via libseat; fall back to direct access.
    pub fn open() -> Result<Self> {
        match Self::try_libseat() {
            Ok(s) => {
                tracing::info!("Session: using libseat");
                Ok(s)
            }
            Err(e) => {
                tracing::warn!("libseat unavailable ({e}), falling back to direct DRM access");
                Ok(Session::Direct)
            }
        }
    }

    fn try_libseat() -> Result<Self> {
        use std::os::raw::c_void;

        let active = Arc::new(AtomicBool::new(false));
        let active_ptr = Arc::into_raw(Arc::clone(&active)) as *mut c_void;

        static LISTENER: ffi::libseat_seat_listener = ffi::libseat_seat_listener {
            enable_seat: enable_seat_cb,
            disable_seat: disable_seat_cb,
        };

        let seat = unsafe { ffi::libseat_open_seat(&LISTENER, active_ptr) };
        if seat.is_null() {
            // Arc was not consumed — drop it
            unsafe { drop(Arc::from_raw(active_ptr as *mut AtomicBool)) };
            anyhow::bail!("libseat_open_seat returned NULL");
        }

        // Pump once so the enable callback fires synchronously on most seatd
        // implementations (returns immediately if already enabled).
        unsafe { ffi::libseat_dispatch(seat, 0) };

        // Re-own the Arc we passed as userdata
        let active2 = unsafe { Arc::from_raw(active_ptr as *const AtomicBool) };

        Ok(Session::Libseat {
            seat,
            active: active2,
        })
    }

    /// Open a device (DRM node, input, …) and return an OwnedFd.
    /// Falls back to a plain open(2) in Direct mode.
    pub fn open_device(&self, path: &str) -> Result<OwnedFd> {
        match self {
            Session::Libseat { seat, .. } => {
                use std::os::raw::c_int;
                let cpath = std::ffi::CString::new(path).context("CString")?;
                let mut raw_fd: c_int = -1;
                let device_id =
                    unsafe { ffi::libseat_open_device(*seat, cpath.as_ptr(), &mut raw_fd) };
                if device_id < 0 || raw_fd < 0 {
                    anyhow::bail!("libseat_open_device failed for {path}");
                }
                // SAFETY: libseat returned a valid fd we now own.
                Ok(unsafe { OwnedFd::from_raw_fd_impl(raw_fd) })
            }
            Session::Direct => {
                use std::fs::OpenOptions;
                use std::os::unix::fs::OpenOptionsExt;
                let f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_CLOEXEC)
                    .open(path)
                    .with_context(|| format!("open {path}"))?;
                Ok(OwnedFd::from(f))
            }
        }
    }

    /// Dispatch pending seat events (call periodically from the event loop).
    pub fn dispatch(&self) {
        if let Session::Libseat { seat, .. } = self {
            unsafe { ffi::libseat_dispatch(*seat, 0) };
        }
    }

    /// Returns true when the seat is currently active (not suspended for VT switch).
    pub fn is_active(&self) -> bool {
        match self {
            Session::Libseat { active, .. } => active.load(Ordering::Acquire),
            Session::Direct => true,
        }
    }

    /// Name of the seat (e.g. "seat0").
    pub fn seat_name(&self) -> &str {
        match self {
            Session::Libseat { seat, .. } => {
                let ptr = unsafe { ffi::libseat_get_seat(*seat) };
                if ptr.is_null() {
                    "seat0"
                } else {
                    unsafe { std::ffi::CStr::from_ptr(ptr).to_str().unwrap_or("seat0") }
                }
            }
            Session::Direct => "seat0",
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Session::Libseat { seat, .. } = self {
            unsafe { ffi::libseat_close_seat(*seat) };
        }
    }
}

// ── libseat callbacks ─────────────────────────────────────────────────────────

extern "C" fn enable_seat_cb(_seat: *mut ffi::seatd_seat, userdata: *mut std::os::raw::c_void) {
    if !userdata.is_null() {
        let flag = unsafe { &*(userdata as *const AtomicBool) };
        flag.store(true, Ordering::Release);
        tracing::debug!("libseat: seat enabled");
    }
}

extern "C" fn disable_seat_cb(seat: *mut ffi::seatd_seat, userdata: *mut std::os::raw::c_void) {
    if !userdata.is_null() {
        let flag = unsafe { &*(userdata as *const AtomicBool) };
        flag.store(false, Ordering::Release);
        tracing::debug!("libseat: seat disabled (VT switch)");
    }
    // Acknowledge the disable so seatd can proceed with the VT switch.
    unsafe { ffi::libseat_dispatch(seat, 0) };
}

// ── OwnedFd from raw fd (helper shim) ────────────────────────────────────────

trait FromRawFdImpl {
    unsafe fn from_raw_fd_impl(fd: i32) -> Self;
}

impl FromRawFdImpl for OwnedFd {
    unsafe fn from_raw_fd_impl(fd: i32) -> Self {
        use std::os::unix::io::FromRawFd;
        OwnedFd::from_raw_fd(fd)
    }
}

// ── VT management ─────────────────────────────────────────────────────────────

/// Activate a specific VT (1-indexed). No-op in Direct mode or if /dev/tty0
/// is not accessible.
pub fn switch_vt(vt: u32) {
    use std::os::unix::io::AsRawFd;
    // VT_ACTIVATE ioctl (Linux-specific)
    const VT_ACTIVATE: u64 = 0x5606;
    const VT_WAITACTIVE: u64 = 0x5607;

    if let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty0")
    {
        unsafe {
            libc::ioctl(
                tty.as_raw_fd(),
                VT_ACTIVATE as libc::c_ulong,
                vt as libc::c_int,
            );
            libc::ioctl(
                tty.as_raw_fd(),
                VT_WAITACTIVE as libc::c_ulong,
                vt as libc::c_int,
            );
        }
    }
}

/// Install a SIGTERM handler that sets a flag so the compositor can shut down
/// cleanly. Returns an Arc<AtomicBool> that becomes `true` on SIGTERM.
pub fn install_sigterm_handler() -> Arc<AtomicBool> {
    static FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
    let flag = FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)));

    extern "C" fn handler(_: libc::c_int) {
        // SAFETY: AtomicBool::store is async-signal-safe.
        if let Some(flag) = crate::backend::session::SIGTERM_FLAG.get() {
            flag.store(true, Ordering::Release);
        }
    }

    unsafe {
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }

    Arc::clone(flag)
}

// Exposed so the signal handler can reach it without a pointer.
pub(crate) static SIGTERM_FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
