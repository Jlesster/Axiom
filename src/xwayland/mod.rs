// src/xwayland/mod.rs — XWayland integration.

pub mod atoms;
pub mod surface;
pub mod wm;

use std::{
    collections::HashMap,
    os::unix::{
        io::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        process::CommandExt,
    },
    process::{Child, Command},
    sync::Arc,
};

use anyhow::{Context, Result};
use x11rb::{
    connection::Connection,
    protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigWindow, ConfigureWindowAux,
        ConnectionExt, CreateWindowAux, EventMask, InputFocus, PropMode, WindowClass,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as WrapperExt,
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME,
};

pub use self::atoms::Atoms;
use crate::wm::WindowId;
pub use surface::XwaylandSurface;
pub use wm::X11WmState;

// ── Actions emitted from X11 event handling ───────────────────────────────────

#[derive(Debug)]
pub enum X11Action {
    MapWindow {
        x11_win: u32,
        title: String,
        app_id: String,
        override_redirect: bool,
    },
    UnmapWindow {
        x11_win: u32,
    },
    ConfigureRequest {
        x11_win: u32,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
    },
    TitleChanged {
        x11_win: u32,
        title: String,
    },
    FocusRequest {
        x11_win: u32,
    },
}

// ── XWayland manager ──────────────────────────────────────────────────────────

pub struct XWaylandState {
    pub child: Option<Child>,
    pub conn: Option<Arc<RustConnection>>,
    pub display: Option<u32>,
    pub wm: Option<X11WmState>,
    pub x11_to_wl: HashMap<u32, WindowId>,
    pub wl_to_x11: HashMap<WindowId, u32>,
    pub pending_surfaces: Vec<XwaylandSurface>,
}

impl XWaylandState {
    pub fn new() -> Self {
        Self {
            child: None,
            conn: None,
            display: None,
            wm: None,
            x11_to_wl: HashMap::new(),
            wl_to_x11: HashMap::new(),
            pending_surfaces: Vec::new(),
        }
    }

    pub fn is_xwayland_window(&self, id: WindowId) -> bool {
        self.wl_to_x11.contains_key(&id)
    }

