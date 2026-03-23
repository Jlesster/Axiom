// src/proto/mod.rs — Wayland protocol global registration.

pub mod compositor;
pub mod dmabuf;
pub mod fractional_scale;
pub mod idle_inhibit;
pub mod layer_shell;
pub mod screencopy;
pub mod seat;
pub mod shm;
pub mod wl_output;
pub mod xdg_decoration;
pub mod xdg_output;
pub mod xdg_shell;

use wayland_protocols::wp::idle_inhibit::zv1::server::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1;
use wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::{
    fractional_scale::v1::server::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    viewporter::server::wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::{
    decoration::zv1::server::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    shell::server::xdg_wm_base::XdgWmBase,
    xdg_output::zv1::server::zxdg_output_manager_v1::ZxdgOutputManagerV1,
};
use wayland_protocols_wlr::{
    layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1,
    screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};
use wayland_server::{
    protocol::{
        wl_compositor::WlCompositor, wl_output::WlOutput, wl_seat::WlSeat, wl_shm::WlShm,
        wl_subcompositor::WlSubcompositor,
    },
    DisplayHandle,
};

pub fn register_globals(dh: &DisplayHandle) {
    dh.create_global::<crate::state::Axiom, WlCompositor, _>(6, ());
    dh.create_global::<crate::state::Axiom, WlSubcompositor, _>(1, ());
    dh.create_global::<crate::state::Axiom, WlShm, _>(1, ());
    dh.create_global::<crate::state::Axiom, WlOutput, _>(4, ());
    dh.create_global::<crate::state::Axiom, WlSeat, _>(7, ());
    dh.create_global::<crate::state::Axiom, XdgWmBase, _>(5, ());
    dh.create_global::<crate::state::Axiom, ZwlrLayerShellV1, _>(4, ());
    dh.create_global::<crate::state::Axiom, ZxdgDecorationManagerV1, _>(1, ());
    dh.create_global::<crate::state::Axiom, ZxdgOutputManagerV1, _>(3, ());
    // Version 3 only — v4 adds zwp_linux_dmabuf_feedback_v1 which we don't
    // implement. Advertising v4 causes clients (waybar, etc.) to request
    // feedback objects, and wayland-backend panics when the handler doesn't
    // call init.init() on the new object id.
    dh.create_global::<crate::state::Axiom, ZwpLinuxDmabufV1, _>(3, ());
    dh.create_global::<crate::state::Axiom, ZwlrScreencopyManagerV1, _>(3, ());
    dh.create_global::<crate::state::Axiom, WpFractionalScaleManagerV1, _>(1, ());
    dh.create_global::<crate::state::Axiom, WpViewporter, _>(1, ());
    dh.create_global::<crate::state::Axiom, ZwpIdleInhibitManagerV1, _>(1, ());
}
