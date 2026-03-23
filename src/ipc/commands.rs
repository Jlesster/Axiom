// src/ipc/commands.rs — JSON command/response schema for axiomctl <-> axiom.
//
// Wire format: newline-delimited JSON.
// Client sends one IpcRequest, reads one IpcResponse, closes.

use serde::{Deserialize, Serialize};

// ── Request ───────────────────────────────────────────────────────────────────

/// Rename all variants that have a field named `cmd` to use `command` instead,
/// and use adjacently-tagged serde so the tag field never collides.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IpcRequest {
    // ── Query ─────────────────────────────────────────────────────────────────
    /// List all windows on the active workspace.
    Clients,
    /// List all workspaces and their state.
    Workspaces,
    /// Return info about all monitors.
    Monitors,
    /// Return the focused window.
    ActiveWindow,
    /// Return compositor version / build info.
    Version,

    // ── Window actions ────────────────────────────────────────────────────────
    /// Close the focused window (or specific id).
    CloseWindow { id: Option<u32> },
    /// Focus window by id.
    FocusWindow { id: u32 },
    /// Move focused window to workspace n (1-based).
    MoveToWorkspace { workspace: usize },
    /// Toggle float on focused window.
    ToggleFloat,
    /// Toggle fullscreen on focused window.
    ToggleFullscreen,
    /// Toggle maximise on focused window.
    ToggleMaximize,
    /// Set window geometry (floating only).
    SetWindowGeometry {
        id: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },

    // ── Workspace actions ─────────────────────────────────────────────────────
    /// Switch to workspace n (1-based).
    SwitchWorkspace { workspace: usize },
    /// Set layout on current workspace.
    SetLayout { layout: String },

    // ── Compositor ────────────────────────────────────────────────────────────
    /// Reload the Lua config.
    Reload,
    /// Execute a shell command.
    Exec { command: String },
    /// Terminate the compositor.
    Exit,
    /// Evaluate a Lua snippet and return its result as a string.
    Lua { code: String },
    /// Emit a Lua keybind by name.
    Bind { key: String },
}

// ── Response ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl IpcResponse {
    pub fn ok(data: impl Serialize) -> Self {
        Self {
            ok: true,
            error: None,
            data: Some(serde_json::to_value(data).unwrap_or(serde_json::Value::Null)),
        }
    }
    pub fn ok_empty() -> Self {
        Self {
            ok: true,
            error: None,
            data: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            data: None,
        }
    }
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

/// Serialise a request to a newline-terminated byte string.
pub fn encode_request(r: &IpcRequest) -> Vec<u8> {
    let mut v = serde_json::to_vec(r).unwrap_or_default();
    v.push(b'\n');
    v
}

/// Serialise a response to a newline-terminated byte string.
pub fn encode_response(r: &IpcResponse) -> Vec<u8> {
    let mut v = serde_json::to_vec(r).unwrap_or_default();
    v.push(b'\n');
    v
}

/// Parse a request from a byte slice (may contain trailing newline).
pub fn decode_request(buf: &[u8]) -> anyhow::Result<IpcRequest> {
    Ok(serde_json::from_slice(buf.trim_ascii_end())?)
}

// ── Data types returned in IpcResponse::data ─────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct ClientInfo {
    pub id: u32,
    pub app_id: String,
    pub title: String,
    pub workspace: usize,
    pub floating: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub xwayland: bool,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub index: usize,
    pub name: String,
    pub active: bool,
    pub window_count: usize,
    pub focused_id: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub active_workspace: usize,
}

#[derive(Serialize, Deserialize)]
pub struct VersionInfo {
    pub compositor: &'static str,
    pub version: &'static str,
    pub wayland_display: String,
}
