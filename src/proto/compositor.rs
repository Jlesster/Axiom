// src/proto/compositor.rs — wl_compositor, wl_subcompositor, wl_surface, wl_region.
//
// Surface state follows the double-buffered Wayland model:
//   pending  — accumulates attach/damage/frame/etc requests
//   current  — applied atomically on wl_surface.commit
//
// We deliberately do NOT use smithay's CompositorState. All commit logic
// is hand-rolled here so we have full control over when textures are uploaded
// and how subsurfaces are ordered.

use crate::proto::fractional_scale;
use std::sync::{Arc, Mutex};

use wayland_server::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::WlCallback,
        wl_compositor::{self, WlCompositor},
        wl_output,
        wl_region::{self, WlRegion},
        wl_subcompositor::{self, WlSubcompositor},
        wl_subsurface::{self, WlSubsurface},
        wl_surface::{self, WlSurface},
    },
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::Axiom;

// ── Surface user-data ─────────────────────────────────────────────────────────

/// Geometry of committed damage (surface-local coordinates).
#[derive(Default, Clone)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// The pending (pre-commit) surface state.
#[derive(Default)]
pub struct PendingSurfaceState {
    pub buffer: Option<Option<WlBuffer>>, // None = no change; Some(None) = detach
    pub dx: i32,
    pub dy: i32,
    pub damage_surface: Vec<Rect>,
    pub damage_buffer: Vec<Rect>,
    pub frame_callbacks: Vec<WlCallback>,
    pub input_region: Option<Option<RegionData>>,
    pub opaque_region: Option<Option<RegionData>>,
    pub buffer_scale: Option<i32>,
    pub buffer_transform: Option<wl_output::Transform>,
}

/// The committed (current) surface state — what the renderer sees.
#[derive(Default)]
pub struct CommittedSurfaceState {
    pub buffer: Option<WlBuffer>,
    pub dx: i32,
    pub dy: i32,
    pub damage_buffer: Vec<Rect>,
    pub frame_callbacks: Vec<WlCallback>,
    pub input_region: Option<RegionData>,
    pub opaque_region: Option<RegionData>,
    pub buffer_scale: i32,
    /// None means Normal (the default).
    pub buffer_transform: Option<wl_output::Transform>,
    pub needs_upload: bool,
}

// wl_output::Transform has no Default impl — we inline Normal at construction.

/// Shared surface data stored as WlSurface user data.
pub struct SurfaceData {
    pub pending: Mutex<PendingSurfaceState>,
    pub current: Mutex<CommittedSurfaceState>,
    pub children: Mutex<Vec<WlSurface>>,
    pub parent: Mutex<Option<WlSurface>>,
    pub role: Mutex<SurfaceRole>,
    pub viewport: Mutex<Option<crate::proto::fractional_scale::ViewportState>>,
}

#[derive(Default, Clone, PartialEq)]
pub enum SurfaceRole {
    #[default]
    None,
    XdgToplevel,
    XdgPopup,
    LayerSurface,
    Subsurface,
    Cursor,
    DnDIcon,
}

impl SurfaceData {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(PendingSurfaceState::default()),
            current: Mutex::new(CommittedSurfaceState {
                buffer: None,
                dx: 0,
                dy: 0,
                damage_buffer: Vec::new(),
                frame_callbacks: Vec::new(),
                input_region: None,
                opaque_region: None,
                buffer_scale: 1,
                buffer_transform: None,
                needs_upload: false,
            }),
            children: Mutex::new(Vec::new()),
            parent: Mutex::new(None),
            role: Mutex::new(SurfaceRole::None),
            viewport: Mutex::new(None),
        })
    }
}

// ── Region user-data ──────────────────────────────────────────────────────────

#[derive(Default, Clone)]
pub struct RegionData {
    pub rects: Vec<(i32, i32, i32, i32)>, // x, y, w, h (additive union)
}

impl RegionData {
    pub fn contains(&self, px: i32, py: i32) -> bool {
        self.rects
            .iter()
            .any(|&(x, y, w, h)| px >= x && px < x + w && py >= y && py < y + h)
    }
}

// ── wl_compositor global ──────────────────────────────────────────────────────

impl GlobalDispatch<WlCompositor, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WlCompositor>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WlCompositor, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                let surface_data = SurfaceData::new();
                data_init.init(id, surface_data);
            }
            wl_compositor::Request::CreateRegion { id } => {
                data_init.init(id, std::sync::Mutex::new(RegionData::default()));
            }
            _ => {}
        }
    }
}

// ── wl_surface dispatch ───────────────────────────────────────────────────────

