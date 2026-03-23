// src/proto/layer_shell.rs — wlr-layer-shell-v1 implementation.

use std::sync::{Arc, Mutex};

use wayland_protocols_wlr::layer_shell::v1::server::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch,
    New, Resource, WEnum,
};

use crate::state::Axiom;

// ── Per-surface data ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Background = 0,
    Bottom = 1,
    Top = 2,
    Overlay = 3,
}

impl Layer {
    fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Background,
            1 => Self::Bottom,
            3 => Self::Overlay,
            _ => Self::Top,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayerSurfaceState {
    pub layer: Layer,
    pub anchor: Anchor,
    pub margin: [i32; 4], // top, right, bottom, left
    pub size: (u32, u32),
    pub exclusive: i32,
    pub keyboard: KeyboardInteractivity,
    pub output: Option<u32>,
    pub mapped: bool,
    pub configured: bool,
}

impl Default for LayerSurfaceState {
    fn default() -> Self {
        Self {
            layer: Layer::Top,
            anchor: Anchor::empty(),
            margin: [0; 4],
            size: (0, 0),
            exclusive: 0,
            keyboard: KeyboardInteractivity::None,
            output: None,
            mapped: false,
            configured: false,
        }
    }
}

pub type LayerSurfaceRef = Arc<Mutex<LayerSurfaceState>>;

// ── Global dispatch ───────────────────────────────────────────────────────────

impl GlobalDispatch<ZwlrLayerShellV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrLayerShellV1>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrLayerShellV1,
        request: zwlr_layer_shell_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_layer_shell_v1::Request::GetLayerSurface {
                id,
                surface,
                output,
                layer,
                namespace: _,
            } => {
                let layer_val = match layer {
                    WEnum::Value(v) => Layer::from_raw(v as u32),
                    WEnum::Unknown(v) => Layer::from_raw(v),
                };
                let data: LayerSurfaceRef = Arc::new(Mutex::new(LayerSurfaceState {
                    layer: layer_val,
                    output: output.as_ref().map(|o| o.id().protocol_id()),
                    ..Default::default()
                }));
                let ls = init.init(id, data.clone());
                ls.configure(state.next_serial(), 0, 0);
                data.lock().unwrap().configured = true;
                state.register_layer_surface(surface, ls, data);
            }
            zwlr_layer_shell_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── Per-surface dispatch ──────────────────────────────────────────────────────

impl Dispatch<ZwlrLayerSurfaceV1, LayerSurfaceRef> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrLayerSurfaceV1,
        request: zwlr_layer_surface_v1::Request,
        data: &LayerSurfaceRef,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        let mut ls = data.lock().unwrap();
        match request {
            zwlr_layer_surface_v1::Request::SetSize { width, height } => {
                ls.size = (width, height);
            }
            zwlr_layer_surface_v1::Request::SetAnchor { anchor } => {
                // anchor arrives as WEnum<Anchor> — unwrap to the bitflags value.
                ls.anchor = match anchor {
                    WEnum::Value(a) => a,
                    WEnum::Unknown(_) => Anchor::empty(),
                };
            }
            zwlr_layer_surface_v1::Request::SetExclusiveZone { zone } => {
                ls.exclusive = zone;
            }
            zwlr_layer_surface_v1::Request::SetMargin {
                top,
                right,
                bottom,
                left,
            } => {
                ls.margin = [top, right, bottom, left];
            }
            zwlr_layer_surface_v1::Request::SetKeyboardInteractivity {
                keyboard_interactivity,
            } => {
                ls.keyboard = match keyboard_interactivity {
                    WEnum::Value(k) => k,
                    WEnum::Unknown(_) => KeyboardInteractivity::None,
                };
            }
            zwlr_layer_surface_v1::Request::GetPopup { popup: _ } => {}
            zwlr_layer_surface_v1::Request::AckConfigure { serial: _ } => {
                ls.mapped = true;
            }
            zwlr_layer_surface_v1::Request::Destroy => {
                drop(ls);
                state.update_usable_area();
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &ZwlrLayerSurfaceV1,
        _data: &LayerSurfaceRef,
    ) {
        state.update_usable_area();
    }
}

// ── Usable area computation ───────────────────────────────────────────────────

pub fn compute_usable_area(
    output_w: i32,
    output_h: i32,
    surfaces: &[(LayerSurfaceRef, WlSurface)],
) -> crate::wm::Rect {
    let mut top = 0i32;
    let mut bottom = 0i32;
    let mut left = 0i32;
    let mut right = 0i32;

    for (ls_ref, _surf) in surfaces {
        let ls = ls_ref.lock().unwrap();
        if !ls.mapped || ls.exclusive <= 0 {
            continue;
        }
        let ex = ls.exclusive;
        let a = ls.anchor;

        let at = a.contains(Anchor::Top);
        let ab = a.contains(Anchor::Bottom);
        let al = a.contains(Anchor::Left);
        let ar = a.contains(Anchor::Right);

        match (at, ab, al, ar) {
            (true, false, false, false) => top = top.max(ex),
            (false, true, false, false) => bottom = bottom.max(ex),
            (false, false, true, false) => left = left.max(ex),
            (false, false, false, true) => right = right.max(ex),
            (true, false, true, true) => top = top.max(ex),
            (false, true, true, true) => bottom = bottom.max(ex),
            (true, true, true, false) => left = left.max(ex),
            (true, true, false, true) => right = right.max(ex),
            _ => {}
        }
    }

    crate::wm::Rect::new(
        left,
        top,
        (output_w - left - right).max(1),
        (output_h - top - bottom).max(1),
    )
}
