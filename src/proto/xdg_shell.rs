// src/proto/xdg_shell.rs — xdg_wm_base / xdg_surface / xdg_toplevel / xdg_popup.
//
// Configure round-trip (correct order):
//
//   1. GetToplevel → send configure(0,0,[]) + xdg_surface.configure(serial).
//      Size 0,0 = "you choose" before WM has assigned a rect.
//
//   2. Client commits wl_surface for the first time (on_surface_commit in
//      state.rs) → wm.add_window() + reflow() assigns a real rect →
//      send configure(w,h,tiled_states) + xdg_surface.configure(serial).
//      configured is set false, ever_acked is still false.
//
//   3. Client calls AckConfigure(serial). configured = true, ever_acked = true.
//
//   4. Client commits content at (w,h). upload_surface_texture() stores texture.
//
//   After step 3 ever_acked stays true forever. Subsequent configures
//   (focus change, resize, etc.) do NOT reset configured back to false,
//   so a race between a re-configure and a commit can never permanently
//   prevent texture upload.

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
                    ever_acked: false,
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
            }
            xdg_wm_base::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── xdg_surface data ──────────────────────────────────────────────────────────

pub type XdgSurfaceDataRef = Arc<Mutex<XdgSurfaceData>>;

#[derive(Clone)]
pub struct XdgSurfaceData {
    pub wl_surface: WlSurface,
    /// True once the client has acked at least one configure from us.
    /// Once true, never reset to false — subsequent configures do not
    /// re-gate texture upload on a new ack.
    pub configured: bool,
    /// Latched true on first AckConfigure. Guards the reset logic in
    /// send_configure_for_surface so we only gate upload before the first ack.
    pub ever_acked: bool,
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

                // Initial configure: size 0,0 = client picks. Real size comes
                // after wm.add_window() / reflow() in on_surface_commit.
                toplevel.configure(0, 0, vec![]);
                let serial = state.next_serial();
                xdg_surface.configure(serial);
                {
                    let mut d = data.lock().unwrap();
                    d.configure_serial = serial;
                    d.configured = false;
                    d.ever_acked = false;
                }
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
                // xdg_surface.set_window_geometry describes the *visible content
                // bounds* within the client's buffer — it is NOT a request for the
                // compositor to resize the window.  Passing it to set_window_geometry
                // overwrites win.rect with the client's self-reported inset dims,
                // which corrupts the compositor's tiling rect and causes subsequent
                // configure messages to send the wrong (shrinking) size.
                //
                // For tiled windows we ignore the hint entirely — the compositor
                // owns the rect.  For floating windows the client *does* choose its
                // own size, so we store it as a decoration hint so the renderer can
                // clip to the visible region; but we still do NOT let it overwrite
                // win.rect.w / win.rect.h because those are set by the user's resize
                // grab, not by the client.
                //
                // If you later need to honour shadow/CSD insets for floating windows,
                // add a dedicated `window_geometry: Option<Rect>` field to Window and
                // store (x, y, width, height) there for the renderer to use as the
                // visible-content clip rect.  Do not route it through set_window_geometry.
                let _ = (x, y, width, height); // acknowledged but intentionally unused
            }

            xdg_surface::Request::AckConfigure { serial } => {
                let mut d = data.lock().unwrap();
                // Accept acks for the current serial OR any older serial —
                // the spec says the client must ack the *latest* before
                // committing, but we treat any ack as "client is alive and
                // has seen at least one configure from us".
                if serial == d.configure_serial || !d.ever_acked {
                    d.configured = true;
                    d.ever_acked = true;
                }
                // Do NOT call reflow() or send another configure here —
                // that would create a configure storm.
            }

            xdg_surface::Request::Destroy => {
                let d = data.lock().unwrap();
                if let Some(win_id) = xdg_toplevel_window_id(&d) {
                    drop(d);
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

// ── send_configure_toplevel ───────────────────────────────────────────────────
//
// Used only by the xdg_toplevel request handlers (maximize, fullscreen, etc.)
// that originate from the client side. The compositor-initiated path goes
// through state::send_configure_for_surface instead.
//
// Both paths produce identical wire bytes — this one uses the typed enum and
// then encodes to bytes; send_configure_for_surface builds the bytes directly.
// They must stay in sync.
pub fn send_configure_toplevel(state: &mut Axiom, toplevel: &XdgToplevel, data: &ToplevelDataRef) {
    let d = data.lock().unwrap();
    let xdg_surface = d.xdg_surface.clone();
    let win_id = d.win_id_or_zero();
    drop(d);

    let (width, height, wl_states) = if win_id > 0 {
        let win = state.wm.window(win_id);
        let focused = state.wm.focused_window() == Some(win_id);
        let tiled = !win.floating && !win.fullscreen;

        let mut st = vec![];
        if win.maximized {
            st.push(xdg_toplevel::State::Maximized);
        }
        if win.fullscreen {
            st.push(xdg_toplevel::State::Fullscreen);
        }
        if focused {
            st.push(xdg_toplevel::State::Activated);
        }
        if tiled {
            st.push(xdg_toplevel::State::TiledLeft);
            st.push(xdg_toplevel::State::TiledRight);
            st.push(xdg_toplevel::State::TiledTop);
            st.push(xdg_toplevel::State::TiledBottom);
        }

        (win.rect.w, win.rect.h, st)
    } else {
        (0, 0, vec![])
    };

    let states_bytes: Vec<u8> = wl_states
        .iter()
        .flat_map(|s| (*s as u32).to_ne_bytes())
        .collect();

    toplevel.configure(width, height, states_bytes);
    let serial = state.next_serial();
    xdg_surface.configure(serial);

    {
        let mut xdg = data.lock().unwrap().xdg_data.lock().unwrap().clone();
        // Only gate on re-ack before the client has ever acked us.
        // After ever_acked is true, leave configured = true.
        drop(xdg);
    }
    // Update serial; preserve ever_acked / configured state correctly.
    let xdg_data = data.lock().unwrap().xdg_data.clone();
    let mut xdg = xdg_data.lock().unwrap();
    xdg.configure_serial = serial;
    if !xdg.ever_acked {
        xdg.configured = false;
    }
    // If ever_acked is true, configured stays true — no re-gating.
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
