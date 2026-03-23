// src/proto/dmabuf.rs — zwp_linux_dmabuf_v1 / zwp_linux_buffer_params_v1.
//
// We advertise version 3 only. Version 4 adds zwp_linux_dmabuf_feedback_v1;
// without a Dispatch impl for that object wayland-backend panics when a client
// requests it. Staying at v3 means the feedback requests never appear in the
// protocol and no client will ask for them.

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

use crate::{backend::egl::DmaBufPlane as EglPlane, state::Axiom};

// ── Supported DRM formats ─────────────────────────────────────────────────────

const SUPPORTED_FORMATS: &[(u32, u64)] = &[
    (0x34325241, 0), // DRM_FORMAT_ARGB8888, no modifier
    (0x34325258, 0), // DRM_FORMAT_XRGB8888, no modifier
    (0x3231564e, 0), // DRM_FORMAT_NV12,     no modifier (video)
];

// ── Protocol-level plane data ─────────────────────────────────────────────────

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

// ── Per-buffer data stored on successfully imported WlBuffers ─────────────────

pub struct DmaBufBuffer {
    pub width: i32,
    pub height: i32,
    pub format: u32,
    pub flags: u32,
    pub planes: Vec<DmaBufPlane>,
    pub egl_image: Arc<crate::backend::egl::EglImage>,
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

        // At v3 we advertise via modifier() if available, format() otherwise.
        // At v3 modifier() exists; format() is the v1/v2 fallback.
        for &(format, modifier) in SUPPORTED_FORMATS {
            let mod_hi = (modifier >> 32) as u32;
            let mod_lo = (modifier & 0xffff_ffff) as u32;
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
            // GetDefaultFeedback / GetSurfaceFeedback are v4-only requests.
            // We advertise v3, so a well-behaved client will never send these.
            // If one somehow does, just ignore it — the object id is never
            // initialised so wayland-server will disconnect the client.
            _ => {
                tracing::warn!(
                    "zwp_linux_dmabuf_v1: unexpected request (v4 feedback not supported)"
                );
            }
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
                } else {
                    tracing::warn!("dmabuf add(): plane_idx {} out of range", idx);
                }
            }

            zwp_linux_buffer_params_v1::Request::Create {
                width: _,
                height: _,
                format: _,
                flags: _,
            } => {
                // Async create path — uncommon; tell client to use create_immed.
                tracing::warn!(
                    "zwp_linux_dmabuf create() async path not implemented; sending failed"
                );
                params_resource.failed();
            }

            zwp_linux_buffer_params_v1::Request::CreateImmed {
                buffer_id,
                width,
                height,
                format,
                flags,
            } => {
                let mut p = data.lock().unwrap();

                if p.created {
                    params_resource.post_error(
                        zwp_linux_buffer_params_v1::Error::AlreadyUsed,
                        "params object already used",
                    );
                    return;
                }
                p.created = true;

                let flags_bits = flags.into_result().map(|f| f.bits()).unwrap_or(0);

                match do_import(state, &p, width, height, format, flags_bits) {
                    Ok(buf) => {
                        data_init.init(buffer_id, buf);
                    }
                    Err(e) => {
                        tracing::error!("DMA-BUF create_immed failed: {}", e);
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

// ── Import helper ─────────────────────────────────────────────────────────────

fn do_import(
    state: &mut Axiom,
    params: &DmaBufParams,
    width: i32,
    height: i32,
    format: u32,
    _flags: u32,
) -> anyhow::Result<DmaBufBuffer> {
    let valid_planes: Vec<&DmaBufPlane> = params.planes.iter().filter(|p| p.fd.is_some()).collect();

    if valid_planes.is_empty() {
        anyhow::bail!("DMA-BUF params has no planes");
    }

    let egl_planes: Vec<EglPlane> = valid_planes
        .iter()
        .map(|p| {
            use std::os::unix::io::AsRawFd;
            EglPlane {
                fd: p.fd.as_ref().unwrap().as_raw_fd(),
                offset: p.offset,
                stride: p.stride,
                modifier_hi: p.modifier_hi,
                modifier_lo: p.modifier_lo,
            }
        })
        .collect();

    let egl_image = state
        .backend
        .egl
        .import_dmabuf(width as u32, height as u32, format, &egl_planes)
        .ok_or_else(|| anyhow::anyhow!("eglCreateImageKHR returned NULL"))?;

    let owned_planes: Vec<DmaBufPlane> = params
        .planes
        .iter()
        .map(|p| DmaBufPlane {
            fd: p.fd.as_ref().and_then(|fd| fd.try_clone().ok()),
            offset: p.offset,
            stride: p.stride,
            modifier_hi: p.modifier_hi,
            modifier_lo: p.modifier_lo,
        })
        .collect();

    tracing::debug!(
        "DMA-BUF imported: format={:#010x} {}x{} {} planes",
        format,
        width,
        height,
        egl_planes.len()
    );

    Ok(DmaBufBuffer {
        width,
        height,
        format,
        flags: _flags,
        planes: owned_planes,
        egl_image: Arc::new(egl_image),
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
