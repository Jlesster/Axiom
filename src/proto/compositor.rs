use crate::state::Axiom;
use crate::wm::Rect;
use std::sync::Mutex;
use wayland_server::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::WlCallback,
        wl_compositor::{self, WlCompositor},
        wl_region::{self, WlRegion},
        wl_subcompositor::{self, WlSubcompositor},
        wl_subsurface::{self, WlSubsurface},
        wl_surface::{self, WlSurface},
    },
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

// ── Surface data ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct PendingState {
    pub buffer: Option<Option<WlBuffer>>,
    pub frame_callbacks: Vec<WlCallback>,
    pub dx: i32,
    pub dy: i32,
}

#[derive(Default)]
pub struct CommittedState {
    pub buffer: Option<WlBuffer>,
    pub frame_callbacks: Vec<WlCallback>,
    pub needs_upload: bool,
}

pub struct SurfaceData {
    pub pending: Mutex<PendingState>,
    pub current: Mutex<CommittedState>,
}

impl Default for SurfaceData {
    fn default() -> Self {
        Self {
            pending: Mutex::new(PendingState::default()),
            current: Mutex::new(CommittedState::default()),
        }
    }
}

#[derive(Default)]
pub struct RegionData {
    pub rects: Vec<Rect>,
}

// ── Commit handler ────────────────────────────────────────────────────────────

pub fn handle_surface_commit(state: &mut Axiom, surface: &WlSurface) {
    let Some(data) = surface.data::<SurfaceData>() else {
        return;
    };

    let (maybe_buf, needs_upload) = {
        let mut pending = data.pending.lock().unwrap();
        let mut current = data.current.lock().unwrap();

        if let Some(maybe_new_buf) = pending.buffer.take() {
            // Queue the OLD buffer for deferred release (after page flip).
            // Do NOT call old.release() here — the GPU may still be reading it.
            if let Some(old) = current.buffer.take() {
                state.render.queue_buffer_release(old);
            }
            current.buffer = maybe_new_buf;
            current.needs_upload = true;
        }

        current
            .frame_callbacks
            .extend(pending.frame_callbacks.drain(..));

        let nu = current.needs_upload;
        let buf = current.buffer.clone();
        (buf, nu)
    };

    // Fire frame callbacks immediately — this tells the client it can
    // render the next frame. We use the current time in milliseconds.
    let cbs: Vec<_> = data
        .current
        .lock()
        .unwrap()
        .frame_callbacks
        .drain(..)
        .collect();
    for cb in cbs {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_millis();
        cb.done(t);
    }

    // Upload wl_shm buffer contents to a GL texture
    if needs_upload {
        if let Some(buf) = maybe_buf {
            if let Some(shm_data) = buf.data::<crate::proto::shm::ShmBufferData>() {
                upload_shm_to_render(state, surface, shm_data);
            }
        }
        data.current.lock().unwrap().needs_upload = false;
    }

    // Notify xdg_shell of the commit
    crate::proto::xdg_shell::on_surface_commit(state, surface);
}

fn upload_shm_to_render(
    state: &mut Axiom,
    surface: &WlSurface,
    shm: &crate::proto::shm::ShmBufferData,
) {
    use std::os::unix::io::AsFd;

    let win_id = crate::proto::xdg_shell::toplevel_for_surface(surface)
        .and_then(|t| t.lock().unwrap().window_id);
    let Some(id) = win_id else { return };

    let total = (shm.offset + shm.stride * shm.height) as usize;

    // SAFETY: pool_fd is valid for the lifetime of the ShmBufferData.
    let mmap = match unsafe { crate::sys::MmapGuard::new(shm.pool_fd.as_fd(), 0, total) } {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("mmap shm pool: {e}");
            return;
        }
    };

    let slice = mmap.as_slice();
    let pixel_data = &slice[shm.offset as usize..];

    state.render.upload_shm_buffer(
        id, pixel_data, shm.width, shm.height, shm.stride, shm.format,
    );
}

// ── GlobalDispatch / Dispatch impls ─────────────────────────────────────────

impl GlobalDispatch<WlCompositor, ()> for Axiom {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<WlCompositor>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        di.init(res, ());
    }
}

impl Dispatch<WlCompositor, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _: &Client,
        _: &WlCompositor,
        req: wl_compositor::Request,
        _: &(),
        _dh: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_compositor::Request::CreateSurface { id } => {
                di.init(id, SurfaceData::default());
            }
            wl_compositor::Request::CreateRegion { id } => {
                di.init(id, RegionData::default());
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, SurfaceData> for Axiom {
    fn request(
        state: &mut Self,
        _: &Client,
        surface: &WlSurface,
        req: wl_surface::Request,
        data: &SurfaceData,
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_surface::Request::Attach { buffer, x, y } => {
                let mut p = data.pending.lock().unwrap();
                p.buffer = Some(buffer);
                p.dx = x;
                p.dy = y;
            }
            wl_surface::Request::Frame { callback } => {
                let cb = di.init(callback, ());
                data.pending.lock().unwrap().frame_callbacks.push(cb);
            }
            wl_surface::Request::Commit => {
                state.on_surface_commit(surface);
            }
            wl_surface::Request::Damage { .. } | wl_surface::Request::DamageBuffer { .. } => {}
            wl_surface::Request::SetInputRegion { .. }
            | wl_surface::Request::SetOpaqueRegion { .. } => {}
            wl_surface::Request::SetBufferScale { .. }
            | wl_surface::Request::SetBufferTransform { .. } => {}
            wl_surface::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlCallback,
        _: wayland_server::protocol::wl_callback::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<WlRegion, RegionData> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlRegion,
        _req: wl_region::Request,
        _data: &RegionData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        // Region add/subtract ops — currently not needed for basic compositing
    }
}

impl GlobalDispatch<WlSubcompositor, ()> for Axiom {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<WlSubcompositor>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        di.init(res, ());
    }
}

impl Dispatch<WlSubcompositor, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlSubcompositor,
        req: wl_subcompositor::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_subcompositor::Request::GetSubsurface {
                id,
                surface: _,
                parent: _,
            } => {
                di.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSubsurface, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlSubsurface,
        _: wl_subsurface::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
