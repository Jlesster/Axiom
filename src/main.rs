// src/main.rs — Axiom compositor entry point.

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

use std::{
    os::unix::io::{AsRawFd, FromRawFd, IntoRawFd},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use calloop::{
    generic::Generic,
    signals::{Signal, Signals},
    EventLoop, Interest, Mode, PostAction,
};
use wayland_server::{backend::ClientData as WlClientData, Display};

use crate::{
    backend::Backend,
    input::InputState,
    proto::seat::SeatState,
    render::RenderState,
    scripting::ScriptEngine,
    state::{Axiom, GrabKind, OutputState},
    wm::{anim::AnimSet, WmConfig, WmState},
    xwayland::X11Action,
};

struct NoopClientData;
impl WlClientData for NoopClientData {
    fn initialized(&self, _: wayland_server::backend::ClientId) {}
    fn disconnected(
        &self,
        _: wayland_server::backend::ClientId,
        _: wayland_server::backend::DisconnectReason,
    ) {
    }
}

macro_rules! probe {
    ($msg:expr) => {{
        use std::io::Write;
        let _ = std::io::stderr().write_all(concat!("PROBE ", $msg, "\n").as_bytes());
        let _ = std::io::stderr().flush();
    }};
}

fn main() -> Result<()> {
    probe!("1: main entered");

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(std::env::var("AXIOM_LOG").unwrap_or_else(|_| "axiom=debug,warn".into()))
        .init();

    probe!("2: tracing init done");

    let mut event_loop: EventLoop<'static, Axiom> = EventLoop::try_new()?;
    probe!("3: event loop created");

    let loop_handle = event_loop.handle();

    let session = backend::session::Session::open()?;
    probe!("4: session opened");

    let gpu_path = find_primary_gpu()?;
    probe!("5: gpu found");

    let mut backend = Backend::open(&gpu_path, session)?;
    probe!("6: backend opened");

    let outputs_raw = backend.create_outputs()?;
    probe!("7: outputs created");

    if let Some(out) = outputs_raw.first() {
        out.make_current(&backend.egl)?;
    }
    probe!("8: egl current");

    let render = RenderState::new()?;
    probe!("9: render state created");

    let mut display: Display<Axiom> = Display::new()?;
    probe!("10: wayland display created");

    let display_handle = display.handle();
    let socket_name = "wayland-axiom".to_string();
    let listener =
        wayland_server::ListeningSocket::bind(&socket_name).context("bind Wayland socket")?;
    probe!("11: wayland socket bound");

    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    }

    proto::register_globals(&display_handle);
    probe!("12: globals registered");

    let (sw, sh) = primary_output_size(&outputs_raw);
    let mut wm = WmState::new(sw, sh, WmConfig::default());
    probe!("13: wm created");

    let config_dir = xdg_config_dir();
    let script = ScriptEngine::new(&config_dir, &wm)?;
    probe!("14: script engine created");

    if let Err(e) = script.run_rc(&mut wm) {
        tracing::warn!("RC script error (continuing without config): {e}");
    }
    probe!("15: rc script ran");

    let input = InputState::new(&backend.session)?;
    probe!("16: input created");

    let seat = SeatState::new();
    probe!("17: seat created");

    let ipc = ipc::IpcServer::bind(&socket_name)?;
    probe!("18: ipc bound");

    let running = Arc::new(AtomicBool::new(true));
    let outputs: Vec<OutputState> = outputs_raw
        .into_iter()
        .enumerate()
        .map(|(i, surf)| {
            let (w, h) = surf.mode_size();
            OutputState {
                name: format!("output-{i}"),
                width: w,
                height: h,
                refresh_mhz: 60_000,
                scale: 1.0,
                render_surf: surf,
                wl_id: i as u32,
                last_vblank: Instant::now(),
                frame_pending: false,
            }
        })
        .collect();
    probe!("19: outputs built");

    let mut state = Axiom {
        display: display_handle.clone(),
        socket_name: socket_name.clone(),
        backend,
        render,
        input,
        seat,
        wm,
        anim: AnimSet::new(),
        script,
        ipc,
        outputs,
        surface_map: Default::default(),
        toplevel_map: Default::default(),
        pending_windows: Default::default(),
        closing_windows: Default::default(),
        layer_surfaces: Default::default(),
        idle_inhibit: proto::idle_inhibit::IdleInhibitState::new(),
        xwayland: xwayland::XWaylandState::new(),
        running: Arc::clone(&running),
        handle: loop_handle.clone(),
        start_time: Instant::now(),
        needs_redraw: true,
        grab: GrabKind::None,
    };
    probe!("20: state constructed");

    // ── Initial modeset ───────────────────────────────────────────────────────
    for out in &mut state.outputs {
        use ::drm::control::Device;
        if let Err(e) = out.render_surf.make_current(&state.backend.egl) {
            tracing::warn!("initial make_current: {e}");
            continue;
        }
        unsafe {
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
        match out
            .render_surf
            .present(&state.backend.egl, &state.backend.drm)
        {
            Ok(fb) => {
                if let Ok(res) = state.backend.drm.resource_handles() {
                    let conn = res.connectors().iter().find_map(|&ch| {
                        state.backend.drm.get_connector(ch, false).ok().and_then(
                            |c: ::drm::control::connector::Info| {
                                let matches = c
                                    .current_encoder()
                                    .and_then(|eh| state.backend.drm.get_encoder(eh).ok())
                                    .map(|enc: ::drm::control::encoder::Info| {
                                        enc.crtc() == Some(out.render_surf.crtc)
                                    })
                                    .unwrap_or(false);
                                if matches {
                                    Some(ch)
                                } else {
                                    None
                                }
                            },
                        )
                    });
                    if let Some(conn_h) = conn {
                        let mode = out.render_surf.mode;
                        if let Err(e) = state.backend.drm.set_crtc(
                            out.render_surf.crtc,
                            Some(fb),
                            (0, 0),
                            &[conn_h],
                            Some(mode),
                        ) {
                            tracing::warn!("set_crtc: {e}");
                        } else {
                            tracing::info!("Initial modeset OK for {}", out.name);
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("initial present: {e}"),
        }
    }
    probe!("21: initial modeset done");

    // ── Hardware cursor ───────────────────────────────────────────────────────
    match render::cursor::HwCursor::load(&state.backend.drm) {
        Ok(cur) => {
            tracing::info!("hardware cursor loaded");
            for out in &state.outputs {
                cur.set_on_crtc(&state.backend.drm, out.render_surf.crtc, 0, 0);
            }
            state.input.hw_cursor_active = true;
            state.render.hw_cursor = Some(cur);
        }
        Err(e) => tracing::warn!("hardware cursor unavailable ({e}), using software fallback"),
    }
    probe!("22: hw cursor done");

    for (i, out) in state.outputs.iter().enumerate().skip(1) {
        let x_offset: i32 = state.outputs[..i].iter().map(|o| o.width as i32).sum();
        state
            .wm
            .add_monitor(out.wl_id, x_offset, 0, out.width as i32, out.height as i32);
    }
    probe!("23: monitors registered");

    // ── XWayland startup (phase 1 — just spawn the process) ──────────────────
    //
    // We cannot block here waiting for Xwayland's display number because
    // Xwayland needs the Wayland event loop to be running in order to complete
    // its own initialisation. Instead we spawn and hand the pipe fd to calloop;
    // finish_start() is called from the event loop once the pipe is readable.
    probe!("24: about to spawn xwayland");
    match state.xwayland.spawn(&socket_name, sw, sh) {
        Ok(pipe_fd) => {
            probe!("25: xwayland spawned, registering pipe fd");
            // We need the raw fd value to capture it in the closure AND to
            // wrap it back into an OwnedFd inside the closure. Using into_raw_fd
            // here gives us a plain integer we can copy into the closure; the
            // OwnedFd is reconstructed (and therefore dropped/closed) inside
            // the one-shot handler after finish_start consumes it.
            let pipe_raw = pipe_fd.into_raw_fd();
            event_loop.handle().insert_source(
                Generic::new(
                    unsafe { calloop::generic::FdWrapper::new(pipe_raw) },
                    Interest::READ,
                    Mode::Edge,
                ),
                move |_, _, state| {
                    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(pipe_raw) };
                    match state.xwayland.finish_start(fd) {
                        Ok(()) => probe!("xwayland: finish_start OK"),
                        Err(e) => tracing::warn!("XWayland finish_start failed: {e}"),
                    }
                    // One-shot — remove this source immediately.
                    Ok(PostAction::Remove)
                },
            )?;
            probe!("26: xwayland pipe fd registered");
        }
        Err(e) => {
            tracing::warn!("XWayland failed to spawn: {e}");
            probe!("24a: xwayland spawn FAILED");
        }
    }

    // ── Wayland display fd ────────────────────────────────────────────────────
    probe!("27: registering wayland display fd");
    {
        let poll_fd = display.backend().poll_fd().as_raw_fd();
        event_loop.handle().insert_source(
            Generic::new(
                unsafe { calloop::generic::FdWrapper::new(poll_fd) },
                Interest::READ,
                Mode::Level,
            ),
            |_, _, _state| Ok(PostAction::Continue),
        )?;
    }
    probe!("28: wayland display fd registered");

    {
        let listener_fd = listener.as_raw_fd();
        event_loop.handle().insert_source(
            Generic::new(
                unsafe { calloop::generic::FdWrapper::new(listener_fd) },
                Interest::READ,
                Mode::Level,
            ),
            move |_, _, state| {
                if let Some(stream) = listener.accept()? {
                    state
                        .display
                        .insert_client(stream, Arc::new(NoopClientData))
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                }
                Ok(PostAction::Continue)
            },
        )?;
    }
    probe!("29: listener fd registered");

    {
        let drm_fd = state.backend.drm.as_raw_fd();
        event_loop.handle().insert_source(
            Generic::new(
                unsafe { calloop::generic::FdWrapper::new(drm_fd) },
                Interest::READ,
                Mode::Level,
            ),
            |_, _, state| {
                state.backend.dispatch_drm_events(|crtc| {
                    for out in &mut state.outputs {
                        if out.render_surf.crtc == crtc {
                            out.frame_pending = false;
                        }
                    }
                    state.needs_redraw = true;
                });
                Ok(PostAction::Continue)
            },
        )?;
    }
    probe!("30: drm fd registered");

    {
        let li_fd = state.input.as_raw_fd();
        event_loop.handle().insert_source(
            Generic::new(
                unsafe { calloop::generic::FdWrapper::new(li_fd) },
                Interest::READ,
                Mode::Level,
            ),
            |_, _, state| {
                input::dispatch_libinput_events(state);
                Ok(PostAction::Continue)
            },
        )?;
    }
    probe!("31: libinput fd registered");

    {
        let seat_fd = state.backend.session.fd;
        event_loop.handle().insert_source(
            Generic::new(
                unsafe { calloop::generic::FdWrapper::new(seat_fd) },
                Interest::READ,
                Mode::Level,
            ),
            |_, _, state| {
                if let Err(e) = state.backend.session.dispatch(0) {
                    tracing::warn!("libseat dispatch: {e}");
                }
                if state.backend.session.take_disable_pending() {
                    tracing::info!("VT switched away — pausing render");
                    for out in &mut state.outputs {
                        out.frame_pending = true;
                    }
                }
                if state.backend.session.take_enable_pending() {
                    tracing::info!("VT returned — resuming render");
                    for out in &mut state.outputs {
                        out.frame_pending = false;
                    }
                    state.needs_redraw = true;
                }
                Ok(PostAction::Continue)
            },
        )?;
    }
    probe!("32: seat fd registered");

    {
        let ipc_fd = state.ipc.as_raw_fd();
        event_loop.handle().insert_source(
            Generic::new(
                unsafe { calloop::generic::FdWrapper::new(ipc_fd) },
                Interest::READ,
                Mode::Edge,
            ),
            |_, _, state| {
                ipc::drain_ipc(state);
                Ok(PostAction::Continue)
            },
        )?;
    }
    probe!("33: ipc fd registered");

    event_loop.handle().insert_source(
        Signals::new(&[Signal::SIGTERM, Signal::SIGINT]).unwrap(),
        |_, _, state| {
            tracing::info!("signal — shutting down");
            state.running.store(false, Ordering::SeqCst);
        },
    )?;
    probe!("34: signal handler registered");

    // ── Main loop ─────────────────────────────────────────────────────────────
    tracing::info!("Axiom running — WAYLAND_DISPLAY={socket_name}");
    probe!("35: entering main loop");

    while running.load(Ordering::SeqCst) {
        event_loop.dispatch(Some(Duration::from_millis(2)), &mut state)?;
        display.dispatch_clients(&mut state)?;

        // If finish_start() completed on this iteration, register the X11 fd.
        if state.xwayland.ready {
            state.xwayland.ready = false;
            if let Some(x11_fd) = state.xwayland.x11_fd() {
                event_loop.handle().insert_source(
                    Generic::new(
                        unsafe { calloop::generic::FdWrapper::new(x11_fd) },
                        Interest::READ,
                        Mode::Level,
                    ),
                    |_, _, state| {
                        let actions = state.xwayland.dispatch_events();
                        dispatch_x11_actions(state, actions);
                        Ok(PostAction::Continue)
                    },
                )?;
                probe!("xwayland: x11 fd registered with calloop");
            } else {
                probe!("xwayland: x11_fd() returned None after finish_start");
            }
        }

        if state.anim.tick() {
            state.needs_redraw = true;
        }
        state.script.tick(&state.wm);

        {
            let actions: Vec<scripting::lua_api::LuaAction> =
                std::mem::take(&mut *state.script.queue.lock().unwrap());
            scripting::lua_api::drain_actions(actions, &mut state);
        }

        if state.needs_redraw {
            render_all(&mut state);
            state.needs_redraw = false;
        }
        if let Err(e) = state.display.flush_clients() {
            tracing::warn!("flush_clients: {e}");
        }
    }

    tracing::info!("Axiom shutdown");
    Ok(())
}

// ── X11 action dispatch ───────────────────────────────────────────────────────

fn dispatch_x11_actions(state: &mut Axiom, actions: Vec<X11Action>) {
    for action in actions {
        match action {
            X11Action::MapWindow {
                x11_win,
                title,
                app_id,
                override_redirect,
                surface_serial,
            } => {
                if !override_redirect {
                    state.try_pair_from_x11(x11_win, title, app_id, surface_serial);
                }
            }

            X11Action::UnmapWindow { x11_win } => {
                state.unpair_x11_window(x11_win);
            }

            X11Action::TitleChanged { x11_win, title } => {
                if let Some(&win_id) = state.xwayland.x11_to_wl.get(&x11_win) {
                    state.wm.set_title(win_id, title);
                    state.needs_redraw = true;
                }
            }

            X11Action::FocusRequest { x11_win } => {
                if let Some(&win_id) = state.xwayland.x11_to_wl.get(&x11_win) {
                    state.wm.focus_window(win_id);
                    state.xwayland.set_focus(x11_win);
                    state.sync_keyboard_focus();
                    state.needs_redraw = true;
                }
            }

            X11Action::ConfigureRequest {
                x11_win,
                x,
                y,
                w,
                h,
            } => {
                if let Some(&win_id) = state.xwayland.x11_to_wl.get(&x11_win) {
                    let r = state.wm.windows.get(&win_id).map(|w| w.rect);
                    if let Some(r) = r {
                        state
                            .xwayland
                            .configure_window(x11_win, r.x, r.y, r.w as u32, r.h as u32);
                    }
                } else {
                    state.xwayland.configure_window(
                        x11_win,
                        x.unwrap_or(0),
                        y.unwrap_or(0),
                        w.unwrap_or(320),
                        h.unwrap_or(240),
                    );
                }
            }
        }
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

fn render_all(state: &mut Axiom) {
    let n = state.outputs.len();
    let cx = state.input.pointer_x as i32;
    let cy = state.input.pointer_y as i32;

    for idx in 0..n {
        if state.outputs[idx].frame_pending {
            continue;
        }

        let surf_ptr = &state.outputs[idx].render_surf as *const backend::OutputSurface;
        let surf = unsafe { &*surf_ptr };

        if surf.make_current(&state.backend.egl).is_err() {
            continue;
        }

        if let Some(ref hw) = state.render.hw_cursor {
            hw.move_on_crtc(&state.backend.drm, surf.crtc, cx, cy);
        }

        state.render.render_output(
            &state.wm,
            &state.anim,
            &state.input,
            &state.outputs,
            &state.layer_surfaces,
            surf,
            idx,
        );

        let surf_mut = &mut state.outputs[idx].render_surf;
        let fb_id = match surf_mut.present(&state.backend.egl, &state.backend.drm) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("present: {e}");
                continue;
            }
        };
        if let Err(e) = surf_mut.page_flip(&state.backend.drm, fb_id) {
            tracing::warn!("page_flip: {e}");
        } else {
            state.outputs[idx].frame_pending = true;
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn find_primary_gpu() -> Result<PathBuf> {
    for n in 0..8u32 {
        let p = PathBuf::from(format!("/dev/dri/card{n}"));
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("no DRI card found")
}

fn xdg_config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into())).join(".config")
        });
    base.join("axiom")
}

fn primary_output_size(outputs: &[backend::OutputSurface]) -> (i32, i32) {
    outputs
        .first()
        .map(|o| {
            let (w, h) = o.mode_size();
            (w as i32, h as i32)
        })
        .unwrap_or((1920, 1080))
}