    pub fn start(&mut self, wayland_display: &str, screen_w: i32, screen_h: i32) -> Result<()> {
        let (x_fd, xw_fd) = create_socket_pair()?;
        let (display_r, display_w) = nix::unistd::pipe().context("pipe for displayfd")?;

        let x_raw = x_fd.as_raw_fd();
        let xw_raw = xw_fd.as_raw_fd();
        let dw_raw = display_w.as_raw_fd();

        let child = unsafe {
            Command::new("Xwayland")
                .args([
                    "-rootless",
                    "-listenfd",
                    &x_raw.to_string(),
                    "-displayfd",
                    &dw_raw.to_string(),
                    "-wm",
                    &xw_raw.to_string(),
                ])
                .env("WAYLAND_DISPLAY", wayland_display)
                .pre_exec(move || {
                    clear_cloexec(x_raw);
                    clear_cloexec(xw_raw);
                    clear_cloexec(dw_raw);
                    Ok(())
                })
                .spawn()
                .context("spawn Xwayland")?
        };

        self.child = Some(child);
        drop(display_w);

        let display_num = read_display_number(display_r.as_raw_fd())?;
        self.display = Some(display_num);
        tracing::info!("XWayland on :{display_num}");
        unsafe {
            std::env::set_var("DISPLAY", format!(":{display_num}"));
        }

        let (conn, screen_num) = RustConnection::connect(Some(&format!(":{display_num}")))
            .context("x11rb connect to XWayland")?;
        let conn = Arc::new(conn);

        let atoms = atoms::Atoms::new(&conn)?.reply()?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        // Become the WM.
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::SUBSTRUCTURE_NOTIFY
                    | EventMask::SUBSTRUCTURE_REDIRECT
                    | EventMask::PROPERTY_CHANGE
                    | EventMask::FOCUS_CHANGE,
            ),
        )?
        .check()
        .context("SubstructureRedirect — is another WM running?")?;

        // WM support window.
        let wm_win = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            wm_win,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new(),
        )?
        .check()?;

        conn.change_property32(
            PropMode::REPLACE,
            root,
            atoms._NET_SUPPORTING_WM_CHECK,
            AtomEnum::WINDOW,
            &[wm_win],
        )?
        .check()?;
        conn.change_property32(
            PropMode::REPLACE,
            wm_win,
            atoms._NET_SUPPORTING_WM_CHECK,
            AtomEnum::WINDOW,
            &[wm_win],
        )?
        .check()?;
        conn.change_property8(
            PropMode::REPLACE,
            wm_win,
            atoms._NET_WM_NAME,
            atoms.UTF8_STRING,
            b"axiom",
        )?
        .check()?;

        let supported: Vec<u32> = vec![
            atoms._NET_WM_NAME.into(),
            atoms._NET_WM_STATE.into(),
            atoms._NET_WM_STATE_FULLSCREEN.into(),
            atoms._NET_WM_STATE_MAXIMIZED_VERT.into(),
            atoms._NET_WM_STATE_MAXIMIZED_HORZ.into(),
            atoms._NET_WM_STATE_HIDDEN.into(),
            atoms._NET_WM_MOVERESIZE.into(),
            atoms._NET_ACTIVE_WINDOW.into(),
            atoms._NET_SUPPORTING_WM_CHECK.into(),
            atoms._NET_CLIENT_LIST.into(),
        ];
        conn.change_property32(
            PropMode::REPLACE,
            root,
            atoms._NET_SUPPORTED,
            AtomEnum::ATOM,
            &supported,
        )?
        .check()?;

        // Root cursor (best-effort).
        let font = conn.generate_id()?;
        let _ = conn.open_font(font, b"cursor");
        let cursor = conn.generate_id()?;
        let _ = conn.create_glyph_cursor(
            cursor,
            font,
            font,
            68,
            69,
            0,
            0,
            0,
            u16::MAX,
            u16::MAX,
            u16::MAX,
        );
        let _ =
            conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().cursor(cursor));

        conn.flush()?;

        self.wm = Some(X11WmState::new(
            conn.clone(),
            atoms,
            root,
            wm_win,
            screen_w,
            screen_h,
        ));
        self.conn = Some(conn);
        Ok(())
    }

    pub fn dispatch_events(&mut self) -> Vec<X11Action> {
        let conn = match self.conn.as_ref() {
            Some(c) => c.clone(),
            None => return vec![],
        };
        let wm: &mut X11WmState = match self.wm.as_mut() {
            Some(w) => w,
            None => return vec![],
        };
        let mut actions = Vec::new();
        loop {
            match conn.poll_for_event() {
                Ok(Some(ev)) => {
                    if let Some(a) = wm.handle_event(&conn, ev) {
                        actions.push(a);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("x11: {e}");
                    break;
                }
            }
        }
        let _ = conn.flush();
        actions
    }

    pub fn set_focus(&self, x11_win: u32) {
        if let (Some(conn), Some(wm)) = (self.conn.as_ref(), self.wm.as_ref()) {
            let conn: &RustConnection = conn;
            let _ = conn.set_input_focus(InputFocus::POINTER_ROOT, x11_win, CURRENT_TIME);
            let _ = conn.change_property32(
                PropMode::REPLACE,
                wm.root,
                wm.atoms._NET_ACTIVE_WINDOW,
                AtomEnum::WINDOW,
                &[x11_win],
            );
            let _ = conn.flush();
        }
    }

    pub fn close_window(&self, x11_win: u32) {
        if let (Some(conn), Some(wm)) = (self.conn.as_ref(), self.wm.as_ref()) {
            let conn: &RustConnection = conn;
            let event = ClientMessageEvent::new(
                32,
                x11_win,
                wm.atoms.WM_PROTOCOLS,
                [wm.atoms.WM_DELETE_WINDOW.into(), CURRENT_TIME, 0, 0, 0],
            );
            let _ = conn.send_event(false, x11_win, EventMask::NO_EVENT, event);
            let _ = conn.flush();
        }
    }

    pub fn configure_window(&self, x11_win: u32, x: i32, y: i32, w: u32, h: u32) {
        if let Some(conn) = self.conn.as_ref() {
            let _ = conn.configure_window(
                x11_win,
                &ConfigureWindowAux::new().x(x).y(y).width(w).height(h),
            );
            let _ = conn.flush();
        }
    }

    /// Return the raw fd of the X11 connection for calloop registration.
    pub fn x11_fd(&self) -> Option<RawFd> {
        self.conn.as_ref().map(|c| {
            use x11rb::connection::Connection;
            c.stream().as_raw_fd()
        })
    }
}

// ── Socket / pipe helpers ─────────────────────────────────────────────────────

fn create_socket_pair() -> Result<(OwnedFd, OwnedFd)> {
    use nix::sys::socket::*;
    let (a, b) = socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::SOCK_CLOEXEC,
    )?;
    Ok((a, b))
}

fn read_display_number(fd: RawFd) -> Result<u32> {
    use std::io::Read;
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    buf.trim()
        .parse::<u32>()
        .context("parse display number from Xwayland")
}

unsafe fn clear_cloexec(fd: RawFd) {
    use nix::fcntl::*;
    let flags = fcntl(fd, FcntlArg::F_GETFD).unwrap_or(0);
    let _ = fcntl(
        fd,
        FcntlArg::F_SETFD(FdFlag::from_bits_truncate(flags & !1)),
    );
}
