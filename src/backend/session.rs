// src/backend/session.rs — libseat session management.

use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};
use std::path::Path;

// ── FFI declarations ──────────────────────────────────────────────────────────

#[repr(C)]
struct LibseatOpaque {
    _private: [u8; 0],
}

#[repr(C)]
struct LibseatListener {
    enable_seat: extern "C" fn(*mut LibseatOpaque, *mut SeatUserData),
    disable_seat: extern "C" fn(*mut LibseatOpaque, *mut SeatUserData),
}

#[link(name = "seat")]
extern "C" {
    fn libseat_open_seat(
        listener: *const LibseatListener,
        userdata: *mut SeatUserData,
    ) -> *mut LibseatOpaque;
    fn libseat_close_seat(seat: *mut LibseatOpaque) -> i32;
    fn libseat_open_device(seat: *mut LibseatOpaque, path: *const i8, device_id: *mut i32) -> i32;
    fn libseat_close_device(seat: *mut LibseatOpaque, device_id: i32) -> i32;
    fn libseat_dispatch(seat: *mut LibseatOpaque, timeout_ms: i32) -> i32;
    fn libseat_get_fd(seat: *mut LibseatOpaque) -> i32;
    /// Switch to a different VT.  Returns 0 on success, -1 on error.
    fn libseat_switch_session(seat: *mut LibseatOpaque, vt: i32) -> i32;
}

// ── Callbacks ─────────────────────────────────────────────────────────────────

#[repr(C)]
pub struct SeatUserData {
    pub enabled: bool,
    pub disable_pending: bool,
    pub enable_pending: bool,
}

extern "C" fn on_enable_seat(_seat: *mut LibseatOpaque, userdata: *mut SeatUserData) {
    if userdata.is_null() {
        return;
    }
    unsafe {
        (*userdata).enabled = true;
        (*userdata).enable_pending = true;
    }
    log::info!("libseat: seat enabled");
}

extern "C" fn on_disable_seat(_seat: *mut LibseatOpaque, userdata: *mut SeatUserData) {
    if userdata.is_null() {
        return;
    }
    unsafe {
        (*userdata).enabled = false;
        (*userdata).disable_pending = true;
    }
    log::info!("libseat: seat disabled (VT switch)");
}

static SEAT_LISTENER: LibseatListener = LibseatListener {
    enable_seat: on_enable_seat,
    disable_seat: on_disable_seat,
};

// ── Session ───────────────────────────────────────────────────────────────────

pub struct Session {
    seat: *mut LibseatOpaque,
    userdata: Box<SeatUserData>,
    /// fd to poll for seat events.
    pub fd: RawFd,
    /// Devices opened via libseat: path → (owned_fd, device_id).
    open_devices: std::collections::HashMap<std::path::PathBuf, (OwnedFd, i32)>,
}

// SAFETY: libseat pointer is owned exclusively by this struct.
unsafe impl Send for Session {}

impl Session {
    pub fn open() -> anyhow::Result<Self> {
        let mut userdata = Box::new(SeatUserData {
            enabled: false,
            disable_pending: false,
            enable_pending: false,
        });

        let seat = unsafe { libseat_open_seat(&SEAT_LISTENER, userdata.as_mut() as *mut _) };
        if seat.is_null() {
            anyhow::bail!("libseat_open_seat failed");
        }

        let fd = unsafe { libseat_get_fd(seat) };
        if fd < 0 {
            unsafe {
                libseat_close_seat(seat);
            }
            anyhow::bail!("libseat_get_fd returned {}", fd);
        }

        let mut session = Self {
            seat,
            userdata,
            fd,
            open_devices: Default::default(),
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !session.userdata.enabled {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("Timed out waiting for libseat enable_seat");
            }
            session.dispatch(100)?;
        }

        log::info!("libseat session opened (fd={})", fd);
        Ok(session)
    }

    pub fn dispatch(&mut self, timeout_ms: i32) -> anyhow::Result<()> {
        let ret = unsafe { libseat_dispatch(self.seat, timeout_ms) };
        if ret < 0 {
            anyhow::bail!("libseat_dispatch failed: {}", ret);
        }
        Ok(())
    }

    /// Request a VT switch.  The actual seat-disable callback arrives
    /// asynchronously via the seat fd.
    pub fn switch_vt(&mut self, vt: u32) -> anyhow::Result<()> {
        let ret = unsafe { libseat_switch_session(self.seat, vt as i32) };
        if ret < 0 {
            anyhow::bail!(
                "libseat_switch_session(vt={}) failed: errno {}",
                vt,
                unsafe { crate::sys::errno() }
            );
        }
        Ok(())
    }

    /// Open a device through libseat (no root required).
    pub fn open_device(&mut self, path: &Path) -> anyhow::Result<OwnedFd> {
        use std::ffi::CString;

        let cpath = CString::new(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 path"))?,
        )?;

        let mut device_id: i32 = -1;
        let raw_fd = unsafe { libseat_open_device(self.seat, cpath.as_ptr(), &mut device_id) };

        if raw_fd < 0 {
            anyhow::bail!("libseat_open_device({:?}) failed: errno {}", path, unsafe {
                crate::sys::errno()
            });
        }

        let owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        if let Ok(clone) = owned.try_clone() {
            self.open_devices
                .insert(path.to_path_buf(), (clone, device_id));
        } else {
            log::warn!(
                "open_device: could not clone fd for {:?}; close tracking disabled",
                path
            );
        }

        log::debug!(
            "Opened device {:?} fd={} device_id={}",
            path,
            raw_fd,
            device_id
        );
        Ok(owned)
    }

    pub fn close_device(&mut self, path: &Path) {
        if let Some((_fd, device_id)) = self.open_devices.remove(path) {
            unsafe {
                libseat_close_device(self.seat, device_id);
            }
        }
    }

    /// Expose the raw seat pointer so libinput can open devices through us.
    ///
    /// # Safety
    /// Caller must not outlive this Session.
    pub unsafe fn raw_seat(&self) -> *mut std::ffi::c_void {
        self.seat as *mut std::ffi::c_void
    }

    pub fn is_active(&self) -> bool {
        self.userdata.enabled
    }

    /// Returns true (and clears the flag) if the seat was *disabled* since
    /// the last call — i.e. we just lost DRM master.
    pub fn take_disable_pending(&mut self) -> bool {
        let v = self.userdata.disable_pending;
        self.userdata.disable_pending = false;
        v
    }

    /// Returns true (and clears the flag) if the seat was *re-enabled* since
    /// the last call — i.e. the user switched back to our VT.
    pub fn take_enable_pending(&mut self) -> bool {
        let v = self.userdata.enable_pending;
        self.userdata.enable_pending = false;
        v
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            libseat_close_seat(self.seat);
        }
    }
}

// (libc FFI lives in crate::sys)
