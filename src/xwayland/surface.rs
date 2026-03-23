// src/xwayland/surface.rs — xwayland-shell-v1 protocol binding.
//
// XWayland surfaces are paired by serial: XWayland sends the surface serial
// via the WL_SURFACE_SERIAL X11 property, and the compositor matches it to
// the xwayland_surface_v1 object that was created for the same serial.

use std::sync::{Arc, Mutex};

use wayland_protocols::xwayland::shell::v1::server::{
    xwayland_shell_v1::{self, XwaylandShellV1},
    xwayland_surface_v1::{self, XwaylandSurfaceV1},
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};

use crate::state::Axiom;

// ── Per-surface pairing data ──────────────────────────────────────────────────

#[derive(Clone)]
pub struct XwaylandSurface {
    pub surface: WlSurface,
    pub serial: Option<u64>,
    /// X11 window ID, set once we receive WL_SURFACE_SERIAL from X side.
    pub x11_win: Option<u32>,
}

// ── xwayland_shell_v1 global ──────────────────────────────────────────────────

impl GlobalDispatch<XwaylandShellV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<XwaylandShellV1>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<XwaylandShellV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &XwaylandShellV1,
        request: xwayland_shell_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xwayland_shell_v1::Request::GetXwaylandSurface { id, surface } => {
                let xws = XwaylandSurface {
                    surface: surface.clone(),
                    serial: None,
                    x11_win: None,
                };
                init.init(id, Arc::new(Mutex::new(xws.clone())));
                // Register as pending until X11 sends the serial.
                state.xwayland.pending_surfaces.push(xws);
            }
            xwayland_shell_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<XwaylandSurfaceV1, Arc<Mutex<XwaylandSurface>>> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &XwaylandSurfaceV1,
        request: xwayland_surface_v1::Request,
        data: &Arc<Mutex<XwaylandSurface>>,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xwayland_surface_v1::Request::SetSerial {
                serial_lo,
                serial_hi,
            } => {
                let serial = (serial_hi as u64) << 32 | serial_lo as u64;
                let mut xws = data.lock().unwrap();
                xws.serial = Some(serial);
                let surf = xws.surface.clone();
                drop(xws);

                // Try to pair with an already-known X11 window that sent this serial.
                state.try_pair_xwayland_surface(&surf, serial);
            }
            xwayland_surface_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
