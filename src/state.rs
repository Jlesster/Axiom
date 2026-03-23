// src/state.rs — Root compositor state (Axiom).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

use calloop::LoopHandle;
use wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle, Resource};

use crate::{
    backend::OutputSurface,
    input::InputState,
    ipc::IpcServer,
    proto::{
        compositor::{SurfaceData, SurfaceRole},
        idle_inhibit::IdleInhibitState,
        layer_shell::LayerSurfaceRef,
        seat::SeatState,
        xdg_shell::{ToplevelDataRef, XdgRole, XdgSurfaceDataRef},
    },
    render::RenderState,
    scripting::{signals, ScriptEngine},
    wm::{anim::AnimSet, Window, WindowId, WmState},
    xwayland::XWaylandState,
};

// ── Per-output state ──────────────────────────────────────────────────────────

pub struct OutputState {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub scale: f64,
    pub render_surf: crate::backend::OutputSurface,
    pub wl_id: u32,
    pub last_vblank: Instant,
    pub frame_pending: bool,
}

// ── Surface / buffer data ─────────────────────────────────────────────────────

#[derive(Clone)]
pub enum RawBuffer {
    Shm {
        pool_fd: std::os::unix::io::RawFd,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: u32,
    },
    Dmabuf {
        fds: Vec<std::os::unix::io::RawFd>,
        offsets: Vec<u32>,
        strides: Vec<u32>,
        width: i32,
        height: i32,
        format: u32,
        modifier: u64,
    },
}

#[derive(Default, Clone)]
pub struct PendingSurface {
    pub buffer: Option<RawBuffer>,
    pub damage: Vec<[i32; 4]>,
    pub frame_callbacks: Vec<u32>,
}

#[derive(Default, Clone)]
pub struct CommittedSurface {
    pub buffer: Option<RawBuffer>,
    pub width: i32,
    pub height: i32,
}

pub struct PendingWindow {
    pub app_id: String,
    pub title: String,
    pub surface_id: u32,
    pub toplevel: ToplevelDataRef,
    pub surface: WlSurface,
}

// ── Interactive grab state ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum GrabKind {
    None,
    Move {
        win_id: WindowId,
        start_x: f64,
        start_y: f64,
    },
    Resize {
        win_id: WindowId,
        start_x: f64,
        start_y: f64,
        orig_w: i32,
        orig_h: i32,
    },
}

// ── Closing window tracking ───────────────────────────────────────────────────

pub struct ClosingWindow {
    pub win_id: WindowId,
    pub surface_id: u32,
}

// ── Root state ────────────────────────────────────────────────────────────────

pub struct Axiom {
    pub display: DisplayHandle,
    pub socket_name: String,

    pub backend: crate::backend::Backend,
    pub render: RenderState,
    pub input: InputState,
    pub seat: SeatState,
    pub wm: WmState,
    pub anim: AnimSet,
    pub script: ScriptEngine,
    pub ipc: IpcServer,

    pub outputs: Vec<OutputState>,

    pub surface_map: HashMap<u32, WindowId>,
    pub toplevel_map: HashMap<u32, ToplevelDataRef>,
    pub pending_windows: HashMap<u32, PendingWindow>,
    pub closing_windows: Vec<ClosingWindow>,
    pub layer_surfaces: Vec<(LayerSurfaceRef, WlSurface)>,

    pub idle_inhibit: IdleInhibitState,
    pub xwayland: XWaylandState,

    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, Self>,
    pub start_time: Instant,
    pub needs_redraw: bool,
    pub grab: GrabKind,
}

impl Axiom {
    // ── Timing ────────────────────────────────────────────────────────────────

    pub fn now_ms(&self) -> u32 {
        self.start_time.elapsed().as_millis() as u32
    }

    pub fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    // ── Serial ────────────────────────────────────────────────────────────────

    pub fn next_serial(&mut self) -> u32 {
        self.seat.next_serial()
    }

    // ── Focus ─────────────────────────────────────────────────────────────────

