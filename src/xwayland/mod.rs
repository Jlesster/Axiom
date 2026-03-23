// src/xwayland/mod.rs — XWayland integration.

pub mod atoms;
pub mod surface;
pub mod wm;

use nix::fcntl::{fcntl, FcntlArg, FdFlag};
use std::{
    collections::HashMap,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    process::{Child, Command},
    sync::Arc,
};

use anyhow::{Context, Result};
use wayland_server::Resource;
use x11rb::{
    connection::Connection,
    protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux, ConnectionExt,
        CreateWindowAux, EventMask, InputFocus, PropMode, WindowClass,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as WrapperExt,
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME,
};

pub use surface::XwaylandSurface;
pub use wm::X11WmState;

use crate::wm::WindowId;

// ── Actions emitted from X11 event handling ───────────────────────────────────

#[derive(Debug)]
pub enum X11Action {
    MapWindow {
        x11_win: u32,
        title: String,
        app_id: String,
        override_redirect: bool,
        surface_serial: Option<u64>,
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
    /// X11 window → surface serial (set by XWayland via WL_SURFACE_SERIAL).
    pub x11_serials: HashMap<u32, u64>,
    /// Set to true once finish_start() succeeds — main loop uses this to
    /// register the X11 fd with calloop.
    pub ready: bool,
    /// Screen dimensions saved by spawn() for use in finish_start().
    pub pending_screen: Option<(i32, i32)>,
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
            x11_serials: HashMap::new(),
            ready: false,
            pending_screen: None,
        }
    }

    pub fn is_xwayland_window(&self, id: WindowId) -> bool {
        self.wl_to_x11.contains_key(&id)
    }

    /// Phase 1: spawn the Xwayland process and return the display-number pipe.
    /// The caller must register the returned fd with calloop and call
    /// finish_start() when it becomes readable.
    pub fn spawn(
        &mut self,
        wayland_display: &str,
        screen_w: i32,
        screen_h: i32,
    ) -> Result<OwnedFd> {
        let (x_fd, xw_fd) = create_socket_pair()?;
        let (display_r, display_w) = nix::unistd::pipe().context("pipe for displayfd")?;

        let x_raw = x_fd.as_raw_fd();
        let xw_raw = xw_fd.as_raw_fd();
        let dw_raw = display_w.as_raw_fd();

        let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;

        tracing::info!(
            "spawning Xwayland: WAYLAND_DISPLAY={:?}, XDG_RUNTIME_DIR=Some({:?}), socket exists={}",
            wayland_display,
            xdg_runtime,
            std::path::Path::new(&format!("{}/{}", xdg_runtime, wayland_display)).exists()
        );

        // Kill only a stale Xwayland that was previously spawned by this
        // Axiom instance — identified by its WAYLAND_DISPLAY env var matching
        // ours.  This avoids killing Hyprland's (or another compositor's)
        // Xwayland running on a different TTY.
        let _ = std::process::Command::new("pkill")
            .args([
                "-fx",
                &format!("Xwayland.*-listenfd.*WAYLAND_DISPLAY={}.*", wayland_display),
            ])
            .status();
        // Also clean up any leftover X11 lock file for :0, :1 etc. that would
        // cause Xwayland to fail to bind its display number.
        for n in 0..10u32 {
            let lock = format!("/tmp/.X{}-lock", n);
            let sock = format!("/tmp/.X11-unix/X{}", n);
            if std::path::Path::new(&lock).exists() {
                // Only remove if the PID in the lock file is no longer alive.
                if let Ok(contents) = std::fs::read_to_string(&lock) {
                    let pid = contents.trim().parse::<u32>().unwrap_or(0);
                    let alive = pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists();
                    if !alive {
                        let _ = std::fs::remove_file(&lock);
                        let _ = std::fs::remove_file(&sock);
                        tracing::info!("cleaned up stale X11 lock :{}", n);
                    }
                }
            }
        }

        let xwayland_log =
            std::fs::File::create("/tmp/xwayland.log").context("create xwayland log")?;
        let xwayland_err = xwayland_log.try_clone()?;

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
                .env("XDG_RUNTIME_DIR", &xdg_runtime)
                .stdout(xwayland_log)
                .stderr(xwayland_err)
                .pre_exec(move || {
                    clear_cloexec(x_raw);
                    clear_cloexec(xw_raw);
                    clear_cloexec(dw_raw);
                    Ok(())
                })
                .spawn()
                .context("spawn Xwayland")?
        };

        drop(x_fd);
        drop(xw_fd);
        drop(display_w);

        self.child = Some(child);
        self.pending_screen = Some((screen_w, screen_h));

        Ok(display_r)
    }

    /// Phase 2: called by calloop when the display-number pipe is readable.
    /// Reads the display number, connects to Xwayland, and becomes the WM.
    pub fn finish_start(&mut self, display_r: OwnedFd) -> Result<()> {
        let display_num = read_display_number(display_r)?;
        self.display = Some(display_num);
        tracing::info!("XWayland on :{display_num}");
        unsafe {
            std::env::set_var("DISPLAY", format!(":{display_num}"));
        }

        let (screen_w, screen_h) = self.pending_screen.take().unwrap_or((1920, 1080));

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
            // Advertise the new EWMH atoms we now handle.
            atoms._NET_WM_WINDOW_TYPE.into(),
            atoms._NET_WM_WINDOW_TYPE_NORMAL.into(),
            atoms._NET_WM_WINDOW_TYPE_DIALOG.into(),
            atoms._NET_WM_WINDOW_TYPE_UTILITY.into(),
            atoms._NET_WM_WINDOW_TYPE_TOOLBAR.into(),
            atoms._NET_WM_WINDOW_TYPE_MENU.into(),
            atoms._NET_WM_WINDOW_TYPE_SPLASH.into(),
            atoms._NET_WM_WINDOW_TYPE_DND.into(),
            atoms._NET_WM_WINDOW_TYPE_DESKTOP.into(),
            atoms._NET_WM_WINDOW_TYPE_NOTIFICATION.into(),
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
        self.ready = true;
        Ok(())
    }

    /// Drain all pending X11 events and return compositor-level actions.
    /// Logs stale pending surfaces and x11_serials at trace level.
    pub fn dispatch_events(&mut self) -> Vec<X11Action> {
        let conn = match self.conn.as_ref() {
            Some(c) => c.clone(),
            None => return vec![],
        };
        let wm = match self.wm.as_mut() {
            Some(w) => w,
            None => return vec![],
        };

        let mut actions = Vec::new();
        loop {
            match conn.poll_for_event() {
                Ok(Some(ev)) => {
                    tracing::trace!("x11 event: {:?}", ev);
                    if let Some(a) = wm.handle_event(&conn, ev) {
                        actions.push(a);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("x11 poll error: {e}");
                    break;
                }
            }
        }

        // Diagnostic: surfaces/serials still waiting for their counterpart.
        for ps in &self.pending_surfaces {
            if ps.serial.is_none() {
                tracing::trace!(
                    "pending xwayland surface surf_id={:?} has no serial yet",
                    ps.surface.id()
                );
            }
        }
        if !self.x11_serials.is_empty() {
            tracing::trace!(
                "x11_serials awaiting Wayland surface: {:?}",
                self.x11_serials.keys().collect::<Vec<_>>()
            );
        }

        let _ = conn.flush();
        actions
    }

    pub fn set_focus(&self, x11_win: u32) {
        if let (Some(conn), Some(wm)) = (self.conn.as_ref(), self.wm.as_ref()) {
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
            use x11rb::connection::Connection as _;
            c.stream().as_raw_fd()
        })
    }

    pub fn read_surface_serial(
        &mut self,
        conn: &RustConnection,
        win: u32,
        wl_surface_serial_atom: u32,
    ) {
        let Ok(cookie) = conn.get_property(
            false,
            win,
            wl_surface_serial_atom,
            x11rb::protocol::xproto::AtomEnum::CARDINAL,
            0,
            2,
        ) else {
            return;
        };

        let Ok(reply) = cookie.reply() else { return };
        if reply.value.len() < 8 {
            return;
        }

        let lo = u32::from_ne_bytes(reply.value[0..4].try_into().unwrap()) as u64;
        let hi = u32::from_ne_bytes(reply.value[4..8].try_into().unwrap()) as u64;
        let serial = (hi << 32) | lo;
        self.x11_serials.insert(win, serial);
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

/// Simple blocking read — safe to call only from calloop after the fd is
/// readable (so read_line will not block).
fn read_display_number(fd: OwnedFd) -> Result<u32> {
    use std::io::{BufRead, BufReader};
    let f = unsafe { std::fs::File::from_raw_fd(fd.into_raw_fd()) };
    let mut line = String::new();
    BufReader::new(f)
        .read_line(&mut line)
        .context("read display number from Xwayland pipe")?;
    line.trim()
        .parse::<u32>()
        .context("parse display number from Xwayland")
}

unsafe fn clear_cloexec(fd: RawFd) {
    let fd = BorrowedFd::borrow_raw(fd);
    let flags = fcntl(fd, FcntlArg::F_GETFD).unwrap_or(0);
    let _ = fcntl(
        fd,
        FcntlArg::F_SETFD(FdFlag::from_bits_truncate(
            flags & !FdFlag::FD_CLOEXEC.bits(),
        )),
    );
}
