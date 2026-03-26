mod backend;
mod input;
mod ipc;
mod proto;
mod render;
mod scripting;
mod state;
mod sys;
mod wm;
mod xwayland;

use anyhow::Result;
use calloop::EventLoop;
use std::os::unix::io::{AsRawFd, FromRawFd};
use tracing_subscriber::EnvFilter;
use wayland_server::Display;
// wayland-server 0.31 uses ListeningSocket, not display.add_socket_auto()
use wayland_server::ListeningSocket;

use state::Axiom;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("AXIOM_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Starting Axiom compositor");

    // ── Create Wayland display and bind socket ────────────────────────────────
    let mut display: Display<Axiom> = Display::new()?;

    // In wayland-server 0.31, sockets are created via ListeningSocket, not
    // display.add_socket_auto(). bind_auto() picks "wayland-0", "wayland-1", …
    let listening_socket = ListeningSocket::bind_auto("wayland", 0..=9)
        .map_err(|e| anyhow::anyhow!("Failed to create Wayland socket: {e}"))?;
    // socket_name() returns Option<&OsStr> — unwrap and convert to String.
    let socket_str = listening_socket
        .socket_name()
        .expect("listening socket has no name")
        .to_string_lossy()
        .to_string();
    std::env::set_var("WAYLAND_DISPLAY", &socket_str);
    tracing::info!("Wayland socket: {socket_str}");

    let dh = display.handle();

    // ── Event loop ────────────────────────────────────────────────────────────
    let mut event_loop: EventLoop<Axiom> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    let mut state = Axiom::new(display, loop_handle.clone(), loop_signal, &dh)?;

    // ── Register Wayland display fd with calloop ──────────────────────────────
    let display_raw = state.display_raw_fd();
    loop_handle.insert_source(
        calloop::generic::Generic::new(
            unsafe { std::os::unix::io::OwnedFd::from_raw_fd(libc::dup(display_raw)) },
            calloop::Interest::READ,
            calloop::Mode::Level,
        ),
        |_, _, state: &mut Axiom| {
            state
                .dispatch_clients()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(calloop::PostAction::Continue)
        },
    )?;

    // ── Register the listening socket with calloop ────────────────────────────
    // ListeningSocket doesn't implement calloop's EventSource directly, so we
    // dup its raw fd into a Generic source and call accept() in the callback.
    // The ListeningSocket must outlive the event loop, so we keep it in scope.
    {
        let ls_raw = listening_socket.as_raw_fd();
        let ls_dup = unsafe { libc::dup(ls_raw) };
        if ls_dup < 0 {
            anyhow::bail!("dup listening socket fd failed");
        }
        let ls_owned = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(ls_dup) };
        loop_handle.insert_source(
            calloop::generic::Generic::new(ls_owned, calloop::Interest::READ, calloop::Mode::Level),
            move |_, _, state: &mut Axiom| {
                // Accept all pending connections.
                loop {
                    match listening_socket.accept() {
                        Ok(Some(client_stream)) => {
                            state
                                .display
                                .handle()
                                .insert_client(client_stream, std::sync::Arc::new(()))
                                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                        }
                        Ok(None) => break, // no more pending connections
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    }
                }
                Ok(calloop::PostAction::Continue)
            },
        )?;
    }

    // ── Register DRM fd for page-flip event draining ──────────────────────────
    // The kernel queues a DRM_EVENT_FLIP_COMPLETE after each page_flip().
    // If we don't read these, the kernel queue fills and page_flip() returns
    // EBUSY, freezing the display after ~2-3 frames.
    {
        use std::os::unix::io::AsRawFd;
        let drm_raw = state.backend.drm.raw_fd();
        let drm_dup = unsafe { libc::dup(drm_raw) };
        if drm_dup >= 0 {
            loop_handle.insert_source(
                calloop::generic::Generic::new(
                    unsafe { std::os::unix::io::OwnedFd::from_raw_fd(drm_dup) },
                    calloop::Interest::READ,
                    calloop::Mode::Level,
                ),
                |_, fd, _state: &mut Axiom| {
                    // Drain all pending DRM events (page-flip acks).
                    // We don't need the timestamps so a raw read is fine.
                    let mut buf = [0u8; 256];
                    loop {
                        use std::os::unix::io::AsRawFd;
                        let n = unsafe {
                            libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len())
                        };
                        if n <= 0 {
                            break;
                        }
                    }
                    Ok(calloop::PostAction::Continue)
                },
            )?;
        }
    }

    // ── Input, IPC, config ────────────────────────────────────────────────────
    state.input.register_fd(&loop_handle)?;
    state.ipc.register(&loop_handle)?;

    if let Err(e) = state.script.load_config(&mut state.wm) {
        tracing::warn!("Config load error: {e}");
    }

    // ── Optional: start XWayland ──────────────────────────────────────────────
    // Store on state so it lives as long as the compositor.
    state.xwayland = crate::xwayland::maybe_start(&socket_str);

    // ── Install SIGTERM/SIGINT handler ────────────────────────────────────────
    let sigterm = crate::backend::session::install_sigterm_handler();

    state.script.emit_bare("compositor.ready");
    tracing::info!("Axiom running — WAYLAND_DISPLAY={socket_str}");

    event_loop.run(
        Some(std::time::Duration::from_millis(8)),
        &mut state,
        |state| {
            // Check for shutdown signal
            if sigterm.load(std::sync::atomic::Ordering::Acquire) {
                state.loop_signal.stop();
                return;
            }

            state.flush_clients();
            state.drain_actions();

            if state.needs_redraw {
                if let Err(e) = state.render.render_frame(
                    &mut state.backend,
                    &state.wm,
                    state.input.pointer_x,
                    state.input.pointer_y,
                ) {
                    tracing::error!("Render error: {e}");
                }
                state.needs_redraw = false;
            }
        },
    )?;

    tracing::info!("Axiom shutting down");
    Ok(())
}
