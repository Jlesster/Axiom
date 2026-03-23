// src/bin/axiomctl.rs — CLI client for the Axiom IPC socket.
//
// Usage:
//   axiomctl clients
//   axiomctl workspaces
//   axiomctl active-window
//   axiomctl monitors
//   axiomctl version
//   axiomctl dispatch close-window
//   axiomctl dispatch switch-workspace 3
//   axiomctl dispatch exec "foot"
//   axiomctl dispatch toggle-float
//   axiomctl dispatch toggle-fullscreen
//   axiomctl dispatch set-layout bsp
//   axiomctl dispatch move-to-workspace 2
//   axiomctl dispatch reload
//   axiomctl dispatch exit
//   axiomctl lua "return axiom.active_workspace()"

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
};

// axiomctl is a standalone binary — it can't depend on the axiom crate
// directly. We re-declare only what we need for the wire protocol.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum IpcRequest {
    Clients,
    Workspaces,
    Monitors,
    ActiveWindow,
    Version,
    CloseWindow {
        id: Option<u32>,
    },
    FocusWindow {
        id: u32,
    },
    MoveToWorkspace {
        workspace: usize,
    },
    SwitchWorkspace {
        workspace: usize,
    },
    ToggleFloat,
    ToggleFullscreen,
    ToggleMaximize,
    SetLayout {
        layout: String,
    },
    SetWindowGeometry {
        id: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    Exec {
        command: String,
    },
    Reload,
    Exit,
    Lua {
        code: String,
    },
    Bind {
        key: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct IpcResponse {
    ok: bool,
    error: Option<String>,
    data: Option<serde_json::Value>,
}

fn socket_path(display: &str) -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(base).join(format!("axiom-{}.sock", display.trim_start_matches(':')))
}

fn encode_request(r: &IpcRequest) -> Vec<u8> {
    let mut v = serde_json::to_vec(r).unwrap_or_default();
    v.push(b'\n');
    v
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: axiomctl <command> [args...]");
        std::process::exit(1);
    }

    let req = match parse_args(&args) {
        Some(r) => r,
        None => {
            eprintln!("Unknown command: {}", args.join(" "));
            std::process::exit(1);
        }
    };

    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-axiom".into());
    let path = socket_path(&display);

    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not connect to {path:?}: {e}");
            std::process::exit(1);
        }
    };

    stream.write_all(&encode_request(&req)).expect("write");

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read response");

    let resp: IpcResponse = serde_json::from_str(line.trim()).unwrap_or_else(|e| {
        eprintln!("Bad response: {e}");
        std::process::exit(1);
    });

    if !resp.ok {
        eprintln!("Error: {}", resp.error.unwrap_or_default());
        std::process::exit(1);
    }

    if let Some(data) = resp.data {
        println!("{}", serde_json::to_string_pretty(&data).unwrap());
    }
}

fn parse_args(args: &[String]) -> Option<IpcRequest> {
    let cmd = args[0].as_str();
    match cmd {
        "clients" => Some(IpcRequest::Clients),
        "workspaces" => Some(IpcRequest::Workspaces),
        "monitors" => Some(IpcRequest::Monitors),
        "active-window" => Some(IpcRequest::ActiveWindow),
        "version" => Some(IpcRequest::Version),

        "lua" => Some(IpcRequest::Lua {
            code: args.get(1).cloned().unwrap_or_default(),
        }),

        "dispatch" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("");
            match sub {
                "close-window" => Some(IpcRequest::CloseWindow {
                    id: args.get(2).and_then(|s| s.parse().ok()),
                }),
                "focus-window" => Some(IpcRequest::FocusWindow {
                    id: args.get(2)?.parse().ok()?,
                }),
                "switch-workspace" => Some(IpcRequest::SwitchWorkspace {
                    workspace: args.get(2)?.parse().ok()?,
                }),
                "move-to-workspace" => Some(IpcRequest::MoveToWorkspace {
                    workspace: args.get(2)?.parse().ok()?,
                }),
                "toggle-float" => Some(IpcRequest::ToggleFloat),
                "toggle-fullscreen" => Some(IpcRequest::ToggleFullscreen),
                "toggle-maximize" => Some(IpcRequest::ToggleMaximize),
                "set-layout" => Some(IpcRequest::SetLayout {
                    layout: args.get(2).cloned().unwrap_or_default(),
                }),
                "exec" => Some(IpcRequest::Exec {
                    command: args.get(2).cloned().unwrap_or_default(),
                }),
                "reload" => Some(IpcRequest::Reload),
                "exit" => Some(IpcRequest::Exit),
                "bind" => Some(IpcRequest::Bind {
                    key: args.get(2).cloned().unwrap_or_default(),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}
