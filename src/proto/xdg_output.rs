// src/proto/xdg_output.rs — zxdg-output-manager-v1 (xdg-output-unstable-v1)
//
// Exposes logical output geometry to clients (waybar, eww, swww, etc).
// We report a 1:1 logical/physical mapping (scale=1) for now; fractional
// scaling can be wired later via wp-fractional-scale-v1.

use wayland_protocols::xdg::xdg_output::zv1::server::{
    zxdg_output_manager_v1::{self, ZxdgOutputManagerV1},
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use crate::state::Axiom;

// ── Per-output logical info sent to clients ───────────────────────────────────

pub struct XdgOutputData {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub name: String,
}

// ── Global ────────────────────────────────────────────────────────────────────

impl GlobalDispatch<ZxdgOutputManagerV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgOutputManagerV1>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ZxdgOutputManagerV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZxdgOutputManagerV1,
        request: zxdg_output_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zxdg_output_manager_v1::Request::GetXdgOutput { id, output } => {
                // Find matching OutputState by wl_output id.
                let out_id = output.id().protocol_id();
                let (x, y, w, h, name) = state
                    .outputs
                    .iter()
                    .find(|o| o.wl_id == out_id)
                    .map(|o| (0i32, 0i32, o.width as i32, o.height as i32, o.name.clone()))
                    .unwrap_or((0, 0, 1920, 1080, "output-0".into()));

                let xdg_out = init.init(
                    id,
                    XdgOutputData {
                        x,
                        y,
                        width: w,
                        height: h,
                        name: name.clone(),
                    },
                );

                // Send the full description and done.
                xdg_out.logical_position(x, y);
                xdg_out.logical_size(w, h);
                xdg_out.name(name);
                xdg_out.done();
            }
            zxdg_output_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── Per-output ────────────────────────────────────────────────────────────────

impl Dispatch<ZxdgOutputV1, XdgOutputData> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZxdgOutputV1,
        request: zxdg_output_v1::Request,
        _data: &XdgOutputData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zxdg_output_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
