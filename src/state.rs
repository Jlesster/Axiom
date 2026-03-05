// state.rs — Trixie root compositor state.

use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::gbm::{GbmAllocator, GbmDevice},
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDevice,
            DrmDeviceFd, DrmNode,
        },
        renderer::{
            element::{
                solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement,
                AsRenderElements,
            },
            gles::GlesRenderer,
        },
        session::libseat::LibSeatSession,
    },
    desktop::{PopupManager, Space, Window},
    input::{
        pointer::{CursorImageStatus, PointerHandle},
        Seat, SeatState,
    },
    output::Output,
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            LoopHandle,
        },
        drm::control::crtc,
        input::Libinput,
        wayland_server::{
            backend::{ClientId, ObjectId},
            DisplayHandle, Resource,
        },
    },
    utils::{Clock, Monotonic, SERIAL_COUNTER as SCOUNTER},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufGlobal, DmabufState},
        seat::WaylandFocus,
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, ToplevelSurface, XdgShellState},
        },
        shm::ShmState,
    },
};

use trixui::smithay::SmithayApp;

use crate::{
    chrome::{ChromeApp, ChromeMsg},
    config::Config,
    twm::{anim::AnimSet, PaneId, TwmState},
};

// ── Type alias ────────────────────────────────────────────────────────────────

pub type GbmDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

// ── Render elements ───────────────────────────────────────────────────────────

smithay::backend::renderer::element::render_elements! {
    pub TrixieElement<=GlesRenderer>;
    Space  = WaylandSurfaceRenderElement<GlesRenderer>,
    Cursor = SolidColorRenderElement,
}

// ── Per-output surface data ───────────────────────────────────────────────────

pub struct SurfaceData {
    pub output: Output,
    pub compositor: GbmDrmCompositor,
    /// Retained for informational use only.
    pub next_frame_time: Instant,
    /// True from the moment a frame is queued until vblank fires.
    /// render_surface will not submit a new frame while this is set,
    /// preventing frame queue overflow on NVIDIA and other DRM drivers.
    pub pending_frame: bool,
    pub frame_duration: Duration,
}

// ── Per-GPU backend data ──────────────────────────────────────────────────────

pub struct BackendData {
    pub surfaces: HashMap<crtc::Handle, SurfaceData>,
    pub renderer: GlesRenderer,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub drm: DrmDevice,
    pub drm_node: DrmNode,
}

// ── Client state ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor: smithay::wayland::compositor::CompositorClientState,
}

impl smithay::reexports::wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(
        &self,
        _: ClientId,
        _: smithay::reexports::wayland_server::backend::DisconnectReason,
    ) {
    }
}

// ── Mouse mode ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    Normal,
    Passthrough,
}

// ── Main compositor state ─────────────────────────────────────────────────────

pub struct Trixie {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub output_manager_state: smithay::wayland::output::OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub popups: PopupManager,
    pub space: Space<Window>,

    /// Maps a Wayland surface ObjectId → TWM PaneId.
    pub surface_to_pane: HashMap<ObjectId, PaneId>,

    pub seat: Seat<Self>,
    pub pointer: PointerHandle<Self>,
    pub cursor_status: CursorImageStatus,
    pub mouse_mode: MouseMode,
    pub libinput: Libinput,

    pub session: LibSeatSession,
    pub backends: HashMap<DrmNode, BackendData>,
    pub primary_gpu: DrmNode,
    pub wayland_socket: String,

    pub twm: TwmState,
    pub anim: AnimSet,
    pub unclaimed: HashMap<ObjectId, ToplevelSurface>,

    pub ui: Option<SmithayApp<ChromeApp>>,

    pub config: Arc<Config>,

    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, Self>,
    pub clock: Clock<Monotonic>,
    pub start_time: Instant,
    pub exec_once_done: bool,

    /// Set to true by any event that causes visible change:
    /// client buffer commit, cursor move, window open/close, config reload,
    /// animation tick, key binding that changes layout/focus.
    /// Cleared in frame_finish after the next frame is scheduled.
    /// This prevents the render loop from running at full refresh rate
    /// when nothing has changed, fixing sluggishness and NVIDIA flicker.
    pub needs_redraw: bool,
}

// ── Render ────────────────────────────────────────────────────────────────────

impl Trixie {
    pub fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    pub fn render_all(&mut self) {
        let nodes: Vec<DrmNode> = self.backends.keys().copied().collect();
        for node in nodes {
            let crtcs: Vec<crtc::Handle> = self.backends[&node].surfaces.keys().copied().collect();
            for crtc in crtcs {
                self.render_surface(node, crtc);
            }
        }
    }

    pub fn render_surface(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let surface = match self.backends.get(&node).and_then(|b| b.surfaces.get(&crtc)) {
            Some(s) => s,
            None => return,
        };

        // Do not submit a new frame while the previous one is still in-flight.
        if surface.pending_frame {
            return;
        }

        let frame_duration = surface.frame_duration;

        // Advance animations. If any are still running, keep needs_redraw set
        // so frame_finish schedules another frame after vblank.
        let still_animating = self.anim.tick();
        if still_animating {
            self.needs_redraw = true;
        }

        let snap = self.twm.snapshot();
        let chrome_cmds = if let Some(ui) = &mut self.ui {
            ui.send(ChromeMsg::Snapshot(snap));
            ui.collect()
        } else {
            vec![]
        };

        let queued = crate::render::render(self, node, crtc, chrome_cmds);

        if queued {
            // render.rs sets pending_frame = true inside queue_frame on success.
            // We only update next_frame_time here for informational purposes.
            if let Some(b) = self.backends.get_mut(&node) {
                if let Some(s) = b.surfaces.get_mut(&crtc) {
                    s.next_frame_time = Instant::now() + s.frame_duration;
                }
            }
        } else {
            // render() returned false — DRM not ready or nothing to draw yet.
            // Schedule a one-shot retry after one frame interval.
            self.handle
                .insert_source(Timer::from_duration(frame_duration), move |_, _, state| {
                    state.render_surface(node, crtc);
                    TimeoutAction::Drop
                })
                .ok();
        }
    }

