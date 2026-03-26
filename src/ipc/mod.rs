use anyhow::{Context, Result};
use calloop::LoopHandle;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use wayland_server::DisplayHandle;

use crate::state::Axiom;
use crate::wm::WindowId;

pub struct IpcServer {
    listener: UnixListener,
    pub socket_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Clients,
    Workspaces,
    Monitors,
    ActiveWindow,
    Version,
    CloseWindow { id: Option<WindowId> },
    FocusWindow { id: Option<WindowId> },
    MoveToWorkspace { workspace: usize },
    ToggleFloat,
    ToggleFullscreen,
    SwitchWorkspace { workspace: usize },
    SetLayout { layout: String },
    Reload,
    Exec { command: String },
    Exit,
    Lua { code: String },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum IpcResponse {
    Ok,
    Error { error: String },
    Data(serde_json::Value),
}

impl IpcServer {
    pub fn new(_dh: &DisplayHandle) -> Result<Self> {
        let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        let path = format!("{runtime}/axiom-{display}.sock");
        let _ = std::fs::remove_file(&path);
        let listener =
            UnixListener::bind(&path).with_context(|| format!("bind IPC socket {path}"))?;
        listener.set_nonblocking(true)?;
        tracing::info!("IPC socket: {path}");
        Ok(Self {
            listener,
            socket_path: path,
        })
    }

    pub fn register(&self, loop_handle: &LoopHandle<'static, Axiom>) -> Result<()> {
        use calloop::generic::Generic;
        use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

        // Clone the listener so we can own an fd for calloop
        let cloned = self.listener.try_clone()?;
        let raw = {
            use std::os::unix::io::IntoRawFd;
            cloned.into_raw_fd()
        };
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };

        loop_handle
            .insert_source(
                Generic::new(owned, calloop::Interest::READ, calloop::Mode::Level),
                |_, _, state: &mut Axiom| {
                    let path = state.ipc.socket_path.clone();
                    accept_all(&path, state);
                    Ok(calloop::PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("register IPC: {e}"))?;
        Ok(())
    }
}

/// Accept all pending connections on the named socket, processing each one.
/// We open a new UnixListener reference from the path each time to avoid
/// borrowing IpcServer alongside Axiom.
fn accept_all(socket_path: &str, state: &mut Axiom) {
    // Re-open the socket in nonblocking mode to accept pending connections
    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        // Already bound — this is the normal case; use a raw dup instead
        Err(_) => {
            // Dup the fd from the stored listener
            use std::os::unix::io::{AsRawFd, FromRawFd};
            let raw = state.ipc.listener.as_raw_fd();
            let dup = unsafe { libc::dup(raw) };
            if dup < 0 {
                return;
            }
            unsafe { UnixListener::from_raw_fd(dup) }
        }
    };
    listener.set_nonblocking(true).ok();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = handle_client(&mut stream, state);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => {
                tracing::warn!("IPC accept error: {e}");
                break;
            }
        }
    }
}

fn handle_client(stream: &mut UnixStream, state: &mut Axiom) -> Result<()> {
    let mut buf = String::new();
    stream.read_to_string(&mut buf)?;
    let resp = match serde_json::from_str::<IpcRequest>(buf.trim()) {
        Ok(req) => process_request(req, state),
        Err(e) => IpcResponse::Error {
            error: format!("parse: {e}"),
        },
    };
    let out = serde_json::to_string(&resp)?;
    stream.write_all(out.as_bytes())?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn process_request(req: IpcRequest, state: &mut Axiom) -> IpcResponse {
    match req {
        IpcRequest::Version => {
            IpcResponse::Data(serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }))
        }
        IpcRequest::Clients => {
            let v: Vec<_> = state.wm.windows.values().map(|w| serde_json::json!({
                "id": w.id, "app_id": w.app_id, "title": w.title,
                "workspace": w.workspace+1, "x": w.rect.x, "y": w.rect.y,
                "w": w.rect.w, "h": w.rect.h, "floating": w.floating, "fullscreen": w.fullscreen,
            })).collect();
            IpcResponse::Data(serde_json::json!(v))
        }
        IpcRequest::Workspaces => {
            let v: Vec<_> = state
                .wm
                .workspaces
                .iter()
                .enumerate()
                .map(|(i, ws)| {
                    serde_json::json!({
                        "index": i+1, "focused": ws.focused, "window_count": ws.windows.len(),
                        "active": i == state.wm.active_ws(),
                    })
                })
                .collect();
            IpcResponse::Data(serde_json::json!(v))
        }
        IpcRequest::ActiveWindow => {
            if let Some(id) = state.wm.focused_window() {
                if let Some(w) = state.wm.windows.get(&id) {
                    return IpcResponse::Data(
                        serde_json::json!({ "id": w.id, "app_id": w.app_id, "title": w.title }),
                    );
                }
            }
            IpcResponse::Data(serde_json::Value::Null)
        }
        IpcRequest::SwitchWorkspace { workspace } => {
            state.wm.switch_workspace(workspace.saturating_sub(1));
            state.needs_redraw = true;
            IpcResponse::Ok
        }
        IpcRequest::CloseWindow { id } => {
            if let Some(id) = id.or_else(|| state.wm.focused_window()) {
                state.close_window(id);
            }
            IpcResponse::Ok
        }
        IpcRequest::ToggleFloat => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.toggle_float(id);
                state.needs_redraw = true;
            }
            IpcResponse::Ok
        }
        IpcRequest::ToggleFullscreen => {
            if let Some(id) = state.wm.focused_window() {
                let on = !state.wm.windows[&id].fullscreen;
                state.wm.fullscreen_window(id, on);
                state.needs_redraw = true;
            }
            IpcResponse::Ok
        }
        IpcRequest::SetLayout { layout } => {
            let ws = state.wm.active_ws();
            if let Some(w) = state.wm.workspaces.get_mut(ws) {
                w.layout = crate::wm::Layout::from_str(&layout);
            }
            state.wm.reflow();
            state.needs_redraw = true;
            IpcResponse::Ok
        }
        IpcRequest::Exec { command } => {
            crate::scripting::spawn(&command);
            IpcResponse::Ok
        }
        IpcRequest::Reload => {
            let _ = state.reload_config();
            IpcResponse::Ok
        }
        IpcRequest::Exit => {
            state.loop_signal.stop();
            IpcResponse::Ok
        }
        IpcRequest::Lua { code } => match state.script.lua.load(&code).exec() {
            Ok(_) => IpcResponse::Ok,
            Err(e) => IpcResponse::Error {
                error: e.to_string(),
            },
        },
        _ => IpcResponse::Error {
            error: "not implemented".to_string(),
        },
    }
}
