// src/proto/output.rs — wl_output global.

use wayland_server::{
    protocol::wl_output::{self, Mode, Subpixel, Transform, WlOutput},
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
        data_init: &mut DataInit<'_, Self>,
    ) {
        let output = data_init.init(resource, ());

        let (w, h, refresh, scale, name) = state
            .outputs
            .first()
            .map(|o| {
                (
                    o.width as i32,
                    o.height as i32,
                    o.refresh_mhz as i32,
                    o.scale as i32,
                    o.name.clone(),
                )
            })
            .unwrap_or((1920, 1080, 60_000, 1, "output-0".to_string()));

        output.geometry(
            0,
            0, // x, y in compositor space
            0,
            0, // physical size mm (unknown)
            Subpixel::Unknown,
            "Unknown".to_string(),
            name.clone(),
            Transform::Normal,
        );

        output.mode(Mode::Current | Mode::Preferred, w, h, refresh);

        if output.version() >= 2 {
            output.scale(scale);
            output.done();
        }

        if output.version() >= 4 {
            output.name(name);
        }
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
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_output::Request::Release = request {}
    }
}
