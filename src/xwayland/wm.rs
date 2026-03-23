// src/xwayland/wm.rs — X11 window manager state for XWayland integration.
//
// All core WM types (Rect, Window, WmState, etc.) live in crate::wm.
// This file only adds X11-specific logic on top.

pub use crate::wm::anim;
pub use crate::wm::layout;
pub use crate::wm::rules;

pub use crate::wm::{Layout, Rect, Window, WindowId, WmConfig, WmState};
pub use rules::{Effect, WindowRule};

use std::collections::HashMap;
use std::sync::Arc;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureNotifyEvent, ConfigureRequestEvent, ConnectionExt,
    DestroyNotifyEvent, EventMask, InputFocus, MapRequestEvent, PropMode, PropertyNotifyEvent,
    UnmapNotifyEvent,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperExt;
use x11rb::CURRENT_TIME;

use crate::xwayland::atoms::Atoms;
use crate::xwayland::X11Action;

// ── Window type ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum WindowType {
    Normal,
    Dialog,
    Utility,
    Toolbar,
    Menu,
    Splash,
    Dock,
    Desktop,
    Notification,
    Unknown,
}

// ── Size hints ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SizeHints {
    pub min_width: Option<i32>,
    pub min_height: Option<i32>,
    pub max_width: Option<i32>,
    pub max_height: Option<i32>,
    pub base_width: Option<i32>,
    pub base_height: Option<i32>,
    pub width_inc: Option<i32>,
    pub height_inc: Option<i32>,
}

// XSizeHints flags.
const P_MIN_SIZE: u32 = 1 << 4;
const P_MAX_SIZE: u32 = 1 << 5;
const P_RESIZE_INC: u32 = 1 << 6;
const P_BASE_SIZE: u32 = 1 << 8;

// _MOTIF_WM_HINTS flags.
const MWM_HINTS_DECORATIONS: u32 = 1 << 1;

// ── X11WmState ────────────────────────────────────────────────────────────────

pub struct X11WmState {
    pub conn: Arc<RustConnection>,
    pub atoms: Atoms,
    pub root: u32,
    pub wm_win: u32,
    pub screen_w: i32,
    pub screen_h: i32,
    /// Map of X11 window → last known title.
    pub titles: HashMap<u32, String>,
    /// Map of X11 window → app_id (WM_CLASS instance name).
    pub app_ids: HashMap<u32, String>,
    /// Map of X11 window → (window type, size hints, wants_decorations).
    pub window_hints: HashMap<u32, (WindowType, SizeHints, bool)>,
}

impl X11WmState {
    pub fn new(
        conn: Arc<RustConnection>,
        atoms: Atoms,
        root: u32,
        wm_win: u32,
        screen_w: i32,
        screen_h: i32,
    ) -> Self {
        Self {
            conn,
            atoms,
            root,
            wm_win,
            screen_w,
            screen_h,
            titles: HashMap::new(),
            app_ids: HashMap::new(),
            window_hints: HashMap::new(),
        }
    }

    // ── Event dispatch ────────────────────────────────────────────────────────

    pub fn handle_event(
        &mut self,
        conn: &RustConnection,
        event: x11rb::protocol::Event,
    ) -> Option<X11Action> {
        use x11rb::protocol::Event::*;
        match event {
            MapRequest(e) => self.on_map_request(conn, e),
            UnmapNotify(e) => self.on_unmap_notify(e),
            DestroyNotify(e) => Some(X11Action::UnmapWindow { x11_win: e.window }),
            ConfigureRequest(e) => self.on_configure_request(e),
            PropertyNotify(e) => self.on_property_notify(conn, e),
            ClientMessage(_) => None,
            _ => None,
        }
    }

    fn on_map_request(&mut self, conn: &RustConnection, e: MapRequestEvent) -> Option<X11Action> {
        let win = e.window;

        // Skip override-redirect windows — they manage themselves and must not
        // be paired with a Wayland toplevel (menus, tooltips, etc.).
        let override_redirect = conn
            .get_window_attributes(win)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|a| a.override_redirect)
            .unwrap_or(false);

        if override_redirect {
            // Still map it so it appears on screen, but don't emit a
            // MapWindow action that would trigger pairing in state.rs.
            let _ = conn.map_window(win);
            let _ = conn.flush();
            tracing::debug!(
                "xwayland map_request: win={win} override_redirect=true — skipping pair"
            );
            return None;
        }

