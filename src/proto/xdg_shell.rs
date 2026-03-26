use crate::state::Axiom;
use crate::wm::WindowId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wayland_protocols::xdg::shell::server::{
    xdg_popup::{self, XdgPopup},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch,
    New, Resource,
};

// ── Serial counter ────────────────────────────────────────────────────────────

static SERIAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
fn next_serial() -> u32 {
    SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ── Per-toplevel data ─────────────────────────────────────────────────────────

pub struct ToplevelData {
    pub window_id: Option<WindowId>,
    pub title: Option<String>,
    pub app_id: Option<String>,
    /// Keep handles so we can send events back to the client.
    pub xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
    /// The underlying wl_surface for surface→window lookups.
    pub wl_surface: WlSurface,
    // Configure state machine
    pub configure_serial: u32,
    pub ever_acked: bool,
    pub mapped: bool, // true after first acked commit
}

pub type ToplevelDataRef = Arc<Mutex<ToplevelData>>;

// ── XdgSurface user-data ──────────────────────────────────────────────────────

pub struct XdgSurfaceData {
    /// The underlying wl_surface (set at GetXdgSurface time).
    pub wl_surface: Option<WlSurface>,
    pub toplevel: Option<ToplevelDataRef>,
    pub configure_serial: u32,
    pub ever_acked: bool,
}
impl Default for XdgSurfaceData {
    fn default() -> Self {
        Self {
            wl_surface: None,
            toplevel: None,
            configure_serial: 0,
            ever_acked: false,
        }
    }
}

pub type XdgSurfaceDataRef = Arc<Mutex<XdgSurfaceData>>;

// ── Global surface→toplevel map ───────────────────────────────────────────────
// Keyed by wl_surface protocol id so on_surface_commit can look up the
// toplevel for any surface in O(1) without scanning.

thread_local! {
    pub static SURFACE_MAP: std::cell::RefCell<HashMap<u32, ToplevelDataRef>> =
        std::cell::RefCell::new(HashMap::new());
}

fn register_surface(surface: &WlSurface, tl: ToplevelDataRef) {
    SURFACE_MAP.with(|m| m.borrow_mut().insert(surface.id().protocol_id(), tl));
}

fn unregister_surface(surface: &WlSurface) {
    SURFACE_MAP.with(|m| m.borrow_mut().remove(&surface.id().protocol_id()));
}

pub fn toplevel_for_surface(surface: &WlSurface) -> Option<ToplevelDataRef> {
    SURFACE_MAP.with(|m| m.borrow().get(&surface.id().protocol_id()).cloned())
}

// ── GlobalDispatch / Dispatch: XdgWmBase ─────────────────────────────────────

impl GlobalDispatch<XdgWmBase, ()> for Axiom {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<XdgWmBase>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        di.init(res, ());
    }
}

impl Dispatch<XdgWmBase, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &XdgWmBase,
        req: xdg_wm_base::Request,
        _: &(),
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                // Store the wl_surface in the xdg_surface data so GetToplevel
                // can register the surface→toplevel mapping.
                let data = Arc::new(Mutex::new(XdgSurfaceData {
                    wl_surface: Some(surface),
                    ..Default::default()
                }));
                di.init(id, data);
            }
            xdg_wm_base::Request::Pong { serial: _ } => {}
            xdg_wm_base::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── Dispatch: XdgSurface ─────────────────────────────────────────────────────

