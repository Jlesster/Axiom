// handlers.rs — Smithay protocol delegate implementations.

use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_layer_shell,
    delegate_output, delegate_primary_selection, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{layer_map_for_output, PopupKind, PopupManager, Window},
    input::{Seat, SeatHandler, SeatState},
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_output, wl_seat, wl_surface::WlSurface},
            Client, Resource,
        },
    },
    utils::SERIAL_COUNTER as SCOUNTER,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_parent, is_sync_subsurface, with_states, CompositorClientState, CompositorHandler,
            CompositorState,
        },
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        seat::WaylandFocus,
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            SelectionHandler,
        },
        shell::{
            wlr_layer::{Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState},
            xdg::{
                decoration::XdgDecorationHandler, PopupSurface, PositionerState, ToplevelSurface,
                XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
            },
        },
        shm::{ShmHandler, ShmState},
    },
};

use smithay::backend::renderer::{utils::on_commit_buffer_handler, ImportDma};
use smithay::input::pointer::CursorImageStatus;

use crate::state::{ClientState, Trixie};

// ── dmabuf ────────────────────────────────────────────────────────────────────

impl DmabufHandler for Trixie {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }
    fn dmabuf_imported(
        &mut self,
        _: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        let ok = self
            .backends
            .get_mut(&self.primary_gpu)
            .map(|b| b.renderer.import_dmabuf(&dmabuf, None).is_ok())
            .unwrap_or(false);
        if ok {
            let _ = notifier.successful::<Trixie>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(Trixie);

// ── shm / buffer ──────────────────────────────────────────────────────────────

impl BufferHandler for Trixie {
    fn buffer_destroyed(&mut self, _: &WlBuffer) {}
}
impl ShmHandler for Trixie {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(Trixie);

// ── compositor ────────────────────────────────────────────────────────────────

impl CompositorHandler for Trixie {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        // Every client commit means new pixel content — always request a redraw.
        self.needs_redraw = true;

        // Claim surfaces that arrived without app_id at new_toplevel time.
        let obj_id = surface.id();
        if self.unclaimed.contains_key(&obj_id) {
            let app_id = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok())
                    .and_then(|l| l.app_id.clone())
            })
            .unwrap_or_default();

            if !app_id.is_empty() {
                tracing::info!("commit: claiming unclaimed surface app_id={app_id:?}");
                self.unclaimed.remove(&obj_id);
                let pane_id = self.twm.open_shell(&app_id);
                tracing::info!(
                    "commit unclaimed: open_shell id={pane_id} panes={}",
                    self.twm.panes.len()
                );
                self.surface_to_pane.insert(obj_id.clone(), pane_id);
                tracing::info!(
                    "commit unclaimed: surface_to_pane now has {} entries",
                    self.surface_to_pane.len()
                );
                self.surface_to_pane.insert(obj_id.clone(), pane_id);

                // Kick open animation for the newly claimed pane.
                if let Some(pane) = self.twm.panes.get(&pane_id) {
                    self.anim.open(pane_id, pane.rect);
                }

                if let Some(pane) = self.twm.panes.get(&pane_id) {
                    let bw = self.twm.border_w;
                    let inner = if bw == 0 {
                        pane.rect
                    } else {
                        pane.rect.inset(bw)
                    };
                    let loc = smithay::utils::Point::from((inner.x as i32, inner.y as i32));
                    let new_size = smithay::utils::Size::from((inner.w as i32, inner.h as i32));

                    let window = self
                        .space
                        .elements()
                        .find(|w| {
                            w.wl_surface()
                                .map(|s| s.as_ref().id() == obj_id)
                                .unwrap_or(false)
                        })
                        .cloned();

                    if let Some(window) = window {
                        self.space.map_element(window.clone(), loc, true);

                        if let Some(toplevel) = window.toplevel() {
                            let already_pending =
                                toplevel.with_pending_state(|s| s.size == Some(new_size));
                            if !already_pending {
                                toplevel.with_pending_state(|s| s.size = Some(new_size));
                                toplevel.send_configure();
                            }
                        }

                        if let Some(surf) = window.wl_surface().map(|s| s.into_owned()) {
                            let serial = SCOUNTER.next_serial();
                            if let Some(kbd) = self.seat.get_keyboard() {
                                kbd.set_focus(self, Some(surf), serial);
                            }
                        }
                    }
                }
                self.sync_focus();
            }
        }

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(p) = get_parent(&root) {
                root = p;
            }
            if let Some(w) = self
                .space
                .elements()
                .find(|w| w.wl_surface().as_deref() == Some(&root))
                .cloned()
            {
                w.on_commit();
            }
        }

        self.popups.commit(surface);
        ensure_initial_configure(surface, &self.space, &mut self.popups);
    }
}
delegate_compositor!(Trixie);

