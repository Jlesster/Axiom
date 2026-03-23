// src/proto/shm.rs — wl_shm / wl_shm_pool / wl_buffer (shared-memory buffers).

use std::{
    ffi::c_void,
    os::unix::io::OwnedFd,
    ptr, slice,
    sync::{Arc, Mutex},
};

use wayland_server::{
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_shm::{self, WlShm},
        wl_shm_pool::{self, WlShmPool},
    },
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};

use crate::state::Axiom;

// ── SHM format table ──────────────────────────────────────────────────────────

const SUPPORTED_FORMATS: &[wl_shm::Format] = &[
    wl_shm::Format::Argb8888,
    wl_shm::Format::Xrgb8888,
    wl_shm::Format::Abgr8888,
    wl_shm::Format::Xbgr8888,
] as &[wl_shm::Format];

// ── Pool data ─────────────────────────────────────────────────────────────────

pub struct ShmPool {
    inner: Arc<Mutex<ShmPoolInner>>,
}

pub struct ShmPoolInner {
    ptr: ptr::NonNull<u8>,
    pub size: usize,
    fd: OwnedFd,
}

unsafe impl Send for ShmPoolInner {}
unsafe impl Sync for ShmPoolInner {}

impl ShmPoolInner {
    fn mmap(fd: &OwnedFd, size: usize) -> anyhow::Result<ptr::NonNull<u8>> {
        use std::os::unix::io::AsRawFd;
        let ptr = unsafe {
            crate::sys::mmap(
                ptr::null_mut(),
                size,
                crate::sys::PROT_READ,
                crate::sys::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == crate::sys::MAP_FAILED {
            anyhow::bail!("mmap failed: {}", std::io::Error::last_os_error());
        }
        Ok(ptr::NonNull::new(ptr as *mut u8).unwrap())
    }

    pub fn new(fd: OwnedFd, size: usize) -> anyhow::Result<Self> {
        let ptr = Self::mmap(&fd, size)?;
        Ok(Self { ptr, size, fd })
    }

    pub fn resize(&mut self, new_size: usize) -> anyhow::Result<()> {
        // Map the new region BEFORE unmapping the old one so that if the new
        // mmap fails self.ptr is never left dangling.
        let new_ptr = Self::mmap(&self.fd, new_size)?;
        unsafe { crate::sys::munmap(self.ptr.as_ptr() as *mut c_void, self.size) };
        self.ptr = new_ptr;
        self.size = new_size;
        Ok(())
    }

    /// Borrow raw bytes for a buffer slice (bounds-checked).
    /// The caller must hold the Mutex lock for the entire duration they
    /// use the returned slice — do not release the lock while GL or any
    /// other consumer is still reading from it.
    pub fn data(&self, offset: usize, len: usize) -> Option<&[u8]> {
        if offset.saturating_add(len) > self.size {
            return None;
        }
        Some(unsafe { slice::from_raw_parts(self.ptr.as_ptr().add(offset), len) })
    }

    pub fn fd_raw(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.fd.as_raw_fd()
    }
}

impl Drop for ShmPoolInner {
    fn drop(&mut self) {
        unsafe { crate::sys::munmap(self.ptr.as_ptr() as *mut c_void, self.size) };
    }
}

// ── Buffer data ───────────────────────────────────────────────────────────────

pub struct ShmBuffer {
    pub pool: Arc<Mutex<ShmPoolInner>>,
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: wl_shm::Format,
}

impl ShmBuffer {
    /// Read pixel data for this buffer (slice into the pool mmap).
    /// The lock is held for the duration of the closure so that a concurrent
    /// pool resize cannot unmap the memory while the caller is reading it.
    pub fn with_data<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&[u8]) -> R,
    {
        let pool = self.pool.lock().unwrap();
        let len = (self.stride * self.height) as usize;
        let data = pool.data(self.offset as usize, len)?;
        Some(f(data))
        // pool lock released here, after f() returns
    }

    /// Expose the pool's file descriptor so screencopy can mmap with PROT_WRITE.
    pub fn pool_fd_raw(&self) -> std::os::unix::io::RawFd {
        self.pool.lock().unwrap().fd_raw()
    }
}

// ── wl_shm global ─────────────────────────────────────────────────────────────

impl GlobalDispatch<WlShm, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WlShm>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let shm = data_init.init(resource, ());
        for &fmt in SUPPORTED_FORMATS.iter() {
            shm.format(fmt);
        }
    }
}

impl Dispatch<WlShm, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlShm,
        request: wl_shm::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_shm::Request::CreatePool { id, fd, size } => {
                match ShmPoolInner::new(fd, size as usize) {
                    Ok(inner) => {
                        let pool = ShmPool {
                            inner: Arc::new(Mutex::new(inner)),
                        };
                        data_init.init(id, pool);
                    }
                    Err(e) => {
                        log::error!("wl_shm: failed to create pool: {}", e);
                    }
                }
            }
            _ => {}
        }
    }
}

// ── wl_shm_pool dispatch ──────────────────────────────────────────────────────

impl Dispatch<WlShmPool, ShmPool> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlShmPool,
        request: wl_shm_pool::Request,
        data: &ShmPool,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                if let Ok(fmt) = format.into_result() {
                    let buf = ShmBuffer {
                        pool: Arc::clone(&data.inner),
                        offset,
                        width,
                        height,
                        stride,
                        format: fmt,
                    };
                    data_init.init(id, buf);
                }
            }

            wl_shm_pool::Request::Resize { size } => {
                let mut inner = data.inner.lock().unwrap();
                if let Err(e) = inner.resize(size as usize) {
                    log::error!("wl_shm_pool resize failed: {}", e);
                }
            }

            wl_shm_pool::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── wl_buffer dispatch ────────────────────────────────────────────────────────

impl Dispatch<WlBuffer, ShmBuffer> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WlBuffer,
        request: wl_buffer::Request,
        _data: &ShmBuffer,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_buffer::Request::Destroy = request {
            state.render.release_buffer(resource);
        }
    }
}

// (libc FFI lives in crate::sys)
pub const PROT_READ: i32 = 0x1;
pub const MAP_SHARED: i32 = 0x01;
pub const MAP_FAILED: *mut c_void = !0usize as *mut _;
