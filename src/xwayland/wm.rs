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

// ── X11WmState ────────────────────────────────────────────────────────────────
//
// Holds the X11-specific fields needed to act as an ICCCM/EWMH window manager
// for XWayland windows.  The Wayland-side WM state lives in crate::wm::WmState.

pub struct X11WmState {
    pub conn: Arc<RustConnection>,
    pub atoms: Atoms,
    pub root: u32,
    pub wm_win: u32,
    pub screen_w: i32,
    pub screen_h: i32,
    /// Map of X11 window → last known title.
    titles: HashMap<u32, String>,
    /// Map of X11 window → app_id (WM_CLASS resource name).
    app_ids: HashMap<u32, String>,
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
        // Read WM_CLASS for app_id.
        let app_id = self.read_wm_class(conn, win).unwrap_or_default();
        self.app_ids.insert(win, app_id.clone());

        // Read _NET_WM_NAME / WM_NAME for title.
        let title = self
            .read_net_wm_name(conn, win)
            .or_else(|| self.read_wm_name(conn, win))
            .unwrap_or_else(|| app_id.clone());
        self.titles.insert(win, title.clone());

        // Check override_redirect.
        let override_redirect = conn
            .get_window_attributes(win)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|a| a.override_redirect)
            .unwrap_or(false);

        let _ = conn.map_window(win);
        let _ = conn.flush();

        Some(X11Action::MapWindow {
            x11_win: win,
            title,
            app_id,
            override_redirect,
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

    fn read_wm_class(&self, conn: &RustConnection, win: u32) -> Option<String> {
        let reply = conn
            .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        let bytes = reply.value;
        // WM_CLASS is two null-separated strings; take the first.
        let first = bytes.split(|&b| b == 0).next()?;
        Some(String::from_utf8_lossy(first).into_owned())
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
}