        // WM_CLASS: use instance name (first field) for rule matching.
        let app_id = self.read_wm_class(conn, win).unwrap_or_default();
        self.app_ids.insert(win, app_id.clone());

        let title = self
            .read_net_wm_name(conn, win)
            .or_else(|| self.read_wm_name(conn, win))
            .unwrap_or_else(|| app_id.clone());
        self.titles.insert(win, title.clone());

        // Read and stash EWMH hints for use in window rules.
        let wtype = self.read_window_type(conn, win);
        let hints = self.read_size_hints(conn, win);
        let decorated = self.wants_decorations(conn, win);
        self.window_hints.insert(win, (wtype, hints, decorated));

        // Read WL_SURFACE_SERIAL so state.rs can pair without a second round-trip.
        let surface_serial = self.read_wl_surface_serial(conn, win);

        tracing::debug!(
            "xwayland map_request: win={win} app_id={app_id:?} title={title:?} serial={surface_serial:?}"
        );

        let _ = conn.map_window(win);
        let _ = conn.flush();

        Some(X11Action::MapWindow {
            x11_win: win,
            title,
            app_id,
            override_redirect,
            surface_serial,
        })
    }

    fn on_unmap_notify(&mut self, e: UnmapNotifyEvent) -> Option<X11Action> {
        Some(X11Action::UnmapWindow { x11_win: e.window })
    }

    fn on_configure_request(&self, e: ConfigureRequestEvent) -> Option<X11Action> {
        use x11rb::protocol::xproto::ConfigWindow;
        Some(X11Action::ConfigureRequest {
            x11_win: e.window,
            x: e.value_mask.contains(ConfigWindow::X).then_some(e.x as i32),
            y: e.value_mask.contains(ConfigWindow::Y).then_some(e.y as i32),
            w: e.value_mask
                .contains(ConfigWindow::WIDTH)
                .then_some(e.width as u32),
            h: e.value_mask
                .contains(ConfigWindow::HEIGHT)
                .then_some(e.height as u32),
        })
    }

    fn on_property_notify(
        &mut self,
        conn: &RustConnection,
        e: PropertyNotifyEvent,
    ) -> Option<X11Action> {
        let win = e.window;
        if e.atom == self.atoms._NET_WM_NAME || e.atom == self.atoms.WM_NAME {
            let title = self
                .read_net_wm_name(conn, win)
                .or_else(|| self.read_wm_name(conn, win))
                .unwrap_or_default();
            self.titles.insert(win, title.clone());
            return Some(X11Action::TitleChanged {
                x11_win: win,
                title,
            });
        }
        None
    }

    // ── Property readers ──────────────────────────────────────────────────────

    /// Returns the WM_CLASS instance name (first null-separated field).
    fn read_wm_class(&self, conn: &RustConnection, win: u32) -> Option<String> {
        let reply = conn
            .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        let bytes = reply.value;
        // Take the first non-empty field (instance name); fall back to class.
        let mut fields = bytes.split(|&b| b == 0).filter(|s| !s.is_empty());
        let instance = fields.next()?;
        Some(String::from_utf8_lossy(instance).into_owned())
    }

    fn read_net_wm_name(&self, conn: &RustConnection, win: u32) -> Option<String> {
        let reply = conn
            .get_property(
                false,
                win,
                self.atoms._NET_WM_NAME,
                self.atoms.UTF8_STRING,
                0,
                512,
            )
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&reply.value).into_owned())
    }

    fn read_wm_name(&self, conn: &RustConnection, win: u32) -> Option<String> {
        let reply = conn
            .get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 512)
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&reply.value).into_owned())
    }

    fn read_wl_surface_serial(&self, conn: &RustConnection, win: u32) -> Option<u64> {
        let reply = conn
            .get_property(
                false,
                win,
                self.atoms.WL_SURFACE_SERIAL,
                x11rb::protocol::xproto::AtomEnum::CARDINAL,
                0,
                2,
            )
            .ok()?
            .reply()
            .ok()?;

        if reply.value.len() < 8 {
            return None;
        }
        let lo = u32::from_ne_bytes(reply.value[0..4].try_into().unwrap()) as u64;
        let hi = u32::from_ne_bytes(reply.value[4..8].try_into().unwrap()) as u64;
        Some((hi << 32) | lo)
    }

    // ── EWMH property readers (Step 4) ────────────────────────────────────────

    /// Returns the set of _NET_WM_STATE atoms active on this window.
    pub fn read_net_wm_state(&self, conn: &RustConnection, win: u32) -> Vec<u32> {
        conn.get_property(false, win, self.atoms._NET_WM_STATE, AtomEnum::ATOM, 0, 32)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.value32().map(|iter| iter.collect()).unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn is_fullscreen(&self, conn: &RustConnection, win: u32) -> bool {
        let fs: u32 = self.atoms._NET_WM_STATE_FULLSCREEN.into();
        self.read_net_wm_state(conn, win).contains(&fs)
    }

    pub fn is_maximized(&self, conn: &RustConnection, win: u32) -> bool {
        let states = self.read_net_wm_state(conn, win);
        let vert: u32 = self.atoms._NET_WM_STATE_MAXIMIZED_VERT.into();
        let horz: u32 = self.atoms._NET_WM_STATE_MAXIMIZED_HORZ.into();
        states.contains(&vert) && states.contains(&horz)
    }

    /// Detect the window type for rule matching / float decisions.
    pub fn read_window_type(&self, conn: &RustConnection, win: u32) -> WindowType {
        let atoms_list = conn
            .get_property(
                false,
                win,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                0,
                32,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|i| i.collect::<Vec<u32>>()));

        let Some(list) = atoms_list else {
            return WindowType::Normal;
        };
        let first = match list.first() {
            Some(&a) => a,
            None => return WindowType::Normal,
        };

        let a = &self.atoms;
        match first {
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_DIALOG) => WindowType::Dialog,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_UTILITY) => WindowType::Utility,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_TOOLBAR) => WindowType::Toolbar,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_MENU) => WindowType::Menu,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_SPLASH) => WindowType::Splash,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_DND) => WindowType::Dock,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_DESKTOP) => WindowType::Desktop,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_NOTIFICATION) => WindowType::Notification,
            x if x == u32::from(a._NET_WM_WINDOW_TYPE_NORMAL) => WindowType::Normal,
            _ => WindowType::Unknown,
        }
    }

    /// Read WM_NORMAL_HINTS (XSizeHints) for size constraints.
    pub fn read_size_hints(&self, conn: &RustConnection, win: u32) -> SizeHints {
        let reply = conn
            .get_property(
                false,
                win,
                AtomEnum::WM_NORMAL_HINTS,
                AtomEnum::WM_SIZE_HINTS,
                0,
                18, // XSizeHints is 18 longs
            )
            .ok()
            .and_then(|c| c.reply().ok());

        let Some(reply) = reply else {
            return SizeHints::default();
        };
        let vals: Vec<i32> = reply
            .value32()
            .map(|i| i.map(|v| v as i32).collect())
            .unwrap_or_default();

        if vals.len() < 18 {
            return SizeHints::default();
        }

        let flags = vals[0] as u32;
        let mut h = SizeHints::default();

        if flags & P_MIN_SIZE != 0 {
            h.min_width = Some(vals[5]);
            h.min_height = Some(vals[6]);
        }
        if flags & P_MAX_SIZE != 0 {
            h.max_width = Some(vals[7]);
            h.max_height = Some(vals[8]);
        }
        if flags & P_RESIZE_INC != 0 {
            h.width_inc = Some(vals[9]);
            h.height_inc = Some(vals[10]);
        }
        if flags & P_BASE_SIZE != 0 {
            h.base_width = Some(vals[15]);
            h.base_height = Some(vals[16]);
        }
        h
    }

    /// Read _MOTIF_WM_HINTS to detect whether this window wants decorations.
    pub fn read_motif_hints(&self, conn: &RustConnection, win: u32) -> Option<u32> {
        let reply = conn
            .get_property(
                false,
                win,
                self.atoms._MOTIF_WM_HINTS,
                self.atoms._MOTIF_WM_HINTS,
                0,
                5,
            )
            .ok()?
            .reply()
            .ok()?;

        let vals: Vec<u32> = reply.value32()?.collect();
        if vals.len() < 3 {
            return None;
        }
        let flags = vals[0];
        let decorations = vals[2];
        if flags & MWM_HINTS_DECORATIONS != 0 {
            Some(decorations)
        } else {
            None
        }
    }

    /// Returns true if the window should receive server-side decorations.
    pub fn wants_decorations(&self, conn: &RustConnection, win: u32) -> bool {
        match self.read_motif_hints(conn, win) {
            Some(d) => d != 0,
            None => true, // no hints = default to decorated
        }
    }
}