    pub fn sync_keyboard_focus(&mut self) {
        let prev = self.seat.keyboard_focus_id();
        let focused = self.wm.focused_window();
        let surface = focused.and_then(|id| self.wm.windows.get(&id)?.surface.clone());
        self.seat.set_keyboard_focus(surface);
        self.seat.set_keyboard_focus_win(focused);

        if prev != focused {
            if let Some(old_id) = prev {
                if let Some(win) = self.wm.windows.get(&old_id) {
                    self.script.emit_client(signals::SIG_UNFOCUS, win);
                }
            }
            if let Some(new_id) = focused {
                if let Some(win) = self.wm.windows.get(&new_id) {
                    self.script.emit_client(signals::SIG_FOCUS, win);
                }
            }
        }
    }

    pub fn send_configure_focused(&mut self) {
        let focused = match self.wm.focused_window() {
            Some(id) => id,
            None => return,
        };
        let surface = match self
            .wm
            .windows
            .get(&focused)
            .and_then(|w| w.surface.as_ref())
        {
            Some(s) => s.clone(),
            None => return,
        };
        self.send_configure_for_surface(&surface, focused);
        self.needs_redraw = true;
    }

    pub fn send_configure_for_surface(&mut self, surface: &WlSurface, win_id: WindowId) {
        let surf_data = match surface.data::<Arc<SurfaceData>>() {
            Some(d) => d.clone(),
            None => return,
        };
        let role = surf_data.role.lock().unwrap().clone();
        if let SurfaceRole::XdgToplevel = role {
            let surf_id = surface.id().protocol_id();
            if let Some(toplevel_ref) = self.toplevel_for_surface(surf_id) {
                let tl = toplevel_ref.lock().unwrap();
                let xdg_surface = tl.xdg_surface.clone();
                let xdg_data = tl.xdg_data.clone();
                drop(tl);

                let win = self.wm.window(win_id);
                let (w, h) = (win.rect.w, win.rect.h);

                // Never send a zero-size configure to an existing window.
                // 0×0 is only valid as the very first "you choose" configure.
                if w == 0 || h == 0 {
                    return;
                }

                let focused = self.wm.focused_window() == Some(win_id);

                let mut states: Vec<u8> = Vec::new();
                let mut push = |v: u32| states.extend_from_slice(&v.to_le_bytes());
                if win.maximized {
                    push(2);
                }
                if win.fullscreen {
                    push(5);
                }
                if focused {
                    push(4);
                }

                let role = xdg_data.lock().unwrap().role.clone();
                if let XdgRole::Toplevel(ref tl_obj) = role {
                    tl_obj.configure(w, h, states);
                    let serial = self.next_serial();
                    xdg_surface.configure(serial);
                    xdg_data.lock().unwrap().configure_serial = serial;
                }
            }
        }
    }

    fn toplevel_for_surface(&self, surf_id: u32) -> Option<ToplevelDataRef> {
        self.toplevel_map.get(&surf_id).cloned()
    }

    // ── Window lifecycle ──────────────────────────────────────────────────────

