// src/backend/drm.rs — DRM device wrapper (drm crate 0.12).

use std::{
    os::unix::io::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
    path::Path,
};

use anyhow::{Context, Result};
use drm::control::Device as ControlDevice;
use drm::control::{
    connector, crtc, framebuffer, Event, ModeTypeFlags, PageFlipFlags, ResourceHandles,
};
use drm::Device as DrmTrait;
use drm_fourcc::DrmFourcc;

use super::{
    egl::{EglContext, EglSurface},
    gbm::{create_surface as gbm_create_surface, GbmDev, OutputSurface},
    session::Session,
};

pub struct DrmDevice {
    fd: OwnedFd,
}

impl AsFd for DrmDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
impl AsRawFd for DrmDevice {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
impl DrmTrait for DrmDevice {}
impl ControlDevice for DrmDevice {}

impl DrmDevice {
    pub fn from_session(session: &mut Session, path: &Path) -> Result<Self> {
        let fd = session.open_device(path)?;
        let dev = Self { fd };
        // libseat grants DRM master implicitly — do NOT call acquire_master_lock().
        let _ = dev.set_client_capability(drm::ClientCapability::UniversalPlanes, true);
        let _ = dev.set_client_capability(drm::ClientCapability::Atomic, true);
        Ok(dev)
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn create_outputs(
        &self,
        gbm: &GbmDev<OwnedFd>,
        egl: &EglContext,
    ) -> Result<Vec<OutputSurface>> {
        let res = self.resource_handles().context("DRM resource_handles")?;
        let mut outputs = Vec::new();

        for &conn_h in res.connectors() {
            let conn = self.get_connector(conn_h, true).context("get_connector")?;
            if conn.state() != connector::State::Connected {
                continue;
            }

            let Some(mode) = best_mode(conn.modes()) else {
                tracing::warn!("connector {:?}: no modes, skipping", conn_h);
                continue;
            };

            let crtc_h = match conn
                .current_encoder()
                .and_then(|eh| self.get_encoder(eh).ok())
                .and_then(|enc| enc.crtc())
            {
                Some(c) => c,
                None => match self.find_crtc_for_connector(&res, conn_h) {
                    Some(c) => c,
                    None => {
                        tracing::warn!("no CRTC for connector {:?}", conn_h);
                        continue;
                    }
                },
            };

            match self.setup_output(gbm, egl, crtc_h, conn_h, mode) {
                Ok(surf) => outputs.push(surf),
                Err(e) => tracing::warn!("output setup failed for {:?}: {e}", conn_h),
            }
        }
        Ok(outputs)
    }

    fn setup_output(
        &self,
        gbm: &GbmDev<OwnedFd>,
        egl: &EglContext,
        crtc_h: crtc::Handle,
        conn_h: connector::Handle,
        mode: drm::control::Mode,
    ) -> Result<OutputSurface> {
        let (w, h) = (mode.size().0 as u32, mode.size().1 as u32);

        let gbm_surface = gbm_create_surface(gbm, w, h, DrmFourcc::Xrgb8888)?;
        let egl_surface: khronos_egl::Surface = egl.create_window_surface(&gbm_surface)?.0;

        egl.make_current(&EglSurface(egl_surface))?;
        egl.swap_buffers(&EglSurface(egl_surface))?;

        let bo = unsafe { gbm_surface.lock_front_buffer() }?;
        let fb = self
            .add_framebuffer(&bo, 24, 32)
            .context("drmModeAddFB (initial)")?;

        self.set_crtc(crtc_h, Some(fb), (0, 0), &[conn_h], Some(mode))
            .context("set_crtc")?;

        tracing::info!(
            "Output ready: {:?} {}x{}@{}Hz",
            conn_h,
            w,
            h,
            mode.vrefresh()
        );

        Ok(OutputSurface {
            surface: gbm_surface,
            width: w,
            height: h,
            format: DrmFourcc::Xrgb8888,
            egl_surface,
            crtc: crtc_h,
            mode,
            front_fb: Some(fb),
            pending_fb: None,
            front_bo: Some(bo),
            pending_bo: None,
        })
    }

    fn find_crtc_for_connector(
        &self,
        res: &ResourceHandles,
        conn: connector::Handle,
    ) -> Option<crtc::Handle> {
        let conn_info = self.get_connector(conn, false).ok()?;
        for &enc_h in conn_info.encoders() {
            if let Ok(enc) = self.get_encoder(enc_h) {
                // filter_crtcs returns Vec<crtc::Handle> — use .first()
                let compatible = res.filter_crtcs(enc.possible_crtcs());
                if let Some(&crtc_h) = compatible.first() {
                    return Some(crtc_h);
                }
            }
        }
        None
    }

    pub fn page_flip(&self, crtc_h: crtc::Handle, fb: framebuffer::Handle) -> Result<()> {
        ControlDevice::page_flip(self, crtc_h, fb, PageFlipFlags::EVENT, None)
            .context("drmModePageFlip")
    }

    pub fn handle_events<F>(&self, mut on_flip: F)
    where
        F: FnMut(crtc::Handle),
    {
        match self.receive_events() {
            Ok(events) => {
                for event in events {
                    if let Event::PageFlip(e) = event {
                        on_flip(e.crtc);
                    }
                }
            }
            Err(e) => tracing::warn!("DRM receive_events: {e}"),
        }
    }
}

fn best_mode(modes: &[drm::control::Mode]) -> Option<drm::control::Mode> {
    modes
        .iter()
        .max_by_key(|m| {
            let preferred = u32::from(m.mode_type().contains(ModeTypeFlags::PREFERRED));
            (preferred, m.vrefresh())
        })
        .copied()
}
