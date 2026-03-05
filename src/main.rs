// main.rs — Trixie compositor entry point.

mod backend;
mod chrome;
mod config;
mod cursor;
mod handlers;
mod input;
mod ipc;
mod render;
mod session;
mod signal;
mod state;
mod twm;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use notify::{EventKind, RecursiveMode, Watcher};
use smithay::backend::drm::NodeType;
use smithay::desktop::{PopupManager, Space};
use smithay::input::pointer::CursorImageStatus;
use smithay::{
    backend::{
        drm::DrmNode,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::ImportDma,
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    input::{keyboard::XkbConfig, SeatState},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, Mode as CalloopMode, PostAction},
        input::Libinput,
        wayland_server::Display as WlDisplay,
    },
    utils::Clock,
    wayland::{
        compositor::CompositorState,
        dmabuf::DmabufState,
        output::OutputManagerState,
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use config::{BarPosition, Config};
use cursor::CursorManager;
use state::{DragState, Trixie};
use twm::TwmState;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(|| -> Box<dyn std::io::Write> { Box::new(std::io::stderr()) })
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tracing::info!("Trixie starting");

    unsafe {
        std::env::set_var("GBM_BACKEND", "nvidia-drm");
        std::env::set_var("__GLX_VENDOR_LIBRARY_NAME", "nvidia");
        std::env::set_var("__GL_SYNC_TO_VBLANK", "0");
    }

    let config = Arc::new(Config::load());
    tracing::info!("Config loaded from {:?}", config::config_path());

    let mut event_loop: EventLoop<'static, Trixie> = EventLoop::try_new().unwrap();
    let display: WlDisplay<Trixie> = WlDisplay::new().unwrap();
    let dh = display.handle();

    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                unsafe { display.get_mut().dispatch_clients(state).unwrap() };
                Ok(PostAction::Continue)
            },
        )
        .unwrap();

    let source = ListeningSocketSource::new_auto().unwrap();
    let socket_name = source.socket_name().to_string_lossy().into_owned();
    event_loop
        .handle()
        .insert_source(source, |stream, _, state| {
            state
                .display_handle
                .insert_client(stream, Arc::new(state::ClientState::default()))
                .unwrap();
        })
        .unwrap();

    let (session, notifier) = LibSeatSession::new().expect("libseat session");
    event_loop
        .handle()
        .insert_source(notifier, |event, _, state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("Session paused");
                state.libinput.suspend();
                for b in state.backends.values_mut() {
                    b.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("Session resumed");
                let _ = state
                    .libinput
                    .udev_assign_seat(state.session.seat().as_str());
                for b in state.backends.values_mut() {
                    let _ = b.drm.activate(false);
                }
                state.handle.insert_idle(|s| s.render_all());
            }
        })
        .unwrap();

    let primary_gpu = primary_gpu(session.seat())
        .unwrap()
        .and_then(|p| DrmNode::from_path(p).ok())
        .and_then(|n| n.node_with_type(NodeType::Render).and_then(|n| n.ok()))
        .unwrap_or_else(|| {
            all_gpus(session.seat())
                .unwrap()
                .into_iter()
                .find_map(|p| DrmNode::from_path(p).ok())
                .expect("No GPU found")
        });
    tracing::info!("Primary GPU: {primary_gpu}");

    let mut libinput_ctx =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput_ctx
        .udev_assign_seat(session.seat().as_str())
        .unwrap();

    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, &config.seat_name);
    let pointer = seat.add_pointer();
    seat.add_keyboard(
        XkbConfig {
            layout: config.keyboard.layout.as_deref().unwrap_or(""),
            variant: config.keyboard.variant.as_deref().unwrap_or(""),
            options: config.keyboard.options.clone(),
            ..XkbConfig::default()
        },
        config.keyboard.repeat_delay as i32,
        config.keyboard.repeat_rate as i32,
    )
    .unwrap();

    let bar_h = config.bar.height;
    let at_bottom = config.bar.position == BarPosition::Bottom;
    let twm = TwmState::new(
        0,
        0,
        bar_h,
        at_bottom,
        config.gap,
        config.border_width,
        12,
        config.workspaces,
    );

    // Build CursorManager — theme is loaded later in backend.rs once the GL
    // context is current and the cursor theme path is resolvable.
    let mut cursor = CursorManager::default();
    if let Some(theme) = config.cursor_theme.as_deref() {
        cursor.theme_name = theme.to_string();
    }

    let mut state = Trixie {
        display_handle: dh.clone(),
        compositor_state: CompositorState::new::<Trixie>(&dh),
        shm_state: ShmState::new::<Trixie>(&dh, vec![]),
        dmabuf_state: DmabufState::new(),
        dmabuf_global: None,
        output_manager_state: OutputManagerState::new_with_xdg_output::<Trixie>(&dh),
        seat_state,
        data_device_state: DataDeviceState::new::<Trixie>(&dh),
        primary_selection_state: PrimarySelectionState::new::<Trixie>(&dh),
        xdg_shell_state: XdgShellState::new::<Trixie>(&dh),
        layer_shell_state: WlrLayerShellState::new::<Trixie>(&dh),
        xdg_decoration_state: XdgDecorationState::new::<Trixie>(&dh),
        popups: PopupManager::default(),
        space: Space::default(),
        surface_to_pane: HashMap::new(),
        seat,
        pointer,
        cursor_status: CursorImageStatus::default_named(),
        cursor,
        mouse_mode: state::MouseMode::Normal,
        drag: DragState::None,
        libinput: libinput_ctx,
        session,
        backends: HashMap::new(),
        primary_gpu,
        wayland_socket: socket_name.clone(),
        twm,
        unclaimed: HashMap::new(),
        config: Arc::clone(&config),
        running: Arc::new(AtomicBool::new(true)),
        handle: event_loop.handle(),
        clock: Clock::new(),
        start_time: std::time::Instant::now(),
        exec_once_done: false,
        anim: twm::anim::AnimSet::default(),
        needs_redraw: true,
        de: None,
    };

    // ── Register scratchpads from config ──────────────────────────────────────
    for sp in &config.scratchpads.clone() {
        state
            .twm
            .register_scratchpad_sized(&sp.name, &sp.app_id, sp.width_pct, sp.height_pct);
        tracing::info!(
            "Registered scratchpad '{}' for app_id='{}' ({:.0}%x{:.0}%)",
            sp.name,
            sp.app_id,
            sp.width_pct * 100.0,
            sp.height_pct * 100.0,
        );
    }

    let udev_backend = UdevBackend::new(state.session.seat()).unwrap();
    for (dev_id, path) in udev_backend.device_list() {
        let Ok(node) = smithay::backend::drm::DrmNode::from_dev_id(dev_id) else {
            continue;
        };
        if let Err(e) = backend::add_gpu(&mut state, &dh, node, path) {
            tracing::warn!("GPU {node} skipped: {e}");
        }
    }

    if let Some(b) = state.backends.get(&state.primary_gpu) {
        let formats: Vec<_> = b.renderer.dmabuf_formats().iter().copied().collect();
        let global = state.dmabuf_state.create_global::<Trixie>(&dh, formats);
        state.dmabuf_global = Some(global);
    }

    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, state| match event {
            UdevEvent::Added { device_id, path } => {
                if let Ok(node) = smithay::backend::drm::DrmNode::from_dev_id(device_id) {
                    let _ = backend::add_gpu(state, &state.display_handle.clone(), node, &path);
                }
            }
            UdevEvent::Changed { .. } => {}
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = smithay::backend::drm::DrmNode::from_dev_id(device_id) {
                    state.backends.remove(&node);
                }
            }
        })
        .unwrap();

    event_loop
        .handle()
        .insert_source(
            LibinputInputBackend::new(state.libinput.clone()),
            |event, _, state| input::handle_input(state, event),
        )
        .unwrap();

    state.run_exec_once();
    state.run_exec();
    tracing::info!("WAYLAND_DISPLAY={socket_name}");

    let cfg_dir = config::config_dir();
    let (reload_tx, reload_rx) = mpsc::channel::<()>();
    let mut watcher = {
        let tx = reload_tx.clone();
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            let is_write = matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_));
            let is_conf = ev
                .paths
                .iter()
                .any(|p| p.extension().and_then(|e| e.to_str()) == Some("conf"));
            if is_write && is_conf {
                let _ = tx.send(());
            }
        })
    }
    .expect("file watcher");

    if cfg_dir.exists() {
        let _ = watcher.watch(&cfg_dir, RecursiveMode::Recursive);
        tracing::info!("Watching {:?} for config changes", cfg_dir);
    }

    let running = state.running.clone();
    let mut display_handle = state.display_handle.clone();

    while running.load(Ordering::SeqCst) {
        let _ = event_loop.dispatch(Some(Duration::from_millis(16)), &mut state);
        if reload_rx.try_recv().is_ok() {
            while reload_rx.try_recv().is_ok() {}
            state.apply_config_reload();
        }
        state.space.refresh();
        state.popups.cleanup();
        if let Err(e) = display_handle.flush_clients() {
            tracing::warn!("flush_clients: {e}");
        }
    }

    tracing::info!("Trixie shutting down");

    drop(display_handle);
    state.libinput.suspend();
    for backend in state.backends.values_mut() {
        backend.drm.pause();
    }
    drop(state);

    tracing::info!("Trixie exited cleanly");
}
