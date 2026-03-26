use anyhow::{Context, Result};
use gbm::{BufferObjectFlags, Device as GbmDev, Format};
use std::os::fd::AsFd; // ← brings AsFd into scope so Arc<DrmDevice>::as_fd() works
use std::sync::Arc;

use super::drm::DrmDevice;

pub struct GbmDevice {
    pub _drm: Arc<DrmDevice>,
    pub inner: GbmDev<std::os::unix::io::BorrowedFd<'static>>,
}

unsafe impl Send for GbmDevice {}

impl GbmDevice {
    pub fn new(drm: Arc<DrmDevice>) -> Result<Self> {
        // SAFETY: Arc keeps DrmDevice alive for at least as long as GbmDevice.
        let borrowed: std::os::unix::io::BorrowedFd<'static> =
            unsafe { std::mem::transmute(drm.as_fd()) };
        let inner = GbmDev::new(borrowed).context("gbm::Device::new")?;
        Ok(Self { _drm: drm, inner })
    }

    pub fn create_surface(&self, w: u32, h: u32) -> Result<gbm::Surface<()>> {
        self.inner
            .create_surface::<()>(
                w,
                h,
                Format::Xrgb8888,
                BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING,
            )
            .context("gbm create_surface")
    }
}
