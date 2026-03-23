// src/proto/fractional_scale.rs — wp-fractional-scale-v1 + wp-viewporter-v1
//
// Fractional scaling flow:
//   1. Client binds wp_fractional_scale_manager_v1.
//   2. Client calls get_fractional_scale(wl_surface) → wp_fractional_scale_v1.
//   3. We send preferred_scale(120) for 1.25×, (144) for 1.5×, (192) for 2×
//      (denominator = 120, so 120/120=1.0, 144/120=1.2, etc.).
//   4. Client renders at the logical size × scale and attaches a buffer scaled
//      by the fractional amount.
//   5. Client uses wp_viewport to set the destination size so we composite
//      at the right logical pixel size.
//
// wp-viewporter:
//   Allows clients to set a source crop rectangle and destination size
//   independent of buffer dimensions.  Required for correct fractional scaling
//   and also used by video players (MPV, etc.).

use std::sync::{Arc, Mutex};

use wayland_protocols::wp::{
    fractional_scale::v1::server::{
        wp_fractional_scale_manager_v1::{self, WpFractionalScaleManagerV1},
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::server::{
        wp_viewport::{self, WpViewport},
        wp_viewporter::{self, WpViewporter},
    },
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch,
    New, Resource,
};

use crate::{proto::compositor::SurfaceData, state::Axiom};

// ── Viewport data (attached to WpViewport) ────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ViewportState {
    /// Source crop rectangle in buffer pixels (None = use full buffer).
    pub src: Option<[f64; 4]>, // x, y, w, h
    /// Destination size in surface-local (logical) pixels.
    pub dst: Option<(i32, i32)>,
}

// ── Fractional scale data (one per surface) ───────────────────────────────────

pub struct FractionalScaleData {
    pub surface: WlSurface,
}

// ─────────────────────────────────────────────────────────────────────────────
// wp_fractional_scale_manager_v1
// ─────────────────────────────────────────────────────────────────────────────

impl GlobalDispatch<WpFractionalScaleManagerV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpFractionalScaleManagerV1>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpFractionalScaleManagerV1,
        request: wp_fractional_scale_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_fractional_scale_manager_v1::Request::GetFractionalScale { id, surface } => {
                let obj = init.init(
                    id,
                    FractionalScaleData {
                        surface: surface.clone(),
                    },
                );
                // Send the current preferred scale for the output the surface is on.
                // scale_120ths: denominator 120. 1.0 = 120, 1.25 = 150, 1.5 = 180, 2.0 = 240.
                let scale_120ths = state.preferred_scale_120ths(&surface);
                obj.preferred_scale(scale_120ths);
            }
            wp_fractional_scale_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleV1, FractionalScaleData> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpFractionalScaleV1,
        request: wp_fractional_scale_v1::Request,
        _data: &FractionalScaleData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_fractional_scale_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// wp_viewporter
// ─────────────────────────────────────────────────────────────────────────────

impl GlobalDispatch<WpViewporter, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpViewporter>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<WpViewporter, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpViewporter,
        request: wp_viewporter::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_viewporter::Request::GetViewport { id, surface } => {
                // Attach ViewportState to the surface via the SurfaceData extension map.
                if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
                    *sd.viewport.lock().unwrap() = Some(ViewportState::default());
                }
                init.init(
                    id,
                    Arc::new(Mutex::new((ViewportState::default(), surface))),
                );
            }
            wp_viewporter::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WpViewport, Arc<Mutex<(ViewportState, WlSurface)>>> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpViewport,
        request: wp_viewport::Request,
        data: &Arc<Mutex<(ViewportState, WlSurface)>>,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        let mut g = data.lock().unwrap();
        let (vp, surf) = &mut *g;
        match request {
            wp_viewport::Request::SetSource {
                x,
                y,
                width,
                height,
            } => {
                if x == -1.0 && y == -1.0 && width == -1.0 && height == -1.0 {
                    vp.src = None;
                } else {
                    vp.src = Some([x, y, width, height]);
                }
                // Propagate to SurfaceData.
                if let Some(sd) = surf.data::<Arc<SurfaceData>>() {
                    if let Some(ref mut svp) = *sd.viewport.lock().unwrap() {
                        svp.src = vp.src;
                    }
                }
            }
            wp_viewport::Request::SetDestination { width, height } => {
                if width == -1 && height == -1 {
                    vp.dst = None;
                } else {
                    vp.dst = Some((width, height));
                }
                if let Some(sd) = surf.data::<Arc<SurfaceData>>() {
                    if let Some(ref mut svp) = *sd.viewport.lock().unwrap() {
                        svp.dst = vp.dst;
                    }
                }
            }
            wp_viewport::Request::Destroy => {
                if let Some(sd) = surf.data::<Arc<SurfaceData>>() {
                    *sd.viewport.lock().unwrap() = None;
                }
            }
            _ => {}
        }
    }
}

// ── Scale helper (called from Dispatch impl above + state.rs) ─────────────────

impl Axiom {
    /// Return the preferred fractional scale (denominator 120) for the output
    /// a surface is most likely on.  Falls back to the primary output.
    pub fn preferred_scale_120ths(&self, _surface: &WlSurface) -> u32 {
        let scale = self.outputs.first().map(|o| o.scale).unwrap_or(1.0);
        // Round to nearest 1/120.
        (scale * 120.0).round() as u32
    }

    /// Notify all fractional-scale objects attached to surfaces on `output_id`
    /// that the preferred scale has changed.  Called when an output's scale
    /// is updated (e.g. from IPC `set-scale` or config reload).
    pub fn notify_fractional_scale_changed(&self, _output_id: u32) {
        // Iterating all surfaces and finding attached WpFractionalScaleV1 objects
        // requires a registry we don't maintain yet.  For now this is a stub —
        // clients that bind fractional scale before a scale change will get the
        // new scale on their next bind.  A full implementation would iterate
        // self.toplevel_map and self.layer_surfaces, look up the
        // WpFractionalScaleV1 stored in SurfaceData, and call preferred_scale().
        tracing::debug!("notify_fractional_scale_changed (stub)");
    }
}
