mod compositor;
mod decorations;
mod layer_shell;
mod output;
mod seat;
mod shm;
mod xdg_shell;

pub use compositor::handle_surface_commit;
pub use seat::{
    clear_keyboard_focus, send_key_event, send_keyboard_focus, send_keyboard_leave,
    send_pointer_axis, send_pointer_button, send_pointer_enter, send_pointer_leave,
    send_pointer_motion,
};
pub use xdg_shell::{close_toplevel, configure_all, configure_toplevel};

use wayland_protocols::xdg::decoration::zv1::server::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
use wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_server::DisplayHandle;

/// Register all compositor globals with the Wayland display.
pub fn register_globals(dh: &DisplayHandle) {
    use wayland_server::protocol::{
        wl_compositor::WlCompositor, wl_output::WlOutput, wl_seat::WlSeat, wl_shm::WlShm,
        wl_subcompositor::WlSubcompositor,
    };

    dh.create_global::<crate::state::Axiom, WlCompositor, ()>(6, ());
    dh.create_global::<crate::state::Axiom, WlSubcompositor, ()>(1, ());
    dh.create_global::<crate::state::Axiom, WlSeat, ()>(7, ());
    dh.create_global::<crate::state::Axiom, WlShm, ()>(1, ());
    dh.create_global::<crate::state::Axiom, WlOutput, ()>(4, ());
    dh.create_global::<crate::state::Axiom, XdgWmBase, ()>(3, ());
    dh.create_global::<crate::state::Axiom, ZxdgDecorationManagerV1, ()>(1, ());
    dh.create_global::<crate::state::Axiom, ZwlrLayerShellV1, ()>(4, ());
}
