use crate::state::Axiom;
use wayland_protocols_wlr::layer_shell::v1::server::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

impl GlobalDispatch<ZwlrLayerShellV1, ()> for Axiom {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<ZwlrLayerShellV1>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        di.init(res, ());
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrLayerShellV1,
        req: zwlr_layer_shell_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            zwlr_layer_shell_v1::Request::GetLayerSurface {
                id,
                surface: _,
                output: _,
                layer: _,
                namespace: _,
            } => {
                let ls = di.init(id, ());
                ls.configure(1, 1920, 24);
            }
            zwlr_layer_shell_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrLayerSurfaceV1,
        req: zwlr_layer_surface_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            zwlr_layer_surface_v1::Request::AckConfigure { serial: _ } => {}
            zwlr_layer_surface_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
