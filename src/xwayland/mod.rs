/// xwayland/mod.rs — XWayland integration for Axiom.
///
/// Launches Xwayland as a child process, wires it up to the Wayland display
/// and a dedicated X11 display socket, and handles the initial handshake so
/// that X11 applications (Steam, Wine, legacy apps) work out of the box —
/// the same level of support as Hyprland / Sway.
///
/// Architecture:
///   1. We create a Unix socket pair for the Wayland connection.
///   2. We allocate an X display number (:N) by locking /tmp/.X<N>-lock.
///   3. We launch `Xwayland :N -rootless -terminate -listenfd <fd> -displayfd <fd2>`
///   4. We read back the display number from <fd2> to confirm Xwayland is ready.
///   5. We set DISPLAY=:N in the environment so child processes find it.
///   6. On compositor shutdown we kill Xwayland and clean up lock + socket files.
use anyhow::{Context, Result};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

// ── Public API ────────────────────────────────────────────────────────────────

pub struct XWayland {
    pub display: u32,
    pub socket_path: PathBuf,
    child: Arc<Mutex<Option<Child>>>,
}

impl XWayland {
    /// Start Xwayland and block until it signals readiness.
    /// Returns `None` if Xwayland is not installed (non-fatal).
    pub fn start(wayland_display: &str) -> Option<Self> {
        match Self::try_start(wayland_display) {
            Ok(xwl) => {
                tracing::info!("XWayland ready on :{}", xwl.display);
                Some(xwl)
            }
            Err(e) => {
                tracing::warn!("XWayland failed to start: {e}");
                None
            }
        }
    }

    fn try_start(wayland_display: &str) -> Result<Self> {
        // 1. Find a free X display number
        let display_num = find_free_display().context("find free X display")?;
        let display_str = format!(":{display_num}");

        // 2. Create the X11 Unix socket Xwayland will listen on
        let socket_path = PathBuf::from(format!("/tmp/.X11-unix/X{display_num}"));
        std::fs::create_dir_all("/tmp/.X11-unix").ok();
        let _ = std::fs::remove_file(&socket_path); // clean up any stale socket

        let x11_listener = UnixListener::bind(&socket_path).context("bind X11 socket")?;

        // 3. Create a pipe so Xwayland can signal readiness
        let (ready_read, ready_write) = crate::sys::pipe_cloexec().context("create ready pipe")?;

        // 4. Create a Wayland client socket pair for Xwayland to connect on
        let (wl_client_fd, wl_server_fd) =
            wayland_socket_pair().context("create Wayland socket pair for Xwayland")?;

        // 5. Write the lock file
        write_lock_file(display_num)?;

        // 6. Spawn Xwayland
        let x11_fd = x11_listener.as_raw_fd();
        let wl_fd = wl_client_fd.as_raw_fd();
        let ready_fd = ready_write.as_raw_fd();

        let mut cmd = Command::new("Xwayland");
        cmd.arg(&display_str)
            .arg("-rootless")
            .arg("-terminate")
            .arg("-listenfd")
            .arg(x11_fd.to_string())
            .arg("-wm")
            .arg(wl_fd.to_string())
            .arg("-displayfd")
            .arg(ready_fd.to_string())
            .env("WAYLAND_DISPLAY", wayland_display)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Clear CLOEXEC on the fds we want Xwayland to inherit.
        cmd.pre_exec_pass_fds(&[x11_fd, wl_fd, ready_fd]);
        let child = cmd.spawn().context("spawn Xwayland")?;

        // 7. Wait for the ready signal (Xwayland writes the display number)
        drop(ready_write); // close write end in our process
        let ready = wait_for_ready(ready_read)?;

        tracing::debug!("Xwayland confirmed display :{ready}");

        // 8. Set DISPLAY for child processes
        std::env::set_var("DISPLAY", &display_str);

        Ok(Self {
            display: display_num,
            socket_path,
            child: Arc::new(Mutex::new(Some(child))),
        })
    }

