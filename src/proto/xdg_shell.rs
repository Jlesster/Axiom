// src/proto/xdg_shell.rs — xdg_wm_base / xdg_surface / xdg_toplevel / xdg_popup.
//
// The critical path:
//   1. Client binds xdg_wm_base.
//   2. Client calls get_xdg_surface(wl_surface) → xdg_surface.
//   3. Client calls xdg_surface.get_toplevel() → xdg_toplevel.
//   4. Client commits the wl_surface → we get a configure round-trip going.
//   5. Client acks the configure, commits again → window is live.
//
// We map each xdg_toplevel directly to a WM Window; the WM assigns geometry
// on the first configure and sends xdg_toplevel.configure + xdg_surface.configure.

use std::sync::{Arc, Mutex};

use wayland_protocols::xdg::shell::server::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch,
    New, Resource,
};

use crate::state::Axiom;
use crate::wm::WindowId;

// ── xdg_wm_base global ────────────────────────────────────────────────────────

impl GlobalDispatch<XdgWmBase, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<XdgWmBase>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgWmBase, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &XdgWmBase,
        request: xdg_wm_base::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                let xdg_data = XdgSurfaceData {
                    wl_surface: surface,
                    configured: false,
                    configure_serial: 0,
                    role: XdgRole::None,
                };
                data_init.init(id, Arc::new(Mutex::new(xdg_data)));
            }

            xdg_wm_base::Request::CreatePositioner { id } => {
                data_init.init(id, std::sync::Mutex::new(PositionerData::default()));
            }

            xdg_wm_base::Request::Pong { serial } => {
                log::trace!("xdg_wm_base pong serial={}", serial);
                // Could track ping/pong for "not responding" detection.
            }

            xdg_wm_base::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── xdg_surface data ──────────────────────────────────────────────────────────

pub type XdgSurfaceDataRef = Arc<Mutex<XdgSurfaceData>>;

pub struct XdgSurfaceData {
    pub wl_surface: WlSurface,
    pub configured: bool,
    pub configure_serial: u32,
    pub role: XdgRole,
}

#[derive(Clone)]
pub enum XdgRole {
    None,
    Toplevel(XdgToplevel),
    Popup(XdgPopup),
}

// ── xdg_surface dispatch ──────────────────────────────────────────────────────

impl Dispatch<XdgSurface, XdgSurfaceDataRef> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        xdg_surface: &XdgSurface,
        request: xdg_surface::Request,
        data: &XdgSurfaceDataRef,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_surface::Request::GetToplevel { id } => {
                let toplevel_data = ToplevelData {
                    xdg_surface: xdg_surface.clone(),
                    xdg_data: Arc::clone(data),
                    window_id: None,
                    title: None,
                    app_id: None,
                    min_size: (0, 0),
                    max_size: (0, 0),
                    pending_states: Vec::new(),
                };
                let toplevel_ref = Arc::new(Mutex::new(toplevel_data));
                let toplevel = data_init.init(id, Arc::clone(&toplevel_ref));
                data.lock().unwrap().role = XdgRole::Toplevel(toplevel.clone());

                let wl_surface = data.lock().unwrap().wl_surface.clone();
                state.register_toplevel(wl_surface, toplevel_ref, Arc::clone(data));

                // Send an *initial* configure with size (0,0) and no states so the client
                // knows to proceed. We do NOT send a full WM configure yet — that happens
                // in on_surface_commit once the window has been added to the WM with a
                // real rect. Sending size 0,0 here tells the client "you choose your size"
                // which is correct for the initial negotiation.
                toplevel.configure(0, 0, vec![]);
                let serial = state.next_serial();
                xdg_surface.configure(serial);
                data.lock().unwrap().configure_serial = serial;
            }

            xdg_surface::Request::GetPopup {
                id,
                parent,
                positioner,
            } => {
                let pos_data = positioner
                    .data::<std::sync::Mutex<PositionerData>>()
                    .and_then(|m| m.lock().ok().map(|d| d.clone()))
                    .unwrap_or_default();
                let popup_data = PopupData {
                    xdg_surface: xdg_surface.clone(),
                    xdg_data: Arc::clone(data),
                    positioner: pos_data,
                    parent: parent.map(|p| p.data::<XdgSurfaceDataRef>().unwrap().clone()),
                };
                let popup = data_init.init(id, Arc::new(Mutex::new(popup_data)));
                data.lock().unwrap().role = XdgRole::Popup(popup);
            }

            xdg_surface::Request::SetWindowGeometry {
                x,
                y,
                width,
                height,
            } => {
                // Store the window geometry hint (inner sans shadows/CSD).
                let d = data.lock().unwrap();
                if let Some(win_id) = xdg_toplevel_window_id(&d) {
                    state.wm.set_window_geometry(win_id, x, y, width, height);
                }
            }

            xdg_surface::Request::AckConfigure { serial } => {
                let mut d = data.lock().unwrap();
                if serial == d.configure_serial {
                    d.configured = true;
                }
            }

            xdg_surface::Request::Destroy => {
                let d = data.lock().unwrap();
                if let Some(win_id) = xdg_toplevel_window_id(&d) {
                    state.wm.remove_window(win_id);
                }
            }

            _ => {}
        }
    }
}

