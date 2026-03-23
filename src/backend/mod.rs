// src/backend/mod.rs

pub mod drm;
pub mod egl;
pub mod gbm;
pub mod session;

pub use drm::DrmDevice;
pub use gbm::OutputSurface;
pub use session::Session;

use anyhow::{Context, Result};
use std::os::unix::io::OwnedFd;
use std::path::Path;

// drm::control types used here — imported from the drm crate directly,
// not through a re-export that doesn't exist in drm 0.12.
use ::drm::control::{crtc, framebuffer};

// ── OutputSurface extension methods ──────────────────────────────────────────

impl OutputSurface {
    pub fn make_current(&self, egl: &egl::EglContext) -> Result<()> {
        egl.make_current(&egl::EglSurface(self.egl_surface))
    }

    pub fn present(
        &mut self,
        egl: &egl::EglContext,
        drm: &DrmDevice,
    ) -> Result<framebuffer::Handle> {
        egl.swap_buffers(&egl::EglSurface(self.egl_surface))?;
        self.post_swap(drm)
    }

    pub fn page_flip(&self, drm: &DrmDevice, fb: framebuffer::Handle) -> Result<()> {
        drm.page_flip(self.crtc, fb)
    }
}

// ── Backend ───────────────────────────────────────────────────────────────────

pub struct Backend {
    pub session: Session,
    pub drm: DrmDevice,
    pub gbm_dev: gbm::GbmDev<OwnedFd>,
    pub egl: egl::EglContext,
}

impl Backend {
    pub fn open(path: &Path, mut session: Session) -> Result<Self> {
        let drm = DrmDevice::from_session(&mut session, path)?;
        let gbm_fd = drm.fd().try_clone_to_owned().context("dup DRM fd")?;
        let gbm_dev = gbm::open_gbm(gbm_fd)?;
        let egl = egl::EglContext::new(&gbm_dev)?;
        Ok(Self {
            session,
            drm,
            gbm_dev,
            egl,
        })
    }

    pub fn create_outputs(&mut self) -> Result<Vec<OutputSurface>> {
        self.drm.create_outputs(&self.gbm_dev, &self.egl)
    }

    pub fn dispatch_drm_events<F>(&mut self, on_flip: F)
    where
        F: FnMut(crtc::Handle),
    {
        self.drm.handle_events(on_flip);
    }
}