    /// Kill Xwayland cleanly. Called on compositor shutdown.
    pub fn stop(&self) {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        // Clean up socket and lock file
        let _ = std::fs::remove_file(&self.socket_path);
        let lock = PathBuf::from(format!("/tmp/.X{}-lock", self.display));
        let _ = std::fs::remove_file(lock);
        // Unset DISPLAY so any subsequent children don't try to use the dead server
        std::env::remove_var("DISPLAY");
    }
}

impl Drop for XWayland {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Find the lowest X display number not currently in use.
fn find_free_display() -> Result<u32> {
    for n in 0u32..=99 {
        let lock = PathBuf::from(format!("/tmp/.X{n}-lock"));
        if !lock.exists() {
            // Double-check the socket too
            let sock = PathBuf::from(format!("/tmp/.X11-unix/X{n}"));
            if !sock.exists() {
                return Ok(n);
            }
        }
    }
    anyhow::bail!("No free X display number found (tried :0 .. :99)")
}

fn write_lock_file(display: u32) -> Result<()> {
    use std::io::Write;
    let path = PathBuf::from(format!("/tmp/.X{display}-lock"));
    let mut f =
        std::fs::File::create(&path).with_context(|| format!("create lock file {path:?}"))?;
    write!(f, "{:>10}\n", std::process::id())?;
    Ok(())
}

/// Create a connected Unix socket pair for use as a Wayland client connection.
fn wayland_socket_pair() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds: [RawFd; 2] = [-1, -1];
    let ret = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if ret < 0 {
        anyhow::bail!("socketpair: {}", std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Read from the ready pipe until Xwayland writes a display number (or EOF).
fn wait_for_ready(pipe_read: OwnedFd) -> Result<u32> {
    use std::io::Read;
    use std::os::unix::io::IntoRawFd;

    // Set a 5-second timeout via poll(2)
    let raw = pipe_read.as_raw_fd();
    let mut pfd = libc::pollfd {
        fd: raw,
        events: libc::POLLIN,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut pfd, 1, 5000) };
    if ret <= 0 {
        anyhow::bail!("Xwayland did not signal readiness within 5 seconds");
    }

    let mut f = unsafe { std::fs::File::from_raw_fd(pipe_read.into_raw_fd()) };
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        // Xwayland started and closed the fd without writing — treat as display 0
        return Ok(0);
    }
    trimmed
        .parse::<u32>()
        .with_context(|| format!("parse Xwayland display number from '{trimmed}'"))
}

// ── pre_exec_pass_fds helper trait ───────────────────────────────────────────
//
// We need to clear the CLOEXEC flag on the fds we're passing to Xwayland
// *after* fork but *before* exec. This is done with a pre_exec hook.

trait CommandPassFds {
    fn pre_exec_pass_fds(&mut self, fds: &[RawFd]) -> &mut Self;
}

impl CommandPassFds for Command {
    fn pre_exec_pass_fds(&mut self, fds: &[RawFd]) -> &mut Self {
        let fds: Vec<RawFd> = fds.to_vec();
        unsafe {
            self.pre_exec(move || {
                for &fd in &fds {
                    // Clear CLOEXEC so the fd is inherited by Xwayland
                    let flags = libc::fcntl(fd, libc::F_GETFD, 0);
                    if flags >= 0 {
                        libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    }
                }
                Ok(())
            });
        }
        self
    }
}

// ── XWayland state integrated into Axiom ─────────────────────────────────────
//
// Called from state.rs new() to optionally start XWayland.

/// Attempt to start XWayland and log the result. Returns None if unavailable.
pub fn maybe_start(wayland_socket: &str) -> Option<XWayland> {
    // Only start if Xwayland binary is findable
    if which_xwayland().is_none() {
        tracing::info!("Xwayland not found in PATH — X11 support disabled");
        return None;
    }
    XWayland::start(wayland_socket)
}

fn which_xwayland() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let candidate = dir.join("Xwayland");
            if candidate.is_file() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}
