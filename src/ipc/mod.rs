// src/ipc/mod.rs — Unix-socket IPC server.
//
// Listens on $XDG_RUNTIME_DIR/axiom-<display>.sock.
// Wire format: one newline-terminated JSON IpcRequest in, one IpcResponse out.
// Drained in the main loop via drain_ipc() — no calloop source needed.

pub mod commands;

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        io::{AsRawFd, RawFd},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
};

use anyhow::{Context, Result};

use crate::state::Axiom;
use commands::*;

// ── Socket path ───────────────────────────────────────────────────────────────

pub fn socket_path(display: &str) -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(base).join(format!("axiom-{}.sock", display.trim_start_matches(':')))
}

// ── Server ────────────────────────────────────────────────────────────────────

pub struct IpcServer {
    pub listener: UnixListener,
    pub path: PathBuf,
}

impl IpcServer {
    pub fn bind(display: &str) -> Result<Self> {
        let path = socket_path(display);
        let _ = std::fs::remove_file(&path);
        let listener =
            UnixListener::bind(&path).with_context(|| format!("bind IPC socket {:?}", path))?;
        listener.set_nonblocking(true)?;
        tracing::info!("IPC socket: {:?}", path);
        Ok(Self { listener, path })
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── Drain all pending connections (called from main loop) ─────────────────────

pub fn drain_ipc(state: &mut Axiom) {
    loop {
        match state.ipc.listener.accept() {
            Ok((stream, _)) => handle_connection(stream, state),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => {
                tracing::warn!("IPC accept: {e}");
                break;
            }
        }
    }
}

// ── Per-connection handler ────────────────────────────────────────────────────

fn handle_connection(stream: UnixStream, state: &mut Axiom) {
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let resp = match decode_request(line.as_bytes()) {
        Ok(req) => dispatch(req, state),
        Err(e) => IpcResponse::err(format!("parse error: {e}")),
    };
    let bytes = encode_response(&resp);
    let _ = (&stream).write_all(&bytes);
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

fn dispatch(req: IpcRequest, state: &mut Axiom) -> IpcResponse {
    match req {
        // ── Queries ───────────────────────────────────────────────────────────
        IpcRequest::Clients => {
            let clients: Vec<ClientInfo> = state
                .wm
                .workspaces
                .iter()
                .flat_map(|ws| {
                    ws.windows.iter().filter_map(|&id| {
                        let w = state.wm.windows.get(&id)?;
                        Some(ClientInfo {
                            id: w.id,
                            app_id: w.app_id.clone(),
                            title: w.title.clone(),
                            workspace: ws.index + 1,
                            floating: w.floating,
                            fullscreen: w.fullscreen,
                            maximized: w.maximized,
                            x: w.rect.x,
                            y: w.rect.y,
                            w: w.rect.w,
                            h: w.rect.h,
                            xwayland: state.xwayland.is_xwayland_window(id),
                        })
                    })
                })
                .collect();
            IpcResponse::ok(clients)
        }

        IpcRequest::Workspaces => {
            let aws = state.wm.active_ws();
            let infos: Vec<WorkspaceInfo> = state
                .wm
                .workspaces
                .iter()
                .map(|ws| WorkspaceInfo {
                    index: ws.index + 1,
                    name: format!("{}", ws.index + 1),
                    active: ws.index == aws,
                    window_count: ws.windows.len(),
                    focused_id: ws.focused,
                })
                .collect();
            IpcResponse::ok(infos)
        }

        IpcRequest::Monitors => {
            let infos: Vec<MonitorInfo> = state
                .outputs
                .iter()
                .enumerate()
                .map(|(i, out)| MonitorInfo {
                    index: i,
                    name: out.name.clone(),
                    x: state.wm.monitors.get(i).map(|m| m.x).unwrap_or(0),
                    y: state.wm.monitors.get(i).map(|m| m.y).unwrap_or(0),
                    width: out.width as i32,
                    height: out.height as i32,
                    scale: out.scale,
                    active_workspace: state
                        .wm
                        .monitors
                        .get(i)
                        .map(|m| m.active_ws + 1)
                        .unwrap_or(1),
                })
                .collect();
            IpcResponse::ok(infos)
        }

        IpcRequest::ActiveWindow => {
            match state
                .wm
                .focused_window()
                .and_then(|id| state.wm.windows.get(&id))
            {
                Some(w) => IpcResponse::ok(ClientInfo {
                    id: w.id,
                    app_id: w.app_id.clone(),
                    title: w.title.clone(),
                    workspace: state.wm.active_ws() + 1,
                    floating: w.floating,
                    fullscreen: w.fullscreen,
                    maximized: w.maximized,
                    x: w.rect.x,
                    y: w.rect.y,
                    w: w.rect.w,
                    h: w.rect.h,
                    xwayland: state.xwayland.is_xwayland_window(w.id),
                }),
                None => IpcResponse::ok(serde_json::Value::Null),
            }
        }

        IpcRequest::Version => IpcResponse::ok(VersionInfo {
            compositor: "axiom",
            version: env!("CARGO_PKG_VERSION"),
            wayland_display: state.socket_name.clone(),
        }),

        // ── Window actions ────────────────────────────────────────────────────
        IpcRequest::CloseWindow { id } => {
            let target = id.or_else(|| state.wm.focused_window());
            match target {
                Some(id) => {
                    state.close_window(id);
                    IpcResponse::ok_empty()
                }
                None => IpcResponse::err("no focused window"),
            }
        }

        IpcRequest::FocusWindow { id } => {
            state.wm.focus_window(id);
            state.sync_keyboard_focus();
            state.needs_redraw = true;
            IpcResponse::ok_empty()
        }

        IpcRequest::MoveToWorkspace { workspace } => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.move_to_workspace(id, workspace.saturating_sub(1));
                state.needs_redraw = true;
            }
            IpcResponse::ok_empty()
        }

        IpcRequest::ToggleFloat => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.toggle_float(id);
                state.needs_redraw = true;
            }
            IpcResponse::ok_empty()
        }

