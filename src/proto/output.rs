use crate::state::Axiom;
use wayland_server::{
    protocol::wl_output::{self, Mode as OutputMode, Subpixel, Transform, WlOutput},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

impl GlobalDispatch<WlOutput, ()> for Axiom {
    fn bind(
        state: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<WlOutput>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let output = di.init(res, ());
        let (w, h) = state.backend.output_size();
        output.geometry(
            0,
            0,
            0,
            0,
            Subpixel::Unknown,
            "Axiom".to_string(),
            "Output0".to_string(),
            Transform::Normal,
        );
        output.mode(
            OutputMode::Current | OutputMode::Preferred,
            w as i32,
            h as i32,
            60000,
        );
        output.scale(1);
        output.done();
    }
}

impl Dispatch<WlOutput, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlOutput,
        req: wl_output::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_output::Request::Release => {}
            _ => {}
        }
    }
}
