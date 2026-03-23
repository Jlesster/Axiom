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
    // The framebuffer currently being scanned out by the CRTC.
    pub scanning_fb: Option<framebuffer::Handle>,
    pub scanning_bo: Option<BufferObject<()>>,
    // The framebuffer submitted to page_flip, not yet confirmed by the flip event.
    pub pending_fb: Option<framebuffer::Handle>,
    pub pending_bo: Option<BufferObject<()>>,
}

impl OutputSurface {
    /// Called after eglSwapBuffers. Locks the new front GBM buffer, wraps it
    /// in a DRM framebuffer, and returns the handle to pass to page_flip.
    ///
    /// Ownership flow:
    ///   new BO/FB  → pending   (submitted to flip, not yet live)
    ///   old pending → scanning (was confirmed live by the previous flip event)
    ///   old scanning → LEFT ALONE here; destroyed only in on_flip_complete
    ///
    /// IMPORTANT: scanning_fb must NOT be destroyed here. The CRTC may still
    /// be actively scanning it out. Only on_flip_complete() may retire it,
    /// because only then has the hardware confirmed it has moved to the new fb.
    pub fn post_swap(&mut self, drm: &impl drm::control::Device) -> Result<framebuffer::Handle> {
        let bo = unsafe { self.surface.lock_front_buffer() }?;
        let fb = drm.add_framebuffer(&bo, 24, 32)?;

        // On the very first frame there is no pending buffer yet, only a
        // scanning one (set by the initial modeset).  It is safe to retire it
        // now because no page_flip has been queued against it — the modeset
        // itself is synchronous and the CRTC will move to our new fb
        // immediately when set_crtc is called.
        if self.pending_fb.is_none() {
            if let Some(old_fb) = self.scanning_fb.take() {
                let _ = drm.destroy_framebuffer(old_fb);
            }
            self.scanning_bo = None;
        }

        // Promote the previously-pending buffer to scanning.
        // (On the first frame this is a no-op because pending was None above.)
        self.scanning_fb = self.pending_fb.take();
        self.scanning_bo = self.pending_bo.take();

        // Record the new submission as pending.
        self.pending_fb = Some(fb);
        self.pending_bo = Some(bo);

        Ok(fb)
    }

    /// Called when the DRM page-flip event fires for this CRTC.
    /// At this point the pending buffer is now live on the CRTC.
    /// The scanning buffer has been replaced and is safe to destroy.
    pub fn on_flip_complete(&mut self, drm: &impl drm::control::Device) {
        // scanning was the previously-live buffer — the hardware has now moved
        // on, so it is safe to release.
        if let Some(old_fb) = self.scanning_fb.take() {
            let _ = drm.destroy_framebuffer(old_fb);
        }
        self.scanning_bo = None;

        // pending is now the live buffer — promote to scanning.
        self.scanning_fb = self.pending_fb.take();
        self.scanning_bo = self.pending_bo.take();
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
