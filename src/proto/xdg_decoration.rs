// src/proto/xdg_decoration.rs — zxdg-decoration-manager-v1
//
// We always respond with ServerSide so GTK4/Qt apps don't draw their own
// titlebars.  Axiom draws its own borders; clients draw nothing.

use wayland_protocols::xdg::decoration::zv1::server::{
    zxdg_decoration_manager_v1::{self, ZxdgDecorationManagerV1},
    zxdg_toplevel_decoration_v1::{self, Mode, ZxdgToplevelDecorationV1},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use crate::state::Axiom;

// ── Global ────────────────────────────────────────────────────────────────────

impl GlobalDispatch<ZxdgDecorationManagerV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgDecorationManagerV1>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ZxdgDecorationManagerV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZxdgDecorationManagerV1,
        request: zxdg_decoration_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zxdg_decoration_manager_v1::Request::GetToplevelDecoration { id, toplevel } => {
                let dec = init.init(id, ());
                // Immediately tell the client we'll handle decorations.
                dec.configure(Mode::ServerSide);
            }
            zxdg_decoration_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── Per-toplevel decoration ───────────────────────────────────────────────────

impl Dispatch<ZxdgToplevelDecorationV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZxdgToplevelDecorationV1,
        request: zxdg_toplevel_decoration_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // Client asked for a specific mode — we always override to server-side.
            zxdg_toplevel_decoration_v1::Request::SetMode { mode: _ } => {
                resource.configure(Mode::ServerSide);
            }
            zxdg_toplevel_decoration_v1::Request::UnsetMode => {
                resource.configure(Mode::ServerSide);
            }
            zxdg_toplevel_decoration_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
