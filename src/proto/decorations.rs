use crate::state::Axiom;
use wayland_protocols::xdg::decoration::zv1::server::{
    zxdg_decoration_manager_v1::{self, ZxdgDecorationManagerV1},
    zxdg_toplevel_decoration_v1::{self, Mode, ZxdgToplevelDecorationV1},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New};

impl GlobalDispatch<ZxdgDecorationManagerV1, ()> for Axiom {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<ZxdgDecorationManagerV1>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        di.init(res, ());
    }
}

impl Dispatch<ZxdgDecorationManagerV1, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZxdgDecorationManagerV1,
        req: zxdg_decoration_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            zxdg_decoration_manager_v1::Request::GetToplevelDecoration { id, toplevel: _ } => {
                let deco = di.init(id, ());
                deco.configure(Mode::ServerSide);
            }
            zxdg_decoration_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        deco: &ZxdgToplevelDecorationV1,
        req: zxdg_toplevel_decoration_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            zxdg_toplevel_decoration_v1::Request::SetMode { mode: _ } => {
                deco.configure(Mode::ServerSide);
            }
            zxdg_toplevel_decoration_v1::Request::UnsetMode => {
                deco.configure(Mode::ServerSide);
            }
            zxdg_toplevel_decoration_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
