// src/proto/dmabuf.rs — zwp_linux_dmabuf_v1 / zwp_linux_buffer_params_v1.
//
// Hardware clients (GPU-rendered apps, video players, screen capture) submit
// buffers as DMA-BUF fds rather than SHM.  We import them as EGLImage and
// bind as GL textures.
//
// The protocol flow is:
//   1. Client binds zwp_linux_dmabuf_v1 → we advertise supported formats.
//   2. Client calls create_params → zwp_linux_buffer_params_v1.
//   3. Client calls add() for each plane, then create()/create_immed().
//   4. We call eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT) → wl_buffer.

use std::os::unix::io::OwnedFd;
use std::sync::{Arc, Mutex};

use wayland_protocols::wp::linux_dmabuf::zv1::server::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
};
use wayland_server::{
    protocol::wl_buffer::{self, WlBuffer},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::Axiom;

// ── Supported DRM formats ─────────────────────────────────────────────────────
// DRM fourcc codes for formats we can import as EGLImage.

const SUPPORTED_FORMATS: &[(u32, u64)] = &[
    (0x34325241, 0), // DRM_FORMAT_ARGB8888, no modifier
    (0x34325258, 0), // DRM_FORMAT_XRGB8888, no modifier
    (0x3231564e, 0), // DRM_FORMAT_NV12, no modifier (video)
];

// ── DMA-BUF plane data ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct DmaBufPlane {
    pub fd: Option<OwnedFd>,
    pub offset: u32,
    pub stride: u32,
    pub modifier_hi: u32,
    pub modifier_lo: u32,
}

pub struct DmaBufParams {
    pub planes: Vec<DmaBufPlane>,
    pub created: bool,
}

impl Default for DmaBufParams {
    fn default() -> Self {
        Self {
            planes: (0..4).map(|_| DmaBufPlane::default()).collect(),
            created: false,
        }
    }
}

/// User data on WlBuffers created from DMA-BUF.
pub struct DmaBufBuffer {
    pub width: i32,
    pub height: i32,
    pub format: u32,
    pub flags: u32,
    pub planes: Vec<DmaBufPlane>,
    /// EGLImage handle (opaque usize so we don't pull in egl types here).
    pub egl_image: Option<usize>,
}

// ── zwp_linux_dmabuf_v1 global ────────────────────────────────────────────────

impl GlobalDispatch<ZwpLinuxDmabufV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpLinuxDmabufV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let dmabuf = data_init.init(resource, ());

        // Advertise supported formats + modifiers.
        for &(format, modifier) in SUPPORTED_FORMATS {
            let mod_hi = (modifier >> 32) as u32;
            let mod_lo = (modifier & 0xffffffff) as u32;
            if dmabuf.version() >= 3 {
                dmabuf.modifier(format, mod_hi, mod_lo);
            } else {
                dmabuf.format(format);
            }
        }
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpLinuxDmabufV1,
        request: zwp_linux_dmabuf_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_linux_dmabuf_v1::Request::CreateParams { params_id } => {
                data_init.init(params_id, Arc::new(Mutex::new(DmaBufParams::default())));
            }
            zwp_linux_dmabuf_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── zwp_linux_buffer_params_v1 dispatch ───────────────────────────────────────

impl Dispatch<ZwpLinuxBufferParamsV1, Arc<Mutex<DmaBufParams>>> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        params_resource: &ZwpLinuxBufferParamsV1,
        request: zwp_linux_buffer_params_v1::Request,
        data: &Arc<Mutex<DmaBufParams>>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_linux_buffer_params_v1::Request::Add {
                fd,
                plane_idx,
                offset,
                stride,
                modifier_hi,
                modifier_lo,
            } => {
                let mut p = data.lock().unwrap();
                let idx = plane_idx as usize;
                if idx < p.planes.len() {
                    p.planes[idx] = DmaBufPlane {
                        fd: Some(fd),
                        offset,
                        stride,
                        modifier_hi,
                        modifier_lo,
                    };
                }
            }

            zwp_linux_buffer_params_v1::Request::Create {
                width,
                height,
                format,
                flags,
            } => {
                let p = data.lock().unwrap();
                match import_dmabuf(
                    state,
                    &p,
                    width,
                    height,
                    format,
                    flags.into_result().map(|f| f.bits()).unwrap_or(0),
                ) {
                    Ok(buf) => {
                        // Asynchronous path — emit the 'created' event with the buffer.
                        // We can't easily init a WlBuffer here without an id, so we emit
                        // 'failed' for the async path and recommend create_immed instead.
                        // Full impl would use a wl_buffer factory.
                        log::warn!("zwp_linux_dmabuf create() async path not fully implemented; use create_immed");
                        drop(buf);
                        params_resource.failed();
                    }
                    Err(e) => {
                        log::error!("DMA-BUF import failed: {}", e);
                        params_resource.failed();
                    }
                }
            }

            zwp_linux_buffer_params_v1::Request::CreateImmed {
                buffer_id,
                width,
                height,
                format,
                flags,
            } => {
                let p = data.lock().unwrap();
                let flags_bits = flags.into_result().map(|f| f.bits()).unwrap_or(0);
                match import_dmabuf(state, &p, width, height, format, flags_bits) {
                    Ok(buf) => {
                        data_init.init(buffer_id, buf);
                    }
                    Err(e) => {
                        log::error!("DMA-BUF create_immed failed: {}", e);
                        // post_error terminates the client connection cleanly
                        // rather than handing back a silently broken buffer.
                        params_resource.post_error(
                            zwp_linux_buffer_params_v1::Error::InvalidFormat,
                            format!("DMA-BUF import failed: {e}"),
                        );
                    }
                }
            }

            zwp_linux_buffer_params_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

fn import_dmabuf(
    state: &mut Axiom,
    params: &DmaBufParams,
    width: i32,
    height: i32,
    format: u32,
    flags: u32,
) -> anyhow::Result<DmaBufBuffer> {
    // Collect valid planes.
    let planes: Vec<DmaBufPlane> = params
        .planes
        .iter()
        .filter(|p| p.fd.is_some())
        .map(|p| DmaBufPlane {
            fd: p.fd.as_ref().map(|fd| {
                use std::os::unix::io::{AsFd, OwnedFd};
                // Duplicate the fd so DmaBufBuffer owns it independently.
                fd.try_clone().unwrap()
            }),
            offset: p.offset,
            stride: p.stride,
            modifier_hi: p.modifier_hi,
            modifier_lo: p.modifier_lo,
        })
        .collect();

    if planes.is_empty() {
        anyhow::bail!("DMA-BUF params has no planes");
    }

    // Stub: EGL DMA-BUF import returns opaque handle 0 until fully wired.
    let egl_image = Some(0usize);

    Ok(DmaBufBuffer {
        width,
        height,
        format,
        flags,
        planes,
        egl_image,
    })
}

// ── wl_buffer dispatch for DMA-BUF buffers ────────────────────────────────────

impl Dispatch<WlBuffer, DmaBufBuffer> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WlBuffer,
        request: wl_buffer::Request,
        _data: &DmaBufBuffer,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_buffer::Request::Destroy = request {
            state.render.release_buffer(resource);
        }
    }
}
