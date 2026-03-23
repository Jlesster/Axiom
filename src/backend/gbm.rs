// src/backend/gbm.rs — GBM device and surface.

use anyhow::Result;
use drm::control::{crtc, framebuffer, Mode};
use drm_fourcc::DrmFourcc;
use gbm::{BufferObject, BufferObjectFlags, Surface as GbmSurface};
use std::os::unix::io::{AsFd, OwnedFd};

pub use gbm::Device as GbmDev;

pub struct OutputSurface {
    pub surface: GbmSurface<()>,
    pub width: u32,
    pub height: u32,
    pub format: DrmFourcc,
    pub egl_surface: khronos_egl::Surface,
    pub crtc: crtc::Handle,
    pub mode: Mode,
    pub front_fb: Option<framebuffer::Handle>,
    pub pending_fb: Option<framebuffer::Handle>,
    pub front_bo: Option<BufferObject<()>>,
    pub pending_bo: Option<BufferObject<()>>,
}

impl OutputSurface {
    pub fn post_swap(&mut self, drm: &impl drm::control::Device) -> Result<framebuffer::Handle> {
        let bo = unsafe { self.surface.lock_front_buffer() }?;
        let fb = drm.add_framebuffer(&bo, 24, 32)?;
        if let Some(old) = self.pending_fb.take() {
            let _ = drm.destroy_framebuffer(old);
        }
        self.pending_bo = self.front_bo.replace(bo);
        self.pending_fb = self.front_fb.replace(fb);
        Ok(fb)
    }

    pub fn on_flip_complete(&mut self) {
        self.pending_bo = None;
    }
    pub fn mode_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

pub fn open_gbm(fd: OwnedFd) -> Result<GbmDev<OwnedFd>> {
    Ok(GbmDev::new(fd)?)
}

pub fn create_surface(
    device: &GbmDev<impl AsFd>,
    width: u32,
    height: u32,
    format: DrmFourcc,
) -> Result<GbmSurface<()>> {
    Ok(device.create_surface(
        width,
        height,
        format,
        BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING,
    )?)
}