        IpcRequest::ToggleFullscreen => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.toggle_fullscreen(id);
                state.send_configure_focused();
                state.needs_redraw = true;
            }
            IpcResponse::ok_empty()
        }

        IpcRequest::ToggleMaximize => {
            if let Some(id) = state.wm.focused_window() {
                let cur = state
                    .wm
                    .windows
                    .get(&id)
                    .map(|w| w.maximized)
                    .unwrap_or(false);
                state.wm.maximize_window(id, !cur);
                state.send_configure_focused();
                state.needs_redraw = true;
            }
            IpcResponse::ok_empty()
        }

        IpcRequest::SetWindowGeometry { id, x, y, w, h } => {
            if let Some(win) = state.wm.windows.get_mut(&id) {
                if win.floating {
                    win.rect = crate::wm::Rect::new(x, y, w, h);
                    win.float_rect = win.rect;
                    state.needs_redraw = true;
                    IpcResponse::ok_empty()
                } else {
                    IpcResponse::err("window is not floating")
                }
            } else {
                IpcResponse::err("window not found")
            }
        }

        // ── Workspace actions ─────────────────────────────────────────────────
        IpcRequest::SwitchWorkspace { workspace } => {
            state.wm.switch_workspace(workspace.saturating_sub(1));
            state.needs_redraw = true;
            IpcResponse::ok_empty()
        }

        IpcRequest::SetLayout { layout } => {
            use crate::wm::Layout;
            let l = match layout.as_str() {
                "tile" | "master_stack" => Layout::MasterStack,
                "bsp" => Layout::Bsp,
                "monocle" | "max" => Layout::Monocle,
                "float" => Layout::Float,
                other => return IpcResponse::err(format!("unknown layout '{other}'")),
            };
            let aws = state.wm.active_ws();
            state.wm.workspaces[aws].layout = l;
            state.wm.reflow();
            state.needs_redraw = true;
            IpcResponse::ok_empty()
        }

        // ── Compositor ────────────────────────────────────────────────────────
        IpcRequest::Reload => {
            state.reload_config();
            IpcResponse::ok_empty()
        }

        IpcRequest::Exec { command } => {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .spawn()
                .ok();
            IpcResponse::ok_empty()
        }

        IpcRequest::Exit => {
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
            IpcResponse::ok_empty()
        }

        IpcRequest::Lua { code } => match state.script.lua.load(&code).eval::<mlua::Value>() {
            Ok(v) => IpcResponse::ok(serde_json::Value::String(format!("{v:?}"))),
            Err(e) => IpcResponse::err(format!("{e}")),
        },

        IpcRequest::Bind { key } => match state.script.fire_keybind(&key) {
            Ok(()) => IpcResponse::ok_empty(),
            Err(e) => IpcResponse::err(format!("{e}")),
        },
    }
}
