// ipc/mod.rs — Unix socket IPC server.
//
// Listens on $XDG_RUNTIME_DIR/trixie.sock (or /tmp/trixie-$UID.sock).
// Each connection sends one newline-terminated command and receives one
// newline-terminated reply, then closes.
//
// Commands (plain text, not JSON):
//   reload                — hot-reload config
//   quit                  — clean shutdown
//   switch_vt <n>         — switch to VT n (1-12)
//   workspace <n>         — jump to workspace n
//   status                — returns human-readable compositor status
//
// No serde, no libc — just std + smithay's calloop.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::atomic::Ordering,
};

use smithay::reexports::calloop::{
    generic::Generic, Interest, LoopHandle, Mode as PollMode, PostAction,
};

use crate::state::Trixie;

// ── Socket path ───────────────────────────────────────────────────────────────

pub fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("trixie.sock");
    }
    // Fall back to reading UID from /proc without libc.
    let uid = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("Uid:")).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .and_then(|u| u.parse::<u32>().ok())
            })
        })
        .unwrap_or(1000);
    PathBuf::from(format!("/tmp/trixie-{uid}.sock"))
}

// ── Server init ───────────────────────────────────────────────────────────────

pub fn init_ipc(
    handle: &LoopHandle<'static, Trixie>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path); // remove stale socket
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    tracing::info!("IPC socket: {:?}", path);

    handle.insert_source(
        Generic::new(listener, Interest::READ, PollMode::Level),
        |_, listener, state| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => handle_connection(stream, state),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        tracing::warn!("IPC accept: {e}");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    )?;

    Ok(path)
}

// ── Connection handler ────────────────────────────────────────────────────────

fn handle_connection(mut stream: UnixStream, state: &mut Trixie) {
    stream.set_nonblocking(false).ok();
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .ok();

    let mut line = String::new();
    {
        let mut reader = BufReader::new(&stream);
        if reader.read_line(&mut line).is_err() {
            return;
        }
    }
    let line = line.trim();
    if line.is_empty() {
        return;
    }

    let reply = dispatch(line, state);
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.write_all(b"\n");
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn dispatch(line: &str, state: &mut Trixie) -> String {
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "reload" => {
            state.apply_config_reload();
            "ok: config reloaded".into()
        }

        "quit" => {
            tracing::info!("IPC quit");
            state.running.store(false, Ordering::SeqCst);
            "ok: shutting down".into()
        }

        "switch_vt" => match arg.parse::<i32>() {
            Ok(n) if (1..=12).contains(&n) => {
                crate::session::switch_vt(state, n);
                format!("ok: switching to vt {n}")
            }
            _ => "err: usage: switch_vt <1-12>".into(),
        },

        "workspace" => match arg.parse::<u8>() {
            Ok(n) if n >= 1 => {
                state.twm.dispatch(crate::twm::TwmAction::Workspace(n));
                state.sync_focus();
                format!("ok: workspace {n}")
            }
            _ => "err: usage: workspace <n>".into(),
        },

        "status" => format_status(state),

        other => format!("err: unknown command '{other}'"),
    }
}

// ── Status formatter — no serde needed ───────────────────────────────────────

fn format_status(state: &Trixie) -> String {
    let snap = state.twm.snapshot();
    let mut out = String::new();

    out.push_str(&format!("active_workspace: {}\n", snap.active_ws + 1));
    out.push_str(&format!(
        "screen: {}x{}\n",
        snap.screen_rect.w, snap.screen_rect.h
    ));

    for ws in &snap.workspaces {
        let focused_title = ws
            .focused
            .and_then(|fid| ws.panes.iter().find(|p| p.id == fid))
            .map(|p| p.title.as_str())
            .unwrap_or("-");

        out.push_str(&format!(
            "workspace {}: panes={} layout={} focused=\"{}\"\n",
            ws.index + 1,
            ws.panes.len(),
            ws.layout,
            focused_title,
        ));
    }

    out.trim_end().to_owned()
}

// ── Cleanup ───────────────────────────────────────────────────────────────────

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}
