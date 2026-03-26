use anyhow::{Context, Result};
use drm::control::{connector, crtc, framebuffer, Device as ControlDevice, PageFlipFlags};
use drm::Device as BasicDevice;
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd, RawFd};

pub struct ConnectorInfo {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub mode: drm::control::Mode,
    pub mode_w: u32,
    pub mode_h: u32,
    pub width_mm: u32,
    pub height_mm: u32,
}

pub struct DrmDevice {
    fd: OwnedFd,
}

impl AsFd for DrmDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
impl BasicDevice for DrmDevice {}
impl ControlDevice for DrmDevice {}

impl DrmDevice {
    pub fn open(path: &str) -> Result<Self> {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        let dev = Self {
            fd: OwnedFd::from(file),
        };
        dev.acquire_master_lock().ok();
        Ok(dev)
    }

    pub fn raw_fd(&self) -> RawFd {
        use std::os::unix::io::AsRawFd;
        self.fd.as_raw_fd()
    }

    pub fn enumerate_connectors(&self) -> Result<Vec<ConnectorInfo>> {
        let res = self.resource_handles().context("resource handles")?;
        let mut out = vec![];
        for &conn_h in res.connectors() {
            let conn = self.get_connector(conn_h, false).context("get connector")?;
            if conn.state() != connector::State::Connected {
                continue;
            }
            let Some(&mode) = conn.modes().first() else {
                continue;
            };
            let enc_h = conn
                .current_encoder()
                .or_else(|| conn.encoders().first().copied())
                .context("no encoder")?;
            let enc = self.get_encoder(enc_h).context("get encoder")?;
            let crtc = enc.crtc().context("encoder has no crtc")?;
            out.push(ConnectorInfo {
                connector: conn_h,
                crtc,
                mode,
                mode_w: mode.size().0 as u32,
                mode_h: mode.size().1 as u32,
                width_mm: conn.size().map(|(w, _)| w as u32).unwrap_or(0),
                height_mm: conn.size().map(|(_, h)| h as u32).unwrap_or(0),
            });
        }
        Ok(out)
    }

    pub fn set_crtc(
        &self,
        crtc: crtc::Handle,
        fb: Option<framebuffer::Handle>,
        pos: (u32, u32),
        connectors: &[connector::Handle],
        mode: Option<drm::control::Mode>,
    ) -> Result<()> {
        ControlDevice::set_crtc(self, crtc, fb, pos, connectors, mode).context("set_crtc")
    }

    pub fn page_flip(
        &self,
        crtc: crtc::Handle,
        fb: framebuffer::Handle,
        flags: PageFlipFlags,
    ) -> Result<()> {
        ControlDevice::page_flip(self, crtc, fb, flags, None::<drm::control::PageFlipTarget>)
            .context("page_flip")
    }

    pub fn add_framebuffer(
        &self,
        bo: &gbm::BufferObject<()>,
        depth: u32,
        bpp: u32,
    ) -> Result<framebuffer::Handle> {
        ControlDevice::add_framebuffer(self, bo, depth, bpp).context("add_framebuffer")
    }

    pub fn destroy_framebuffer(&self, fb: framebuffer::Handle) -> Result<()> {
        ControlDevice::destroy_framebuffer(self, fb).context("destroy_framebuffer")
    }
}
