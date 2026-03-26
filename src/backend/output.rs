use anyhow::{Context, Result};
use drm::control::{crtc, framebuffer, PageFlipFlags};
use khronos_egl as egl;

use super::drm::{ConnectorInfo, DrmDevice};
use super::egl::EglContext;
use super::gbm::GbmDevice;

pub struct OutputSurface {
    pub width: u32,
    pub height: u32,
    pub crtc: crtc::Handle,
    gbm_surface: gbm::Surface<()>,
    egl_surface: egl::Surface,
    front_fb: Option<framebuffer::Handle>,
    _front_bo: Option<gbm::BufferObject<()>>,
}

impl OutputSurface {
    pub fn new(
        drm: &DrmDevice,
        gbm: &GbmDevice,
        egl: &EglContext,
        conn: &ConnectorInfo,
    ) -> Result<Self> {
        let (w, h) = (conn.mode_w, conn.mode_h);
        let gbm_surface = gbm.create_surface(w, h)?;
        let egl_surface = egl.create_window_surface(&gbm_surface)?;

        egl.make_current(&egl_surface)?;
        egl.load_gl();
        unsafe {
            gl::Viewport(0, 0, w as i32, h as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
        egl.swap_buffers(&egl_surface)?;

        // SAFETY: GBM buffer has been fully rendered and swapped before locking.
        let bo = unsafe { gbm_surface.lock_front_buffer() }.context("lock front buffer")?;
        let fb = drm.add_framebuffer(&bo, 24, 32)?;
        drm.set_crtc(
            conn.crtc,
            Some(fb),
            (0, 0),
            &[conn.connector], // &[Handle; 1] coerces to &[Handle] fine — if not, use .as_slice()
            Some(conn.mode),
        )
        .context("initial set_crtc")?;

        Ok(Self {
            width: w,
            height: h,
            crtc: conn.crtc,
            gbm_surface,
            egl_surface,
            front_fb: Some(fb),
            _front_bo: Some(bo),
        })
    }

    pub fn make_current(&self, egl: &EglContext) -> Result<()> {
        egl.make_current(&self.egl_surface)
    }

    pub fn present(&mut self, drm: &DrmDevice, egl: &EglContext) -> Result<()> {
        egl.swap_buffers(&self.egl_surface)?;
        // SAFETY: rendering complete, buffer ownership transferred to GBM.
        let bo = unsafe { self.gbm_surface.lock_front_buffer() }.context("lock_front_buffer")?;
        let fb = drm.add_framebuffer(&bo, 24, 32)?;
        drm.page_flip(self.crtc, fb, PageFlipFlags::EVENT)?;
        if let Some(old) = self.front_fb.replace(fb) {
            drm.destroy_framebuffer(old).ok();
        }
        self._front_bo = Some(bo);
        Ok(())
    }
}