    pub fn register_toplevel(
        &mut self,
        surface: WlSurface,
        toplevel: ToplevelDataRef,
        _xdg_surface_data: XdgSurfaceDataRef,
    ) {
        let surf_id = surface.id().protocol_id();
        let app_id = toplevel.lock().unwrap().app_id.clone().unwrap_or_default();
        let title = toplevel.lock().unwrap().title.clone().unwrap_or_default();

        self.toplevel_map.insert(surf_id, toplevel.clone());
        self.pending_windows.insert(
            surf_id,
            PendingWindow {
                app_id,
                title,
                surface_id: surf_id,
                toplevel,
                surface: surface.clone(),
            },
        );

        if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
            *sd.role.lock().unwrap() = SurfaceRole::XdgToplevel;
        }
    }

    pub fn on_surface_commit(&mut self, surface: &WlSurface) {
        let surf_id = surface.id().protocol_id();

        // ── Pending window: promote only when a real buffer has arrived ───────────
        if let Some(pw) = self.pending_windows.get(&surf_id) {
            // Check whether this commit carries a non-null buffer.
            let has_buffer = surface
                .data::<Arc<SurfaceData>>()
                .map(|sd| sd.current.lock().unwrap().buffer.is_some())
                .unwrap_or(false);

            if !has_buffer {
                // No content yet — keep the configure round-trip alive but don't
                // touch the WM or render state.  No redraw needed.
                self.send_configure_focused();
                return;
            }

            // We have a buffer.  Remove from pending before any borrows below.
            let pw = self.pending_windows.remove(&surf_id).unwrap();

            // ── Step 1: upload under a temporary key (surf_id) ────────────────────
            // We use surf_id as the key because win_id doesn't exist yet.
            // upload_surface_texture accepts WindowId (u32 alias) so this works
            // as long as surf_id isn't already a live win_id — which it can't be
            // because surface ids and window ids come from different spaces.
            self.render
                .upload_surface_texture(surf_id, surface, &self.backend.egl);
            if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
                sd.current.lock().unwrap().needs_upload = false;
            }

            // ── Step 2: add to WM ─────────────────────────────────────────────────
            let mut win = Window::new(surf_id, pw.app_id.clone());
            win.title = pw.title.clone();
            win.surface = Some(surface.clone());
            let win_id = self.wm.add_window(win);
            pw.toplevel.lock().unwrap().window_id = Some(win_id);
            self.surface_map.insert(surf_id, win_id);

            // ── Step 3: re-key the texture entry surf_id → win_id ─────────────────
            // If surf_id == win_id (they happen to collide) this is a no-op.
            if surf_id != win_id {
                if let Some(tex) = self.render.textures.remove(&surf_id) {
                    self.render.textures.insert(win_id, tex);
                }
            }

            let rect = self.wm.window(win_id).rect;
            self.anim
                .set_geometry(win_id, rect.x, rect.y, rect.w, rect.h);

            // Fire frame callbacks for this commit.
            if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
                let cbs: Vec<_> = sd
                    .current
                    .lock()
                    .unwrap()
                    .frame_callbacks
                    .drain(..)
                    .collect();
                let now = self.now_ms();
                for cb in cbs {
                    cb.done(now);
                }
            }

            self.send_configure_focused();
            self.sync_keyboard_focus();

            if let Some(win) = self.wm.windows.get(&win_id) {
                self.script.emit_client(signals::SIG_MANAGE, win);
            }
            signals::update_client_list(&self.script.lua, &self.wm);
            signals::update_screen_count(&self.script.lua, self.wm.monitors.len());

            // Redraw only now — window is in draw list AND has a valid texture.
            self.needs_redraw = true;
            return;
        }

        // ── Layer-shell surface commit ────────────────────────────────────────────
        let is_layer = self
            .layer_surfaces
            .iter()
            .any(|(_, s)| s.id().protocol_id() == surf_id);
        if is_layer {
            if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
                let needs_upload = sd.current.lock().unwrap().needs_upload;
                if needs_upload {
                    self.render
                        .upload_layer_texture(surf_id, surface, &self.backend.egl);
                    sd.current.lock().unwrap().needs_upload = false;
                    // Only mark needs_redraw after upload — not on bare commits.
                    self.needs_redraw = true;
                }
                // Always fire frame callbacks so clients don't stall.
                let cbs: Vec<_> = sd
                    .current
                    .lock()
                    .unwrap()
                    .frame_callbacks
                    .drain(..)
                    .collect();
                let now = self.now_ms();
                for cb in cbs {
                    cb.done(now);
                }
            }
            return;
        }

        // ── Live window commit ────────────────────────────────────────────────────
        if let Some(&win_id) = self.surface_map.get(&surf_id) {
            if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
                let needs_upload = sd.current.lock().unwrap().needs_upload;
                if needs_upload {
                    self.render
                        .upload_surface_texture(win_id, surface, &self.backend.egl);
                    sd.current.lock().unwrap().needs_upload = false;
                }
                let cbs: Vec<_> = sd
                    .current
                    .lock()
                    .unwrap()
                    .frame_callbacks
                    .drain(..)
                    .collect();
                let now = self.now_ms();
                for cb in cbs {
                    cb.done(now);
                }
            }
            self.needs_redraw = true;
        }
    }

    pub fn on_surface_destroy(&mut self, surface: &WlSurface) {
        let id = surface.id().protocol_id();
        self.pending_windows.remove(&id);
        self.toplevel_map.remove(&id);

        let closing_idx = self.closing_windows.iter().position(|c| c.surface_id == id);
        if let Some(idx) = closing_idx {
            let cw = self.closing_windows.swap_remove(idx);
            self.finalize_window_removal(cw.win_id);
            return;
        }

        if let Some(win_id) = self.surface_map.remove(&id) {
            self.finalize_window_removal(win_id);
        }
    }

    pub fn close_window(&mut self, id: WindowId) {
        if let Some(win) = self.wm.windows.get(&id) {
            self.script.emit_client(signals::SIG_UNMANAGE, win);
        }

        let surface_id = self
            .wm
            .windows
            .get(&id)
            .and_then(|w| w.surface.as_ref())
            .map(|s| s.id().protocol_id());

        if let Some(sid) = surface_id {
            if let Some(tl_ref) = self.toplevel_map.get(&sid) {
                let xdg_data = tl_ref.lock().unwrap().xdg_data.clone();
                let role = xdg_data.lock().unwrap().role.clone();
                if let XdgRole::Toplevel(ref tl_obj) = role {
                    tl_obj.close();
                }
            }
        }

        self.wm.remove_window(id);
        self.anim.begin_close(id);

        if let Some(sid) = surface_id {
            self.surface_map.remove(&sid);
            self.closing_windows.push(ClosingWindow {
                win_id: id,
                surface_id: sid,
            });
        } else {
            self.finalize_window_removal(id);
        }

        signals::update_client_list(&self.script.lua, &self.wm);
        self.needs_redraw = true;
    }

    fn finalize_window_removal(&mut self, id: WindowId) {
        self.anim.remove(id);
        self.render.remove_window_texture(id);
        self.needs_redraw = true;
    }

    // ── Hit-testing ───────────────────────────────────────────────────────────

    pub fn surface_at(&self, px: f64, py: f64) -> Option<(WlSurface, f64, f64)> {
        let aws = self.wm.active_ws();
        let ws = &self.wm.workspaces[aws];
        for &win_id in ws.windows.iter().rev() {
            let win = self.wm.windows.get(&win_id)?;
            let r = self.anim.get_rect(win_id, win.rect);
            if r.contains(px as i32, py as i32) {
                if let Some(ref surf) = win.surface {
                    return Some((surf.clone(), px - r.x as f64, py - r.y as f64));
                }
            }
        }
        None
    }

    // ── Interactive grab ──────────────────────────────────────────────────────

    pub fn start_interactive_move(&mut self, id: WindowId) {
        if !self
            .wm
            .windows
            .get(&id)
            .map(|w| w.floating)
            .unwrap_or(false)
        {
            self.wm.toggle_float(id);
        }
        self.grab = GrabKind::Move {
            win_id: id,
            start_x: self.input.pointer_x,
            start_y: self.input.pointer_y,
        };
    }

    pub fn start_interactive_resize(&mut self, id: WindowId, _edges: u32) {
        let (w, h) = self
            .wm
            .windows
            .get(&id)
            .map(|w| (w.rect.w, w.rect.h))
            .unwrap_or((400, 300));
        self.grab = GrabKind::Resize {
            win_id: id,
            start_x: self.input.pointer_x,
            start_y: self.input.pointer_y,
            orig_w: w,
            orig_h: h,
        };
    }

    pub fn update_interactive_grab(&mut self, px: f64, py: f64) {
        match self.grab.clone() {
            GrabKind::None => {}

            GrabKind::Move {
                win_id,
                start_x,
                start_y,
            } => {
                let dx = (px - start_x) as i32;
                let dy = (py - start_y) as i32;
                self.wm.move_float(win_id, dx, dy);
                // Update the grab's reference point each frame so deltas stay small.
                self.grab = GrabKind::Move {
                    win_id,
                    start_x: px,
                    start_y: py,
                };
                // Send configure so client knows its new position.
                if let Some(surf) = self.wm.windows.get(&win_id).and_then(|w| w.surface.clone()) {
                    self.send_configure_for_surface(&surf, win_id);
                }
                self.needs_redraw = true;
            }

            GrabKind::Resize {
                win_id,
                start_x,
                start_y,
                orig_w,
                orig_h,
            } => {
                let dw = (px - start_x) as i32;
                let dh = (py - start_y) as i32;
                self.wm.resize_float(win_id, dw, dh);
                self.grab = GrabKind::Resize {
                    win_id,
                    start_x: px,
                    start_y: py,
                    orig_w: self
                        .wm
                        .windows
                        .get(&win_id)
                        .map(|w| w.rect.w)
                        .unwrap_or(orig_w),
                    orig_h: self
                        .wm
                        .windows
                        .get(&win_id)
                        .map(|w| w.rect.h)
                        .unwrap_or(orig_h),
                };
                // Critical: send configure so the client redraws at the new size.
                if let Some(surf) = self.wm.windows.get(&win_id).and_then(|w| w.surface.clone()) {
                    self.send_configure_for_surface(&surf, win_id);
                }
                self.needs_redraw = true;
            }
        }
    }

    pub fn end_grab(&mut self) {
        self.grab = GrabKind::None;
    }

    // ── Layer shell ───────────────────────────────────────────────────────────

    pub fn register_layer_surface(
        &mut self,
        surface: WlSurface,
        _layer_surface: wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        data: crate::proto::layer_shell::LayerSurfaceRef,
    ) {
        self.layer_surfaces.push((data, surface));
        self.update_usable_area();
        // No needs_redraw here — content arrives on first commit.
    }

    pub fn unregister_layer_surface(&mut self, surface: &WlSurface) {
        let id = surface.id().protocol_id();
        self.layer_surfaces
            .retain(|(_, s)| s.id().protocol_id() != id);
        self.render.remove_layer_texture(id);
        self.update_usable_area();
        self.needs_redraw = true;
    }

    pub fn update_usable_area(&mut self) {
        if let Some(mon) = self.wm.monitors.first() {
            let (w, h) = (mon.width, mon.height);
            // Start from layer-shell computed area.
            let mut usable =
                crate::proto::layer_shell::compute_usable_area(w, h, &self.layer_surfaces);
            // Then subtract bar height on top of whatever layer-shell left.
            let bh = self.wm.config.bar_height;
            if bh > 0 {
                if self.wm.config.bar_at_bottom {
                    usable.h = (usable.h - bh).max(1);
                } else {
                    usable.y += bh;
                    usable.h = (usable.h - bh).max(1);
                }
            }
            let oid = mon.output_id;
            self.wm.update_monitor_usable(oid, usable);
        }
    }

    // ── XWayland surface pairing ──────────────────────────────────────────────
    //
    // Two arrival paths; whichever arrives second calls complete_pairing().
    //
    //  Wayland-first: set_serial fires → try_pair_xwayland_surface()
    //    checks x11_serials for a matching X11 window.
    //    If found → complete_pairing().
    //    If not   → surface stays in pending_surfaces until MapWindow arrives.
    //
    //  X11-first: MapWindow fires → try_pair_from_x11()
    //    checks pending_surfaces for a matching serial.
    //    If found → complete_pairing().
    //    If not   → serial stored in x11_serials until set_serial arrives.

    /// Called from xwayland_surface_v1::set_serial (Wayland side arrives first
    /// or second).
    pub fn try_pair_xwayland_surface(&mut self, surface: &WlSurface, serial: u64) {
        // Check if X11 already told us about this serial.
        let x11_win = self
            .xwayland
            .x11_serials
            .iter()
            .find(|(_, &s)| s == serial)
            .map(|(&w, _)| w);

        if let Some(x11_win) = x11_win {
            // X11 arrived first — pair now.
            self.xwayland.x11_serials.remove(&x11_win);
            let title = self
                .xwayland
                .wm
                .as_ref()
                .and_then(|wm| wm.titles.get(&x11_win).cloned())
                .unwrap_or_default();
            let app_id = self
                .xwayland
                .wm
                .as_ref()
                .and_then(|wm| wm.app_ids.get(&x11_win).cloned())
                .unwrap_or_default();
            self.complete_pairing(surface.clone(), x11_win, title, app_id);
        } else {
            // Wayland arrived first — update pending entry so X11 side can
            // find it by serial when MapWindow fires.
            for ps in &mut self.xwayland.pending_surfaces {
                if ps.surface.id() == surface.id() {
                    ps.serial = Some(serial);
                    break;
                }
            }
        }
    }

    /// Called from the X11Action::MapWindow handler (X11 side arrives first or
    /// second).
    pub fn try_pair_from_x11(
        &mut self,
        x11_win: u32,
        title: String,
        app_id: String,
        surface_serial: Option<u64>,
    ) {
        let serial = match surface_serial {
            Some(s) => s,
            None => {
                // No serial on this window — nothing to pair (e.g.
                // override-redirect menus). Record nothing and return.
                return;
            }
        };

        // Check if the Wayland surface already arrived and is sitting in
        // pending_surfaces with a matching serial.
        let pending_idx = self
            .xwayland
            .pending_surfaces
            .iter()
            .position(|ps| ps.serial == Some(serial));

        if let Some(idx) = pending_idx {
            let ps = self.xwayland.pending_surfaces.swap_remove(idx);
            self.complete_pairing(ps.surface, x11_win, title, app_id);
        } else {
            // Wayland surface not here yet — stash serial so set_serial can
            // find it.
            self.xwayland.x11_serials.insert(x11_win, serial);
        }
    }

    /// Complete a pairing once both sides are known. Safe to call from either
    /// direction.
    fn complete_pairing(
        &mut self,
        surface: WlSurface,
        x11_win: u32,
        title: String,
        app_id: String,
    ) {
        let surf_id = surface.id().protocol_id();

        // Guard against double-pairing.
        if self.xwayland.x11_to_wl.contains_key(&x11_win) {
            return;
        }
        if self.surface_map.contains_key(&surf_id) {
            return;
        }

        let mut win = Window::new(surf_id, app_id);
        win.title = title;
        win.surface = Some(surface);

        let win_id = self.wm.add_window(win);

        self.surface_map.insert(surf_id, win_id);
        self.xwayland.x11_to_wl.insert(x11_win, win_id);
        self.xwayland.wl_to_x11.insert(win_id, x11_win);

        let rect = self.wm.window(win_id).rect;
        self.anim
            .set_geometry(win_id, rect.x, rect.y, rect.w, rect.h);
        self.sync_keyboard_focus();

        if let Some(win) = self.wm.windows.get(&win_id) {
            self.script.emit_client(signals::SIG_MANAGE, win);
        }
        signals::update_client_list(&self.script.lua, &self.wm);

        self.needs_redraw = true;
        tracing::debug!("xwayland paired: x11={x11_win} surf={surf_id} win={win_id}");
    }

    /// Tear down the pairing when an X11 window unmaps.
    pub fn unpair_x11_window(&mut self, x11_win: u32) {
        let Some(win_id) = self.xwayland.x11_to_wl.remove(&x11_win) else {
            return;
        };
        self.xwayland.wl_to_x11.remove(&win_id);

        let surf_id = self
            .wm
            .windows
            .get(&win_id)
            .and_then(|w| w.surface.as_ref())
            .map(|s| s.id().protocol_id());

        if let Some(sid) = surf_id {
            self.surface_map.remove(&sid);
        }

        if let Some(win) = self.wm.windows.get(&win_id) {
            self.script.emit_client(signals::SIG_UNMANAGE, win);
        }

        self.wm.remove_window(win_id);
        self.anim.remove(win_id);
        self.render.remove_window_texture(win_id);

        signals::update_client_list(&self.script.lua, &self.wm);
        self.sync_keyboard_focus();
        self.needs_redraw = true;
    }

    // ── Config ────────────────────────────────────────────────────────────────

    pub fn reload_config(&mut self) {
        if let Err(e) = self.script.reload(&mut self.wm) {
            tracing::warn!("config reload: {e}");
        }
        self.needs_redraw = true;
    }
}