fn xdg_toplevel_window_id(d: &XdgSurfaceData) -> Option<WindowId> {
    if let XdgRole::Toplevel(ref tl) = d.role {
        tl.data::<Arc<Mutex<ToplevelData>>>()
            .and_then(|td| td.lock().ok()?.window_id)
    } else {
        None
    }
}

// ── xdg_toplevel data + dispatch ──────────────────────────────────────────────

pub type ToplevelDataRef = Arc<Mutex<ToplevelData>>;

pub struct ToplevelData {
    pub xdg_surface: XdgSurface,
    pub xdg_data: XdgSurfaceDataRef,
    pub window_id: Option<WindowId>,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub min_size: (i32, i32),
    pub max_size: (i32, i32),
    pub pending_states: Vec<xdg_toplevel::State>,
}

impl Dispatch<XdgToplevel, ToplevelDataRef> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        toplevel: &XdgToplevel,
        request: xdg_toplevel::Request,
        data: &ToplevelDataRef,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel::Request::SetTitle { title } => {
                let mut d = data.lock().unwrap();
                d.title = Some(title.clone());
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.set_window_title(win_id, title);
                }
            }

            xdg_toplevel::Request::SetAppId { app_id } => {
                let mut d = data.lock().unwrap();
                d.app_id = Some(app_id.clone());
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.set_window_app_id(win_id, app_id);
                }
            }

            xdg_toplevel::Request::SetParent { parent } => {
                let parent_win =
                    parent.and_then(|p| p.data::<ToplevelDataRef>()?.lock().ok()?.window_id);
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.set_window_parent(win_id, parent_win);
                }
            }

            xdg_toplevel::Request::SetMinSize { width, height } => {
                data.lock().unwrap().min_size = (width, height);
            }

            xdg_toplevel::Request::SetMaxSize { width, height } => {
                data.lock().unwrap().max_size = (width, height);
            }

            xdg_toplevel::Request::Move { seat: _, serial: _ } => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.start_interactive_move(win_id);
                }
            }

            xdg_toplevel::Request::Resize {
                seat: _,
                serial: _,
                edges,
            } => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    if let Ok(edges) = edges.into_result() {
                        state.start_interactive_resize(win_id, edges as u32);
                    }
                }
            }

            xdg_toplevel::Request::SetMaximized => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.maximize_window(win_id, true);
                    send_configure_toplevel(state, toplevel, data);
                }
            }

            xdg_toplevel::Request::UnsetMaximized => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.maximize_window(win_id, false);
                    send_configure_toplevel(state, toplevel, data);
                }
            }

            xdg_toplevel::Request::SetFullscreen { output: _ } => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.fullscreen_window(win_id, true);
                    send_configure_toplevel(state, toplevel, data);
                }
            }

            xdg_toplevel::Request::UnsetFullscreen => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.fullscreen_window(win_id, false);
                    send_configure_toplevel(state, toplevel, data);
                }
            }

            xdg_toplevel::Request::SetMinimized => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.minimize_window(win_id);
                }
            }

            xdg_toplevel::Request::Destroy => {
                let d = data.lock().unwrap();
                if let Some(win_id) = d.window_id {
                    drop(d);
                    state.wm.remove_window(win_id);
                }
            }

            _ => {}
        }
    }
}