impl Dispatch<WlSurface, Arc<SurfaceData>> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        surface: &WlSurface,
        request: wl_surface::Request,
        data: &Arc<SurfaceData>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_surface::Request::Attach { buffer, x, y } => {
                let mut pending = data.pending.lock().unwrap();
                pending.buffer = Some(buffer);
                pending.dx = x;
                pending.dy = y;
            }

            wl_surface::Request::Damage {
                x,
                y,
                width,
                height,
            } => {
                let mut pending = data.pending.lock().unwrap();
                pending.damage_surface.push(Rect {
                    x,
                    y,
                    w: width,
                    h: height,
                });
            }

            wl_surface::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                let mut pending = data.pending.lock().unwrap();
                pending.damage_buffer.push(Rect {
                    x,
                    y,
                    w: width,
                    h: height,
                });
            }

            wl_surface::Request::Frame { callback } => {
                let cb = data_init.init(callback, ());
                let mut pending = data.pending.lock().unwrap();
                pending.frame_callbacks.push(cb);
            }

            wl_surface::Request::SetInputRegion { region } => {
                let region_data = region.as_ref().and_then(|r| {
                    r.data::<std::sync::Mutex<RegionData>>()
                        .and_then(|m| m.lock().ok().map(|d| d.clone()))
                });
                data.pending.lock().unwrap().input_region = Some(region_data);
            }

            wl_surface::Request::SetOpaqueRegion { region } => {
                let region_data = region.as_ref().and_then(|r| {
                    r.data::<std::sync::Mutex<RegionData>>()
                        .and_then(|m| m.lock().ok().map(|d| d.clone()))
                });
                data.pending.lock().unwrap().opaque_region = Some(region_data);
            }

            wl_surface::Request::SetBufferScale { scale } => {
                data.pending.lock().unwrap().buffer_scale = Some(scale);
            }

            wl_surface::Request::SetBufferTransform { transform } => {
                if let Ok(t) = transform.into_result() {
                    data.pending.lock().unwrap().buffer_transform = Some(t);
                }
            }

            wl_surface::Request::Commit => {
                commit_surface(state, surface, data);
            }

            wl_surface::Request::Destroy => {
                destroy_surface(state, surface, data);
            }

            _ => {}
        }
    }
}

/// Apply pending → current (the commit transaction).
fn commit_surface(state: &mut Axiom, surface: &WlSurface, data: &Arc<SurfaceData>) {
    let mut pending = data.pending.lock().unwrap();
    let mut current = data.current.lock().unwrap();

    // Buffer
    if let Some(new_buffer) = pending.buffer.take() {
        // Release the old buffer back to the client.
        if let Some(old) = current.buffer.take() {
            old.release();
        }
        current.buffer = new_buffer;
        current.needs_upload = current.buffer.is_some();
    }

    // Offsets
    current.dx = pending.dx;
    current.dy = pending.dy;

    // Damage — accumulate into current
    current
        .damage_buffer
        .extend(pending.damage_buffer.drain(..));
    pending.damage_surface.clear();

    // Frame callbacks — hand off to current so the render loop fires them
    current
        .frame_callbacks
        .extend(pending.frame_callbacks.drain(..));

    // Regions
    if let Some(r) = pending.input_region.take() {
        current.input_region = r;
    }
    if let Some(r) = pending.opaque_region.take() {
        current.opaque_region = r;
    }

    // Scale / transform
    if let Some(s) = pending.buffer_scale.take() {
        current.buffer_scale = s;
    }
    if let Some(t) = pending.buffer_transform.take() {
        current.buffer_transform = Some(t);
    }

    drop(pending);
    drop(current);

    // Notify the WM that this surface has new content.
    state.on_surface_commit(surface);
}

fn destroy_surface(state: &mut Axiom, surface: &WlSurface, _data: &Arc<SurfaceData>) {
    state.on_surface_destroy(surface);
}

// ── wl_region dispatch ────────────────────────────────────────────────────────

impl Dispatch<WlRegion, std::sync::Mutex<RegionData>> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlRegion,
        request: wl_region::Request,
        data: &std::sync::Mutex<RegionData>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // RegionData is passed by reference; we need interior mutability.
        // Use the Arc<Mutex<>> pattern via a wrapper. For now treat as append-only.
        match request {
            wl_region::Request::Add {
                x,
                y,
                width,
                height,
            } => {
                if let Ok(mut d) = data.lock() {
                    d.rects.push((x, y, width, height));
                }
            }
            wl_region::Request::Subtract { .. } => {}
            wl_region::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── wl_callback dispatch ──────────────────────────────────────────────────────

impl Dispatch<WlCallback, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlCallback,
        _request: wayland_server::protocol::wl_callback::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // wl_callback has no requests; only the done event matters (sent by us).
    }
}

// ── wl_subcompositor global ───────────────────────────────────────────────────

impl GlobalDispatch<WlSubcompositor, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WlSubcompositor>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WlSubcompositor, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlSubcompositor,
        request: wl_subcompositor::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_subcompositor::Request::GetSubsurface {
                id,
                surface,
                parent,
            } => {
                // Record parent relationship on the child surface.
                if let Some(child_data) = surface.data::<Arc<SurfaceData>>() {
                    *child_data.parent.lock().unwrap() = Some(parent.clone());
                    *child_data.role.lock().unwrap() = SurfaceRole::Subsurface;
                }
                // Record child in parent's child list.
                if let Some(parent_data) = parent.data::<Arc<SurfaceData>>() {
                    parent_data.children.lock().unwrap().push(surface);
                }
                data_init.init(id, ());
            }
            wl_subcompositor::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── wl_subsurface dispatch ────────────────────────────────────────────────────

impl Dispatch<WlSubsurface, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlSubsurface,
        _request: wl_subsurface::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // set_position / place_above / place_below / set_sync handled here in
        // a full implementation. Stubs are sufficient to not crash clients.
    }
}
