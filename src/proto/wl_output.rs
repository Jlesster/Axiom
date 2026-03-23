// src/proto/wl_output.rs — wl_output global.
//
// Advertises each physical output to clients.  The geometry and mode events
// tell clients the screen size; done() marks the end of the description.

use wayland_server::{
    protocol::wl_output::{self, Subpixel, Transform, WlOutput},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::Axiom;

impl GlobalDispatch<WlOutput, ()> for Axiom {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WlOutput>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let output = init.init(resource, ());

        // Send geometry + mode for the primary output.
        // A full implementation would match the wl_output object to a specific
        // monitor; for now we describe monitor 0.
        let (w, h) = state
            .outputs
            .first()
            .map(|o| (o.width as i32, o.height as i32))
            .unwrap_or((1920, 1080));

        output.geometry(
            0,
            0, // x, y (logical position)
            0,
            0, // physical size mm (0 = unknown)
            Subpixel::Unknown,
            "Unknown".into(),
            "Unknown".into(),
            Transform::Normal,
        );
        output.mode(
            wl_output::Mode::Current | wl_output::Mode::Preferred,
            w,
            h,
            60_000, // refresh mHz
        );
        output.scale(1);
        output.done();
    }
}

impl Dispatch<WlOutput, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlOutput,
        request: wl_output::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_output::Request::Release => {}
            _ => {}
        }
    }
}
