mod drm;
mod egl;
mod gbm;
mod output;
pub mod session;

pub use output::OutputSurface;

use anyhow::{Context, Result};
use std::sync::Arc;

use self::drm::DrmDevice;
use self::egl::EglContext;
use self::gbm::GbmDevice;

pub struct Backend {
    pub drm: Arc<DrmDevice>,
    pub gbm: GbmDevice,
    pub egl: EglContext,
    pub outputs: Vec<OutputSurface>,
}

impl Backend {
    pub fn init() -> Result<Self> {
        let drm_path = find_drm_device()?;
        tracing::info!("Using DRM device: {drm_path}");

        let drm = Arc::new(DrmDevice::open(&drm_path).context("open DRM device")?);
        let connectors = drm.enumerate_connectors().context("enumerate connectors")?;

        if connectors.is_empty() {
            anyhow::bail!("No active connectors found");
        }

        let gbm = GbmDevice::new(Arc::clone(&drm)).context("create GBM device")?;
        let egl = EglContext::new(&gbm).context("create EGL context")?;

        let mut outputs = vec![];
        for conn in &connectors {
            match OutputSurface::new(&drm, &gbm, &egl, conn) {
                Ok(out) => {
                    tracing::info!("Output {}x{}", conn.mode_w, conn.mode_h);
                    outputs.push(out);
                }
                Err(e) => tracing::warn!("Output init failed: {e}"),
            }
        }

        if outputs.is_empty() {
            anyhow::bail!("No outputs initialised");
        }

        Ok(Self {
            drm,
            gbm,
            egl,
            outputs,
        })
    }

    pub fn output_size(&self) -> (u32, u32) {
        self.outputs
            .first()
            .map(|o| (o.width, o.height))
            .unwrap_or((1920, 1080))
    }
}

fn find_drm_device() -> Result<String> {
    if let Ok(dev) = std::env::var("DRM_DEVICE") {
        return Ok(dev);
    }
    for i in 0..8 {
        let path = format!("/dev/dri/card{i}");
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
    }
    anyhow::bail!("No DRM device found — set DRM_DEVICE env var")
}
