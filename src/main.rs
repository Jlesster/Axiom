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
    os::unix::io::AsRawFd,
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

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(std::env::var("AXIOM_LOG").unwrap_or_else(|_| "axiom=debug,warn".into()))
        .init();
    tracing::info!("Axiom starting");

    let mut event_loop: EventLoop<'static, Axiom> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    let session = backend::session::Session::open()?;

    let gpu_path = find_primary_gpu()?;
    tracing::info!("GPU: {gpu_path:?}");
    let mut backend = Backend::open(&gpu_path, session)?;
    let outputs_raw = backend.create_outputs()?;

    if let Some(out) = outputs_raw.first() {
        out.make_current(&backend.egl)?;
    }
    let render = RenderState::new()?;

    let mut display: Display<Axiom> = Display::new()?;
    let display_handle = display.handle();

    let socket_name = "wayland-axiom".to_string();
    let listener =
        wayland_server::ListeningSocket::bind(&socket_name).context("bind Wayland socket")?;
    tracing::info!("WAYLAND_DISPLAY={socket_name}");
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    }

    proto::register_globals(&display_handle);

    let (sw, sh) = primary_output_size(&outputs_raw);
    let mut wm = WmState::new(sw, sh, WmConfig::default());

    let config_dir = xdg_config_dir();
    let script = ScriptEngine::new(&config_dir, &wm)?;
    script.run_rc(&mut wm)?;

    let input = InputState::new(&backend.session)?;
    let seat = SeatState::new();

    // ── IPC server ────────────────────────────────────────────────────────────
    let ipc = ipc::IpcServer::bind(&socket_name)?;

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

    for (i, out) in state.outputs.iter().enumerate().skip(1) {
        let x_offset: i32 = state.outputs[..i].iter().map(|o| o.width as i32).sum();
        state
            .wm
            .add_monitor(out.wl_id, x_offset, 0, out.width as i32, out.height as i32);
    }

    // ── Event sources ─────────────────────────────────────────────────────────
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
    {
        let ipc_fd = state.ipc.as_raw_fd();
        event_loop.handle().insert_source(
            Generic::new(
                unsafe { calloop::generic::FdWrapper::new(ipc_fd) },
                Interest::READ,
                Mode::Level,
            ),
            |_, _, state| {
                ipc::drain_ipc(state);
                Ok(PostAction::Continue)
            },
        )?;
    }
    event_loop.handle().insert_source(
        Signals::new(&[Signal::SIGTERM, Signal::SIGINT]).unwrap(),
        |_, _, state| {
            tracing::info!("signal — shutting down");
            state.running.store(false, Ordering::SeqCst);
        },
    )?;

    // ── Main loop ─────────────────────────────────────────────────────────────
    tracing::info!("Axiom running — WAYLAND_DISPLAY={socket_name}");
    state.script.emit_bare("compositor.ready");

    while running.load(Ordering::SeqCst) {
        event_loop.dispatch(Some(Duration::from_millis(2)), &mut state)?;
        display.dispatch_clients(&mut state)?;

        if state.anim.tick() {
            state.needs_redraw = true;
        }
        state.script.tick(&state.wm);

        // Drain script actions: pull the queue out first to avoid a
        // simultaneous mutable borrow of both state.script and state.
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