    pub fn frame_finish(&mut self, node: DrmNode, crtc: crtc::Handle) {
        if let Some(b) = self.backends.get_mut(&node) {
            if let Some(s) = b.surfaces.get_mut(&crtc) {
                // Vblank fired — the frame is on screen. Clear the guard so the
                // next render_surface call can submit a new frame.
                s.pending_frame = false;
                if let Err(e) = s.compositor.frame_submitted() {
                    tracing::warn!("frame_submitted: {e}");
                }
            }
        }

        // Always schedule the next frame after vblank. The render loop runs
        // continuously; pending_frame is the sole guard against frame queue
        // overflow (render_surface returns early if pending_frame is set).
        //
        // The open-window seizure/flash was caused by reset_buffers() being
        // called every frame in render.rs (now removed), not by the loop being
        // unconditional. Demand-driven gating here caused the loop to die on
        // the first idle frame because trixui's chrome path doesn't re-signal
        // needs_redraw on every tick.
        self.needs_redraw = false;
        self.handle
            .insert_idle(move |s| s.render_surface(node, crtc));
    }

    // ── Focus sync ────────────────────────────────────────────────────────────

    pub fn sync_focus(&mut self) {
        let focused_pane_id = self.twm.workspaces[self.twm.active_ws].focused;

        let focused_surf: Option<
            smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        > = focused_pane_id.and_then(|target_pid| {
            let obj_id = self.surface_to_pane.iter().find_map(|(oid, &pid)| {
                if pid == target_pid {
                    Some(oid.clone())
                } else {
                    None
                }
            })?;
            self.space.elements().find_map(|w| {
                let surf = w.wl_surface()?;
                if surf.as_ref().id() == obj_id {
                    Some(surf.into_owned())
                } else {
                    None
                }
            })
        });

        if let Some(surf) = focused_surf {
            let serial = SCOUNTER.next_serial();
            if let Some(kbd) = self.seat.get_keyboard() {
                kbd.set_focus(self, Some(surf.clone()), serial);
            }
            if let Some(ptr) = self.seat.get_pointer() {
                let loc = ptr.current_location();
                let bw = self.twm.border_w;
                let surf_local = focused_pane_id
                    .and_then(|pid| self.twm.panes.get(&pid))
                    .map(|pane| {
                        let inner = if pane.fullscreen || bw == 0 {
                            pane.rect
                        } else {
                            pane.rect.inset(bw)
                        };
                        smithay::utils::Point::<f64, smithay::utils::Logical>::from((
                            loc.x - inner.x as f64,
                            loc.y - inner.y as f64,
                        ))
                    })
                    .unwrap_or(loc);

                let serial2 = SCOUNTER.next_serial();
                ptr.motion(
                    self,
                    Some((surf, surf_local)),
                    &smithay::input::pointer::MotionEvent {
                        location: loc,
                        serial: serial2,
                        time: self.clock.now().as_millis(),
                    },
                );
                ptr.frame(self);
            }
        }
    }

    // ── Config reload ─────────────────────────────────────────────────────────

    pub fn apply_config_reload(&mut self) {
        let mut new_cfg = (*self.config).clone();
        new_cfg.hot_reload();
        let new_arc = Arc::new(new_cfg);
        self.config = Arc::clone(&new_arc);

        self.twm.gap = self.config.gap;
        self.twm.border_w = self.config.border_width;

        let config_bar_h = self.config.bar.height;
        let at_bottom = self.config.bar.position == crate::config::BarPosition::Bottom;

        let actual_bar_h = if let Some(ui) = &mut self.ui {
            ui.set_bar_height_px(config_bar_h);
            let cell_h = ui.line_h();
            if cell_h == 0 {
                config_bar_h
            } else {
                ((config_bar_h + cell_h - 1) / cell_h) * cell_h
            }
        } else {
            config_bar_h
        };

        self.twm.set_bar_height(actual_bar_h, at_bottom);

        if let Some(ui) = &mut self.ui {
            ui.send(ChromeMsg::ConfigReloaded(new_arc));
        }

        if let Some(kbd) = self.seat.get_keyboard() {
            kbd.change_repeat_info(
                self.config.keyboard.repeat_rate as i32,
                self.config.keyboard.repeat_delay as i32,
            );
        }

        self.needs_redraw = true;
        tracing::info!("Config reloaded");
    }

    // ── Spawn ─────────────────────────────────────────────────────────────────

    pub fn spawn(&self, cmd: &str, args: &[String]) {
        let bin = expand_tilde(cmd);
        tracing::info!("spawn: {bin} {args:?}");
        if let Err(e) = std::process::Command::new(&bin)
            .args(args)
            .env("WAYLAND_DISPLAY", &self.wayland_socket)
            .spawn()
        {
            tracing::warn!("spawn failed ({bin}): {e}");
        }
    }

    pub fn run_exec_once(&mut self) {
        if self.exec_once_done {
            return;
        }
        self.exec_once_done = true;
        let entries = self.config.exec_once.clone();
        for e in &entries {
            self.spawn(&e.command, &e.args);
        }
    }

    pub fn run_exec(&self) {
        let entries = self.config.exec.clone();
        for e in &entries {
            self.spawn(&e.command, &e.args);
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    s.to_string()
}