// ── selection ─────────────────────────────────────────────────────────────────

impl SelectionHandler for Trixie {
    type SelectionUserData = ();
}
impl ClientDndGrabHandler for Trixie {}
impl ServerDndGrabHandler for Trixie {}
impl DataDeviceHandler for Trixie {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
delegate_data_device!(Trixie);

impl PrimarySelectionHandler for Trixie {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}
delegate_primary_selection!(Trixie);

// ── output ────────────────────────────────────────────────────────────────────

impl OutputHandler for Trixie {}
delegate_output!(Trixie);

// ── seat ──────────────────────────────────────────────────────────────────────

impl SeatHandler for Trixie {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let focus = target.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, focus.clone());
        set_primary_focus(dh, seat, focus);
    }

    fn cursor_image(&mut self, _: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        // Cursor shape changed — need to repaint.
        self.needs_redraw = true;
    }
}
delegate_seat!(Trixie);

// ── wlr layer shell ───────────────────────────────────────────────────────────

impl WlrLayerShellHandler for Trixie {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = output
            .as_ref()
            .and_then(|o| self.space.outputs().find(|op| op.owns(o)).cloned())
            .or_else(|| self.space.outputs().next().cloned());
        if let Some(output) = output {
            let mut map = layer_map_for_output(&output);
            map.map_layer(&smithay::desktop::LayerSurface::new(surface, namespace))
                .ok();
        }
        self.needs_redraw = true;
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let wl = surface.wl_surface().clone();
        let output = self
            .space
            .outputs()
            .find(|o| {
                layer_map_for_output(o)
                    .layers()
                    .any(|l| l.wl_surface() == &wl)
            })
            .cloned();
        if let Some(output) = output {
            let mut map = layer_map_for_output(&output);
            let to_rm: Vec<_> = map
                .layers()
                .filter(|l| l.wl_surface() == &wl)
                .cloned()
                .collect();
            for l in to_rm {
                map.unmap_layer(&l);
            }
        }
        self.needs_redraw = true;
    }
}
delegate_layer_shell!(Trixie);

// ── xdg shell ─────────────────────────────────────────────────────────────────

impl XdgShellHandler for Trixie {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::info!(
            "new_toplevel: after open_shell twm.panes.len()={}",
            self.twm.panes.len()
        );
        let surf_id = surface.wl_surface().id();