impl Dispatch<XdgSurface, XdgSurfaceDataRef> for Axiom {
    fn request(
        _state: &mut Self,
        _: &Client,
        xdg_surface: &XdgSurface,
        req: xdg_surface::Request,
        data: &XdgSurfaceDataRef,
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            xdg_surface::Request::GetToplevel { id } => {
                let tl_res = di.init(id, Arc::clone(data));

                let serial = next_serial();

                // Retrieve the wl_surface we stored at GetXdgSurface time.
                let wl_surface = data
                    .lock()
                    .unwrap()
                    .wl_surface
                    .clone()
                    .expect("XdgSurfaceData must have wl_surface set");

                let tl_data = Arc::new(Mutex::new(ToplevelData {
                    window_id: None,
                    title: None,
                    app_id: None,
                    xdg_surface: xdg_surface.clone(),
                    toplevel: tl_res.clone(),
                    wl_surface: wl_surface.clone(),
                    configure_serial: serial,
                    ever_acked: false,
                    mapped: false,
                }));

                {
                    let mut d = data.lock().unwrap();
                    d.toplevel = Some(Arc::clone(&tl_data));
                    d.configure_serial = serial;
                }

                // Register surface → toplevel mapping immediately so
                // on_surface_commit can find it on the first commit.
                register_surface(&wl_surface, Arc::clone(&tl_data));

                // Initial configure: size 0,0 → "client picks its own size".
                tl_res.configure(0, 0, vec![]);
                xdg_surface.configure(serial);
            }
            xdg_surface::Request::GetPopup {
                id,
                parent: _,
                positioner: _,
            } => {
                di.init(id, Arc::clone(data));
            }
            xdg_surface::Request::AckConfigure { serial } => {
                let mut d = data.lock().unwrap();
                if d.configure_serial == serial {
                    d.ever_acked = true;
                    if let Some(ref tl) = d.toplevel.clone() {
                        tl.lock().unwrap().ever_acked = true;
                    }
                }
            }
            xdg_surface::Request::SetWindowGeometry { .. } => {
                // Clients use this to declare their visible region (excluding
                // drop shadows, CSD decorations). We could honour this in the
                // renderer — for now we accept and ignore it since we use
                // server-side decorations and the WM-assigned rect.
            }
            xdg_surface::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _: wayland_server::backend::ClientId,
        _resource: &XdgSurface,
        data: &XdgSurfaceDataRef,
    ) {
        let d = data.lock().unwrap();
        // Unregister the surface mapping and close the window if it's still open.
        if let Some(ref tl) = d.toplevel {
            let tl = tl.lock().unwrap();
            unregister_surface(&tl.wl_surface);
            if let Some(id) = tl.window_id {
                let id = id;
                drop(tl);
                drop(d);
                state.close_window(id);
            }
        }
    }
}

// ── Dispatch: XdgToplevel ────────────────────────────────────────────────────