/// Send a configure event for a toplevel reflecting current WM state.
pub fn send_configure_toplevel(state: &mut Axiom, toplevel: &XdgToplevel, data: &ToplevelDataRef) {
    let d = data.lock().unwrap();
    let xdg_surface = d.xdg_surface.clone();
    let win_id = d.win_id_or_zero();
    drop(d);

    let (width, height, wl_states) = if win_id > 0 {
        let win = state.wm.window(win_id);
        let mut st = vec![];
        if win.maximized {
            st.push(xdg_toplevel::State::Maximized);
        }
        if win.fullscreen {
            st.push(xdg_toplevel::State::Fullscreen);
        }
        if state.wm.focused_window() == Some(win_id) {
            st.push(xdg_toplevel::State::Activated);
        }
        (win.rect.w, win.rect.h, st)
    } else {
        (0, 0, vec![])
    };

    // Encode states as a wl_array (Vec<u8> of little-endian u32).
    let states_bytes: Vec<u8> = wl_states
        .iter()
        .flat_map(|s| (*s as u32).to_ne_bytes())
        .collect();

    toplevel.configure(width, height, states_bytes);

    let serial = state.next_serial();
    xdg_surface.configure(serial);
    data.lock()
        .unwrap()
        .xdg_data
        .lock()
        .unwrap()
        .configure_serial = serial;
}

impl ToplevelData {
    fn win_id_or_zero(&self) -> WindowId {
        self.window_id.unwrap_or(0)
    }
}

// ── xdg_popup data + dispatch ─────────────────────────────────────────────────

pub type PopupDataRef = Arc<Mutex<PopupData>>;

pub struct PopupData {
    pub xdg_surface: XdgSurface,
    pub xdg_data: XdgSurfaceDataRef,
    pub positioner: PositionerData,
    pub parent: Option<XdgSurfaceDataRef>,
}

impl Dispatch<XdgPopup, PopupDataRef> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        popup: &XdgPopup,
        request: xdg_popup::Request,
        data: &PopupDataRef,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_popup::Request::Grab { seat: _, serial: _ } => {
                // Implement popup grab (keyboard + pointer exclusivity).
                state.input.set_popup_grab(popup.clone());
            }
            xdg_popup::Request::Reposition { positioner, token } => {
                let new_pos = positioner
                    .data::<std::sync::Mutex<PositionerData>>()
                    .and_then(|m| m.lock().ok().map(|d| d.clone()))
                    .unwrap_or_default();
                data.lock().unwrap().positioner = new_pos;
                popup.repositioned(token);
            }
            xdg_popup::Request::Destroy => {
                state.input.clear_popup_grab();
            }
            _ => {}
        }
    }
}

// ── xdg_positioner ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PositionerData {
    pub width: i32,
    pub height: i32,
    pub anchor_rect: (i32, i32, i32, i32),
    pub anchor: xdg_positioner::Anchor,
    pub gravity: xdg_positioner::Gravity,
    pub constraint_adjustment: u32,
    pub offset: (i32, i32),
    pub reactive: bool,
}

impl Default for PositionerData {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            anchor_rect: (0, 0, 0, 0),
            anchor: xdg_positioner::Anchor::None,
            gravity: xdg_positioner::Gravity::None,
            constraint_adjustment: 0,
            offset: (0, 0),
            reactive: false,
        }
    }
}

impl Dispatch<XdgPositioner, std::sync::Mutex<PositionerData>> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &XdgPositioner,
        request: xdg_positioner::Request,
        data: &std::sync::Mutex<PositionerData>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Ok(mut d) = data.lock() else { return };

        match request {
            xdg_positioner::Request::SetSize { width, height } => {
                d.width = width;
                d.height = height;
            }
            xdg_positioner::Request::SetAnchorRect {
                x,
                y,
                width,
                height,
            } => {
                d.anchor_rect = (x, y, width, height);
            }
            xdg_positioner::Request::SetAnchor { anchor } => {
                if let Ok(a) = anchor.into_result() {
                    d.anchor = a;
                }
            }
            xdg_positioner::Request::SetGravity { gravity } => {
                if let Ok(g) = gravity.into_result() {
                    d.gravity = g;
                }
            }
            xdg_positioner::Request::SetConstraintAdjustment {
                constraint_adjustment,
            } => {
                d.constraint_adjustment = constraint_adjustment;
            }
            xdg_positioner::Request::SetOffset { x, y } => {
                d.offset = (x, y);
            }
            xdg_positioner::Request::SetReactive => {
                d.reactive = true;
            }
            xdg_positioner::Request::Destroy => {}
            _ => {}
        }
    }
}