        let app_id = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok())
                .and_then(|l| l.app_id.clone())
        })
        .unwrap_or_default();

        tracing::info!("new_toplevel: app_id={app_id:?}");

        // Apply window rules.
        let rules = self.config.window_rules.clone();
        let mut float = false;
        let mut forced_size: Option<(u32, u32)> = None;
        for rule in &rules {
            if rule.matcher.matches(&app_id, "") {
                use crate::config::RuleEffect::*;
                for effect in &rule.effects {
                    match effect {
                        Float => float = true,
                        Size(w, h) => forced_size = Some((*w, *h)),
                        _ => {}
                    }
                }
            }
        }

        let pane_id = if !app_id.is_empty() {
            let id = self.twm.open_shell(&app_id);
            self.surface_to_pane.insert(surf_id.clone(), id);
            if float {
                if let Some(pane) = self.twm.panes.get_mut(&id) {
                    pane.floating = true;
                }
            }
            // Kick open animation immediately for panes with known rects.
            if let Some(pane) = self.twm.panes.get(&id) {
                self.anim.open(id, pane.rect);
            }
            Some(id)
        } else {
            tracing::info!("new_toplevel: app_id empty, parking as unclaimed");
            self.unclaimed.insert(surf_id, surface.clone());
            None
        };

        let configure_size = forced_size
            .map(|(w, h)| smithay::utils::Size::from((w as i32, h as i32)))
            .or_else(|| {
                pane_id.and_then(|id| self.twm.panes.get(&id)).map(|pane| {
                    let inner = if self.twm.border_w == 0 {
                        pane.rect
                    } else {
                        pane.rect.inset(self.twm.border_w)
                    };
                    smithay::utils::Size::from((inner.w as i32, inner.h as i32))
                })
            })
            .unwrap_or_else(|| {
                self.space
                    .outputs()
                    .next()
                    .and_then(|o| self.space.output_geometry(o))
                    .map(|g| g.size)
                    .unwrap_or_else(|| smithay::utils::Size::from((1920, 1080)))
            });

        surface.with_pending_state(|s| {
            s.size = Some(configure_size);
            s.decoration_mode = Some(
                smithay::reexports::wayland_protocols::xdg::decoration::zv1::server
                    ::zxdg_toplevel_decoration_v1::Mode::ServerSide,
            );
        });
        surface.send_configure();

        let window = Window::new_wayland_window(surface.clone());
        let loc = pane_id
            .and_then(|id| self.twm.panes.get(&id))
            .map(|pane| {
                let inner = if self.twm.border_w == 0 {
                    pane.rect
                } else {
                    pane.rect.inset(self.twm.border_w)
                };
                (inner.x as i32, inner.y as i32)
            })
            .unwrap_or((0, 0));

        self.space.map_element(window.clone(), loc, true);

        if pane_id.is_some() {
            if let Some(surf) = window.wl_surface().map(|s| s.into_owned()) {
                let serial = SCOUNTER.next_serial();
                if let Some(kbd) = self.seat.get_keyboard() {
                    kbd.set_focus(self, Some(surf), serial);
                }
            }
        }

        self.needs_redraw = true;
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let surf_id = surface.wl_surface().id();

        self.unclaimed.remove(&surf_id);

        if let Some(pane_id) = self.surface_to_pane.remove(&surf_id) {
            tracing::info!("toplevel_destroyed: surf={surf_id:?} -> pane={pane_id}");

            // Play close animation then defer actual close by 150 ms.
            if let Some(pane) = self.twm.panes.get(&pane_id) {
                self.anim.close(pane_id, pane.rect);
            }

            let handle = self.handle.clone();
            handle
                .insert_source(
                    smithay::reexports::calloop::timer::Timer::from_duration(
                        std::time::Duration::from_millis(150),
                    ),
                    move |_, _, state: &mut Trixie| {
                        state.twm.close_pane(pane_id);
                        state.needs_redraw = true;
                        smithay::reexports::calloop::timer::TimeoutAction::Drop
                    },
                )
                .ok();
        } else {
            // No surface_to_pane entry. This can happen for unclaimed surfaces
            // that never got an app_id. Only close panes by app_id if no live
            // surface_to_pane entry maps to a pane with that app_id — otherwise
            // we would nuke panes that are still displayed.
            let app_id = with_states(surface.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok())
                    .and_then(|l| l.app_id.clone())
            })
            .unwrap_or_default();

            tracing::info!(
                "toplevel_destroyed: surf={surf_id:?} has no pane mapping, app_id={app_id:?}"
            );

            if !app_id.is_empty() {
                // Collect pane IDs for this app_id that are NOT referenced by
                // any live surface_to_pane entry (i.e. truly orphaned panes).
                let live_pane_ids: std::collections::HashSet<_> =
                    self.surface_to_pane.values().copied().collect();

                let orphaned: Vec<_> = self
                    .twm
                    .panes
                    .values()
                    .filter(|p| p.content.app_id() == app_id && !live_pane_ids.contains(&p.id))
                    .map(|p| p.id)
                    .collect();

                for pane_id in orphaned {
                    tracing::info!("toplevel_destroyed: closing orphaned pane {pane_id}");
                    self.twm.close_pane(pane_id);
                }
            }
        }

        // Hand focus to the next available window.
        let next_surf: Option<WlSurface> = self
            .space
            .elements()
            .next()
            .and_then(|w| w.wl_surface().map(|s| s.into_owned()));
        if let Some(next) = next_surf {
            let serial = SCOUNTER.next_serial();
            if let Some(kbd) = self.seat.get_keyboard() {
                kbd.set_focus(self, Some(next), serial);
            }
        }

        self.needs_redraw = true;
    }

    fn new_popup(&mut self, surface: PopupSurface, _: PositionerState) {
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _: PopupSurface, _: wl_seat::WlSeat, _: smithay::utils::Serial) {}
}
delegate_xdg_shell!(Trixie);

// ── xdg decoration ────────────────────────────────────────────────────────────

impl XdgDecorationHandler for Trixie {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        toplevel.send_configure();
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, _: zxdg_toplevel_decoration_v1::Mode) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(Trixie);

// ── ensure_initial_configure ──────────────────────────────────────────────────

fn ensure_initial_configure(
    surface: &WlSurface,
    space: &smithay::desktop::Space<Window>,
    popups: &mut PopupManager,
) {
    if let Some(window) = space
        .elements()
        .find(|w| w.wl_surface().as_deref() == Some(surface))
    {
        if let Some(toplevel) = window.toplevel() {
            if !toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
        }
        return;
    }

    if let Some(popup) = popups.find_popup(surface) {
        match popup {
            PopupKind::Xdg(ref xdg) => {
                if !xdg.is_initial_configure_sent() {
                    let _ = xdg.send_configure();
                }
            }
            PopupKind::InputMethod(_) => {}
        }
    }
}