impl Dispatch<XdgToplevel, XdgSurfaceDataRef> for Axiom {
    fn request(
        state: &mut Self,
        _: &Client,
        _toplevel: &XdgToplevel,
        req: xdg_toplevel::Request,
        data: &XdgSurfaceDataRef,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            xdg_toplevel::Request::SetTitle { title } => {
                let window_id = {
                    let d = data.lock().unwrap();
                    d.toplevel.as_ref().and_then(|tl| {
                        let mut tl = tl.lock().unwrap();
                        tl.title = Some(title.clone());
                        tl.window_id
                    })
                };
                if let Some(id) = window_id {
                    state.wm.set_title(id, title);
                }
            }
            xdg_toplevel::Request::SetAppId { app_id } => {
                let window_id = {
                    let d = data.lock().unwrap();
                    d.toplevel.as_ref().and_then(|tl| {
                        let mut tl = tl.lock().unwrap();
                        tl.app_id = Some(app_id.clone());
                        tl.window_id
                    })
                };
                if let Some(id) = window_id {
                    state.wm.set_app_id(id, app_id);
                }
            }
            xdg_toplevel::Request::Destroy => {
                let window_id = data
                    .lock()
                    .unwrap()
                    .toplevel
                    .as_ref()
                    .and_then(|tl| tl.lock().unwrap().window_id);
                if let Some(id) = window_id {
                    state.close_window(id);
                }
            }
            xdg_toplevel::Request::SetMaximized => {
                // Treat maximize as "fill the workspace area" — effectively
                // the same as our default tiling layout does already.
                // Send a configure with the current WM rect so the client
                // knows its new size.
                let window_id = data
                    .lock()
                    .unwrap()
                    .toplevel
                    .as_ref()
                    .and_then(|tl| tl.lock().unwrap().window_id);
                if let Some(id) = window_id {
                    if let Some(w) = state.wm.windows.get_mut(&id) {
                        w.maximized = true;
                    }
                    crate::proto::configure_toplevel(state, id);
                    state.needs_redraw = true;
                }
            }
            xdg_toplevel::Request::UnsetMaximized => {
                let window_id = data
                    .lock()
                    .unwrap()
                    .toplevel
                    .as_ref()
                    .and_then(|tl| tl.lock().unwrap().window_id);
                if let Some(id) = window_id {
                    if let Some(w) = state.wm.windows.get_mut(&id) {
                        w.maximized = false;
                    }
                    crate::proto::configure_toplevel(state, id);
                    state.needs_redraw = true;
                }
            }
            xdg_toplevel::Request::SetFullscreen { output: _ } => {
                let window_id = data
                    .lock()
                    .unwrap()
                    .toplevel
                    .as_ref()
                    .and_then(|tl| tl.lock().unwrap().window_id);
                if let Some(id) = window_id {
                    state.wm.fullscreen_window(id, true);
                    crate::proto::configure_toplevel(state, id);
                    state.needs_redraw = true;
                }
            }
            xdg_toplevel::Request::UnsetFullscreen => {
                let window_id = data
                    .lock()
                    .unwrap()
                    .toplevel
                    .as_ref()
                    .and_then(|tl| tl.lock().unwrap().window_id);
                if let Some(id) = window_id {
                    state.wm.fullscreen_window(id, false);
                    crate::proto::configure_toplevel(state, id);
                    state.needs_redraw = true;
                }
            }
            xdg_toplevel::Request::Move { .. } => {
                // Interactive move: in a tiling WM this is a no-op unless the
                // window is floating. Future: start a drag for float windows.
            }
            xdg_toplevel::Request::Resize { .. } => {
                // Interactive resize: no-op for tiled windows.
                // Future: allow resize for floating windows.
            }
            xdg_toplevel::Request::SetMinimized => {
                // Minimise to workspace — send to an invisible workspace slot.
                // For now: move to last workspace and switch back.
                let window_id = data
                    .lock()
                    .unwrap()
                    .toplevel
                    .as_ref()
                    .and_then(|tl| tl.lock().unwrap().window_id);
                if let Some(id) = window_id {
                    let last = state.wm.workspaces.len().saturating_sub(1);
                    state.wm.move_to_workspace(id, last);
                    state.needs_redraw = true;
                }
            }
            xdg_toplevel::Request::SetMinSize { width, height } => {
                // Store min size on the window for future use in layouts.
                let window_id = data
                    .lock()
                    .unwrap()
                    .toplevel
                    .as_ref()
                    .and_then(|tl| tl.lock().unwrap().window_id);
                // Currently we don't have min_size in the Window struct —
                // we silently accept and ignore. A future commit can add
                // Window::min_w / min_h and honour them in compute_layout.
                let _ = (window_id, width, height);
            }
            xdg_toplevel::Request::SetMaxSize { width, height } => {
                let _ = (width, height);
            }
            xdg_toplevel::Request::SetParent { parent: _ } => {
                // Parent hint for dialog positioning — no-op for now.
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _: wayland_server::backend::ClientId,
        _resource: &XdgToplevel,
        data: &XdgSurfaceDataRef,
    ) {
        let window_id = data
            .lock()
            .unwrap()
            .toplevel
            .as_ref()
            .and_then(|tl| tl.lock().unwrap().window_id);
        if let Some(id) = window_id {
            state.close_window(id);
        }
    }
}

// ── Dispatch: XdgPopup ───────────────────────────────────────────────────────

impl Dispatch<XdgPopup, XdgSurfaceDataRef> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        popup: &XdgPopup,
        req: xdg_popup::Request,
        _: &XdgSurfaceDataRef,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            xdg_popup::Request::Grab { .. } => {
                // Popup grab: the client wants exclusive input. We acknowledge
                // by sending a configure immediately.
                popup.configure(0, 0, 1, 1); // x, y, width, height
                                             // TODO: implement actual grab/dismiss logic for context menus.
            }
            xdg_popup::Request::Reposition {
                positioner: _,
                token,
            } => {
                popup.repositioned(token);
            }
            xdg_popup::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── Surface commit handler ────────────────────────────────────────────────────

/// Called from compositor.rs after every wl_surface.commit.
pub fn on_surface_commit(state: &mut Axiom, surface: &WlSurface) {
    let tl_ref = match toplevel_for_surface(surface) {
        Some(t) => t,
        None => return,
    };

    let (ever_acked, window_id) = {
        let tl = tl_ref.lock().unwrap();
        (tl.ever_acked, tl.window_id)
    };

    if !ever_acked {
        // Client is still in the initial configure handshake.
        return;
    }

    if window_id.is_none() {
        // First commit after ack — introduce this surface to the WM.
        let id = state.wm.add_window();

        {
            let mut tl = tl_ref.lock().unwrap();
            tl.window_id = Some(id);
            tl.mapped = true;

            // Apply any title/app_id already received before the first commit.
            if let Some(ref t) = tl.title.clone() {
                state.wm.set_title(id, t.clone());
            }
            if let Some(ref a) = tl.app_id.clone() {
                state.wm.set_app_id(id, a.clone());
            }
        }

        // Focus the new window and fire Lua signals.
        state.wm.focus_window(id);
        state.sync_keyboard_focus();
        state.script.emit_client_open(&state.wm, id);

        // Send a second configure with the WM-assigned size so the client
        // can resize its content to fit the tile.
        send_configure(state, id, &tl_ref);
        state.needs_redraw = true;
    } else {
        // Subsequent commits — content has been updated, schedule a redraw.
        state.needs_redraw = true;
        // Re-configure if the WM layout changed since the last commit
        // (e.g. a new window arrived and tiles were reflowed). The client
        // will see the new size on next ack+commit cycle.
    }
}

// ── Public helpers ────────────────────────────────────────────────────────────

/// Send xdg_toplevel.close to the client owning `id`.
pub fn close_toplevel(_dh: &DisplayHandle, id: WindowId) {
    SURFACE_MAP.with(|m| {
        for tl_ref in m.borrow().values() {
            let tl = tl_ref.lock().unwrap();
            if tl.window_id == Some(id) {
                tl.toplevel.close();
                return;
            }
        }
    });
}

/// Send a size configure to the toplevel for `id` based on current WM rect.
pub fn configure_toplevel(state: &mut Axiom, id: WindowId) {
    let tl_ref = SURFACE_MAP.with(|m| {
        m.borrow()
            .values()
            .find(|t| t.lock().unwrap().window_id == Some(id))
            .cloned()
    });
    if let Some(tl_ref) = tl_ref {
        send_configure(state, id, &tl_ref);
    }
}

/// Send configure to all mapped toplevels (e.g. after a layout change).
pub fn configure_all(state: &mut Axiom) {
    let entries: Vec<(WindowId, ToplevelDataRef)> = SURFACE_MAP.with(|m| {
        m.borrow()
            .values()
            .filter_map(|t| {
                let id = t.lock().unwrap().window_id?;
                Some((id, Arc::clone(t)))
            })
            .collect()
    });
    for (id, tl_ref) in entries {
        send_configure(state, id, &tl_ref);
    }
}

// ── Internal ──────────────────────────────────────────────────────────────────

fn send_configure(state: &mut Axiom, id: WindowId, tl_ref: &ToplevelDataRef) {
    use wayland_protocols::xdg::shell::server::xdg_toplevel::State as TlState;

    let rect = state
        .wm
        .windows
        .get(&id)
        .map(|w| w.rect)
        .unwrap_or_default();
    let win = state.wm.windows.get(&id);
    let is_fs = win.map(|w| w.fullscreen).unwrap_or(false);
    let is_max = win.map(|w| w.maximized).unwrap_or(false);
    let is_focused = state.wm.focused_window() == Some(id);
    let is_floating = win.map(|w| w.floating).unwrap_or(false);

    let mut states: Vec<u8> = Vec::with_capacity(32);

    if is_focused {
        push_state(&mut states, TlState::Activated);
    }
    if is_fs {
        push_state(&mut states, TlState::Fullscreen);
    }
    if is_max {
        push_state(&mut states, TlState::Maximized);
    }

    // Send tiled hints for non-floating tiled windows so clients suppress
    // drop-shadows and CSD rounding. This matches what Hyprland does.
    if !is_floating && !is_fs {
        push_state(&mut states, TlState::TiledLeft);
        push_state(&mut states, TlState::TiledRight);
        push_state(&mut states, TlState::TiledTop);
        push_state(&mut states, TlState::TiledBottom);
    }

    let serial = next_serial();
    let mut tl = tl_ref.lock().unwrap();
    tl.configure_serial = serial;
    tl.toplevel.configure(rect.w, rect.h, states);
    tl.xdg_surface.configure(serial);
}

/// Push a TlState value as its u32 native-endian bytes into the states vec.
fn push_state(buf: &mut Vec<u8>, s: wayland_protocols::xdg::shell::server::xdg_toplevel::State) {
    let v = s as u32;
    buf.extend_from_slice(&v.to_ne_bytes());
}
