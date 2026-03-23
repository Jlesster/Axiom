// src/state.rs — Root compositor state (Axiom).

use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};

use calloop::LoopHandle;
use wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle, Resource};

use crate::{
    input::InputState,
    ipc::IpcServer,
    portal::{PortalHandle, PortalRequest},
    proto::{
        compositor::{SurfaceData, SurfaceRole},
        idle_inhibit::IdleInhibitState,
        layer_shell::LayerSurfaceRef,
        seat::SeatState,
        xdg_shell::{ToplevelDataRef, XdgRole},
    },
    render::RenderState,
    scripting::{signals, ScriptEngine},
    wm::{anim::AnimSet, Window, WindowId, WmState},
    xwayland::X11Action,
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

    pub outputs: Vec<OutputState>,

    pub surface_map: HashMap<u32, WindowId>,
    pub toplevel_map: HashMap<u32, ToplevelDataRef>,
    pub pending_windows: HashMap<u32, PendingWindow>,
    pub closing_windows: Vec<ClosingWindow>,
    pub layer_surfaces: Vec<(LayerSurfaceRef, WlSurface)>,

    // ── New subsystems ────────────────────────────────────────────────────────
    pub ipc: IpcServer,
    pub idle_inhibit: IdleInhibitState,
    pub xwayland: crate::xwayland::XWaylandState,
    pub portal: Option<PortalHandle>,

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

        // Also update X11 focus if the focused window is an XWayland window.
        if let Some(id) = focused {
            if let Some(&x11_win) = self.xwayland.wl_to_x11.get(&id) {
                self.xwayland.set_focus(x11_win);
            }
        }

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
            .and_then(|w| w.surface.clone())
        {
            Some(s) => s,
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
        _xdg_surface_data: crate::proto::xdg_shell::XdgSurfaceDataRef,
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

        if let Some(pw) = self.pending_windows.remove(&surf_id) {
            let mut win = Window::new(surf_id, pw.app_id.clone());
            win.title = pw.title.clone();
            win.surface = Some(surface.clone());
            let win_id = self.wm.add_window(win);
            pw.toplevel.lock().unwrap().window_id = Some(win_id);
            self.surface_map.insert(surf_id, win_id);
            let rect = self.wm.window(win_id).rect;
            self.anim
                .set_geometry(win_id, rect.x, rect.y, rect.w, rect.h);
            self.send_configure_focused();
            self.sync_keyboard_focus();
            if let Some(win) = self.wm.windows.get(&win_id) {
                self.script.emit_client(signals::SIG_MANAGE, win);
            }
            signals::update_client_list(&self.script.lua, &self.wm);
            signals::update_screen_count(&self.script.lua, self.wm.monitors.len());
        }

        let is_layer = self
            .layer_surfaces
            .iter()
            .any(|(_, s)| s.id().protocol_id() == surf_id);
        if is_layer {
            if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
                if sd.current.lock().unwrap().needs_upload {
                    self.render.upload_layer_texture(surf_id, surface);
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
            return;
        }

        if let Some(&win_id) = self.surface_map.get(&surf_id) {
            if let Some(sd) = surface.data::<Arc<SurfaceData>>() {
                if sd.current.lock().unwrap().needs_upload {
                    self.render.upload_surface_texture(win_id, surface);
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
        let closing = self.closing_windows.iter().position(|c| c.surface_id == id);
        if let Some(idx) = closing {
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

        // If XWayland window, send WM_DELETE_WINDOW.
        if let Some(&x11_win) = self.xwayland.wl_to_x11.get(&id) {
            self.xwayland.close_window(x11_win);
        }

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
        // Clean up XWayland pairing.
        if let Some(x11_win) = self.xwayland.wl_to_x11.remove(&id) {
            self.xwayland.x11_to_wl.remove(&x11_win);
        }
        self.anim.remove(id);
        self.render.remove_window_texture(id);
        self.needs_redraw = true;
    }

    // ── XWayland ──────────────────────────────────────────────────────────────

    /// Called when a new X11 window should be mapped into the WM.
    pub fn map_xwayland_window(&mut self, action: X11Action) {
        if let X11Action::MapWindow {
            x11_win,
            title,
            app_id,
            override_redirect,
        } = action
        {
            // For override_redirect (tooltips/menus) just render them, no WM.
            if override_redirect {
                tracing::debug!("X11 override_redirect window {x11_win}");
                return;
            }
            let mut win = Window::new(x11_win, app_id.clone());
            win.title = title;
            // Surface will be paired when xwayland-surface-v1 set_serial fires.
            let win_id = self.wm.add_window(win);
            self.xwayland.x11_to_wl.insert(x11_win, win_id);
            self.xwayland.wl_to_x11.insert(win_id, x11_win);

            // Configure the X11 window to match the WM rect.
            let rect = self.wm.window(win_id).rect;
            self.xwayland
                .configure_window(x11_win, rect.x, rect.y, rect.w as u32, rect.h as u32);

            signals::update_client_list(&self.script.lua, &self.wm);
            self.sync_keyboard_focus();
            self.needs_redraw = true;
        }
    }

    pub fn unmap_xwayland_window(&mut self, x11_win: u32) {
        if let Some(win_id) = self.xwayland.x11_to_wl.remove(&x11_win) {
            self.xwayland.wl_to_x11.remove(&win_id);
            self.wm.remove_window(win_id);
            self.anim.remove(win_id);
            self.render.remove_window_texture(win_id);
            signals::update_client_list(&self.script.lua, &self.wm);
            self.needs_redraw = true;
        }
    }

    /// Pair an XWayland surface (set_serial) with its X11 window.
    /// XWayland writes the serial as WL_SURFACE_SERIAL on the X11 window;
    /// we receive it here after the Wayland set_serial call.
    pub fn try_pair_xwayland_surface(&mut self, surface: &WlSurface, _serial: u64) {
        // In practice XWayland sends the X11 window id as a property on the
        // surface before this point.  The simpler pairing used by most
        // compositors is via WL_SURFACE_ID (legacy) or WL_SURFACE_SERIAL (v2).
        // We match pending surfaces by surface identity.
        let surf_id = surface.id().protocol_id();
        // Check if any known X11 window is unpaired.
        let unpaired: Vec<u32> = self
            .xwayland
            .x11_to_wl
            .keys()
            .copied()
            .filter(|x| {
                let wl_id = self.xwayland.x11_to_wl[x];
                self.wm
                    .windows
                    .get(&wl_id)
                    .and_then(|w| w.surface.as_ref())
                    .is_none()
            })
            .collect();
        if let Some(x11_win) = unpaired.first().copied() {
            let wl_id = self.xwayland.x11_to_wl[&x11_win];
            if let Some(win) = self.wm.windows.get_mut(&wl_id) {
                win.surface = Some(surface.clone());
            }
            self.surface_map.insert(surf_id, wl_id);
            tracing::debug!("Paired X11 window {x11_win} → Wayland surface {surf_id}");
            self.needs_redraw = true;
        }
    }

    // ── Portal ────────────────────────────────────────────────────────────────

    /// Poll portal requests and handle them on the compositor thread.
    pub fn poll_portal_requests(&mut self) {
        // Temporarily take portal out to avoid borrow issues.
        let mut portal = match self.portal.take() {
            Some(p) => p,
            None => return,
        };
        while let Ok(req) = portal.rx.try_recv() {
            match req {
                PortalRequest::Screenshot { path } => {
                    self.take_screenshot(&path);
                }
                PortalRequest::StartCast { output_name: _ } => {
                    // Push the next frame via the event sender.
                    if let Some(pixels) = self.read_output_pixels(0) {
                        let (w, h) = self
                            .outputs
                            .first()
                            .map(|o| (o.width, o.height))
                            .unwrap_or((1920, 1080));
                        let _ = portal.tx.try_send(crate::portal::PortalEvent::Frame {
                            pixels,
                            width: w,
                            height: h,
                        });
                    }
                }
                PortalRequest::StopCast => {}
            }
        }
        self.portal = Some(portal);
    }

    fn take_screenshot(&mut self, path: &str) {
        let Some(pixels) = self.read_output_pixels(0) else {
            return;
        };
        let (w, h) = self
            .outputs
            .first()
            .map(|o| (o.width, o.height))
            .unwrap_or((1920, 1080));
        // Save as PNG using raw bytes (BGRA → RGBA swap).
        let rgba: Vec<u8> = pixels
            .chunks(4)
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect();
        match image_save_png(path, &rgba, w, h) {
            Ok(()) => tracing::info!("Screenshot saved: {path}"),
            Err(e) => tracing::warn!("Screenshot failed: {e}"),
        }
    }

    pub fn read_output_pixels(&mut self, out_idx: usize) -> Option<Vec<u8>> {
        let out = self.outputs.get(out_idx)?;
        let (w, h) = (out.width, out.height);
        let surf = &out.render_surf as *const crate::backend::OutputSurface;
        let surf = unsafe { &*surf };
        surf.make_current(&self.backend.egl).ok()?;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        unsafe {
            gl::ReadPixels(
                0,
                0,
                w as i32,
                h as i32,
                gl::BGRA,
                gl::UNSIGNED_BYTE,
                pixels.as_mut_ptr() as _,
            );
            // Flip vertically.
            let stride = (w * 4) as usize;
            let mut tmp = vec![0u8; stride];
            for row in 0..h as usize / 2 {
                let top = row * stride;
                let bot = (h as usize - 1 - row) * stride;
                tmp.copy_from_slice(&pixels[top..top + stride]);
                pixels.copy_within(bot..bot + stride, top);
                pixels[bot..bot + stride].copy_from_slice(&tmp);
            }
        }
        Some(pixels)
    }

    // ── Hit-testing ───────────────────────────────────────────────────────────

    pub fn surface_at(&self, px: f64, py: f64) -> Option<(WlSurface, f64, f64)> {
        // Check layer surfaces first (overlay / top layers intercept input).
        for (ls_ref, surf) in self.layer_surfaces.iter().rev() {
            let ls = ls_ref.lock().unwrap();
            use crate::proto::layer_shell::Layer;
            if matches!(ls.layer, Layer::Overlay | Layer::Top) && ls.mapped {
                let (x, y, lw, lh) = crate::render::layer_geom(
                    &ls,
                    self.outputs.first().map(|o| o.width as i32).unwrap_or(1920),
                    self.outputs
                        .first()
                        .map(|o| o.height as i32)
                        .unwrap_or(1080),
                );
                if px as i32 >= x && py as i32 >= y && (px as i32) < x + lw && (py as i32) < y + lh
                {
                    return Some((surf.clone(), px - x as f64, py - y as f64));
                }
            }
        }

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
                // Also move X11 window if applicable.
                if let Some(&x11) = self.xwayland.wl_to_x11.get(&win_id) {
                    let rect = self
                        .wm
                        .windows
                        .get(&win_id)
                        .map(|w| w.rect)
                        .unwrap_or_default();
                    self.xwayland.configure_window(
                        x11,
                        rect.x,
                        rect.y,
                        rect.w as u32,
                        rect.h as u32,
                    );
                }
                self.grab = GrabKind::Move {
                    win_id,
                    start_x: px,
                    start_y: py,
                };
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
                if let Some(&x11) = self.xwayland.wl_to_x11.get(&win_id) {
                    let rect = self
                        .wm
                        .windows
                        .get(&win_id)
                        .map(|w| w.rect)
                        .unwrap_or_default();
                    self.xwayland.configure_window(
                        x11,
                        rect.x,
                        rect.y,
                        rect.w as u32,
                        rect.h as u32,
                    );
                }
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
        self.needs_redraw = true;
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
            let usable = crate::proto::layer_shell::compute_usable_area(w, h, &self.layer_surfaces);
            let oid = mon.output_id;
            self.wm.update_monitor_usable(oid, usable);
        }
    }

    // ── Config ────────────────────────────────────────────────────────────────

    pub fn reload_config(&mut self) {
        if let Err(e) = self.script.reload(&mut self.wm) {
            tracing::warn!("config reload: {e}");
        }
        self.needs_redraw = true;
    }
}

// ── Simple PNG writer (no extra deps — raw DEFLATE via miniz) ─────────────────
// We use the `image` crate which is not in Cargo.toml yet.
// For now write a PPM so there are no new deps; add `image = "0.25"` to pull
// in full PNG support.

fn image_save_png(path: &str, rgba: &[u8], w: u32, h: u32) -> anyhow::Result<()> {
    // Write as PPM (universally readable, no extra dep).
    // Swap to PNG by adding `image = "0.25"` and using image::save_buffer.
    use std::io::Write;
    let ppm_path = path.replace(".png", ".ppm");
    let mut f = std::fs::File::create(&ppm_path)?;
    write!(f, "P6\n{w} {h}\n255\n")?;
    // PPM is RGB — drop alpha.
    for px in rgba.chunks(4) {
        f.write_all(&px[..3])?;
    }
    tracing::info!("Saved {ppm_path} (add image crate for PNG)");
    Ok(())
}
