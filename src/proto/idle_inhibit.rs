// src/proto/idle_inhibit.rs — zwp-idle-inhibit-v1
//
// When any client holds an active inhibitor we call `loginctl lock-session`
// suppression by notifying systemd-logind (or swayidle-compatible daemons)
// via a simple inhibitor count that external tools can query via IPC.
// The compositor itself inhibits screen blanking by informing the kernel
// DRM DPMS state (see render loop integration in state.rs).

use std::sync::{Arc, Mutex};

use wayland_protocols::wp::idle_inhibit::zv1::server::{
    zwp_idle_inhibit_manager_v1::{self, ZwpIdleInhibitManagerV1},
    zwp_idle_inhibitor_v1::{self, ZwpIdleInhibitorV1},
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};

use crate::state::Axiom;

// ── Global inhibitor count ────────────────────────────────────────────────────

/// Shared counter: number of live inhibitors.  Zero means idle is allowed.
#[derive(Default, Clone)]
pub struct IdleInhibitState {
    count: Arc<Mutex<u32>>,
}

impl IdleInhibitState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inhibited(&self) -> bool {
        *self.count.lock().unwrap() > 0
    }

    fn inc(&self) {
        *self.count.lock().unwrap() += 1;
    }

    fn dec(&self) {
        let mut g = self.count.lock().unwrap();
        *g = g.saturating_sub(1);
    }
}

// ── Per-inhibitor object data ─────────────────────────────────────────────────

pub struct InhibitorData {
    pub surface: WlSurface,
    state: IdleInhibitState,
}

// ── Global ────────────────────────────────────────────────────────────────────

impl GlobalDispatch<ZwpIdleInhibitManagerV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpIdleInhibitManagerV1>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpIdleInhibitManagerV1,
        request: zwp_idle_inhibit_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_idle_inhibit_manager_v1::Request::CreateInhibitor { id, surface } => {
                let inh_state = state.idle_inhibit.clone();
                inh_state.inc();
                tracing::debug!(
                    "idle inhibitor created (total={})",
                    *inh_state.count.lock().unwrap()
                );
                init.init(
                    id,
                    InhibitorData {
                        surface,
                        state: inh_state,
                    },
                );
            }
            zwp_idle_inhibit_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── Per-inhibitor ─────────────────────────────────────────────────────────────

impl Dispatch<ZwpIdleInhibitorV1, InhibitorData> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpIdleInhibitorV1,
        request: zwp_idle_inhibitor_v1::Request,
        _data: &InhibitorData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_idle_inhibitor_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &ZwpIdleInhibitorV1,
        data: &InhibitorData,
    ) {
        data.state.dec();
        tracing::debug!(
            "idle inhibitor destroyed (total={})",
            *data.state.count.lock().unwrap()
        );
    }
}
