// render.rs — per-frame render logic.
//
// Changes vs original:
//   - Applies workspace transition X offset (anim.ws_offsets()) to all windows.
//   - Inserts a software cursor element when hardware cursor plane is unavailable.
//   - Layout-morph animations are handled transparently via anim.get_rect().

use std::time::Duration;

use smithay::utils::Physical;
use smithay::{
    backend::{
        drm::DrmNode,
        renderer::{
            element::{
                solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement,
                AsRenderElements,
            },
            gles::GlesRenderer,
        },
    },
    desktop::{utils::send_frames_surface_tree, Window},
    reexports::{drm::control::crtc, wayland_server::Resource},
    utils::Scale,
    wayland::seat::WaylandFocus,
};

use trixui::pipelines::de::WindowInfo;
use trixui::renderer::DrawCmd;

use crate::{
    state::{trixui_rect, Trixie, TrixieElement},
    twm::{PaneId, Rect as TwmRect},
};

macro_rules! rdebug {
    ($($t:tt)*) => { tracing::debug!(target: "trixie_render", $($t)*) };
}

// ── Public entry ──────────────────────────────────────────────────────────────

pub fn render(state: &mut Trixie, node: DrmNode, crtc: crtc::Handle) -> bool {
    let focused_id = state.twm.workspaces[state.twm.active_ws].focused;

    let pane_snapshots: Vec<(PaneId, String, bool)> = state
        .twm
        .panes
        .values()
        .filter(|p| !state.anim.is_closing(p.id))
        .map(|p| (p.id, p.content.title().to_owned(), Some(p.id) == focused_id))
        .collect();

    rdebug!(
        "render: {} panes in twm, {} in snapshot, {} in space",
        state.twm.panes.len(),
        pane_snapshots.len(),
        state.space.elements().count(),
    );

    // Build the window list for the chrome pipeline.
    let windows: Vec<WindowInfo> = pane_snapshots
        .iter()
        .map(|(id, title, focused)| {
            let rect = resolve_pane_inner_rect(state, *id);
            WindowInfo::new(trixui_rect(rect), title.as_str(), *focused).tag(*id as u64)
        })
        .collect();

    if let Some(de) = &mut state.de {
        de.set_windows(windows);
    }

    sync_window_positions(state);

    // Collect chrome commands before touching the backend (no GL calls yet).
    let chrome_cmds: Vec<DrawCmd> = state.de.as_mut().map(|de| de.collect()).unwrap_or_default();

    let elements = build_elements(state, node);
    rdebug!("render: {} elements built", elements.len());

    let backend = match state.backends.get_mut(&node) {
        Some(b) => b,
        None => {
            rdebug!("render: no backend for node");
            return false;
        }
    };
    let surface = match backend.surfaces.get_mut(&crtc) {
        Some(s) => s,
        None => {
            rdebug!("render: no surface for crtc");
            return false;
        }
    };

    // Update hardware cursor position — inside the backend borrow so we only
    // touch drm once.  move_cursor is a no-op when hw_cursor_ok is false.
    let cx = state.cursor.pos.x as i32;
    let cy = state.cursor.pos.y as i32;
    state.cursor.move_cursor(&backend.drm, crtc, cx, cy);

    let clear = clear_color(&state.config.colors.pane_bg);
    let flags = smithay::backend::drm::compositor::FrameFlags::empty();

    match surface.compositor.render_frame::<_, TrixieElement>(
        &mut backend.renderer,
        &elements,
        clear,
        flags,
    ) {
        Ok(frame) => {
            rdebug!("render_frame ok: is_empty={}", frame.is_empty);

            if !frame.is_empty {
                if let Some(de) = &mut state.de {
                    unsafe {
                        gl::Enable(gl::BLEND);
                        gl::BlendFuncSeparate(
                            gl::ONE,
                            gl::ONE_MINUS_SRC_ALPHA,
                            gl::ONE,
                            gl::ONE_MINUS_SRC_ALPHA,
                        );
                    }
                    de.flush_collected(chrome_cmds);
                }
            }

            match surface.compositor.queue_frame(()) {
                Ok(()) => {
                    rdebug!("queue_frame ok");
                    let output = surface.output.clone();
                    let time = state.clock.now();
                    let wins: Vec<Window> = state.space.elements().cloned().collect();
                    for window in &wins {
                        if let Some(surf) = window.wl_surface() {
                            send_frames_surface_tree(
                                surf.as_ref(),
                                &output,
                                time,
                                Some(Duration::ZERO),
                                |_, _| Some(output.clone()),
                            );
                        }
                    }
                    surface.pending_frame = true;
                    true
                }
                Err(e) => {
                    tracing::warn!("queue_frame: {e}");
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!("render_frame: {e}");
            false
        }
    }
}

// ── Pane rect resolution ──────────────────────────────────────────────────────

fn resolve_pane_inner_rect(state: &Trixie, id: PaneId) -> smithay::utils::Rectangle<i32, Physical> {
    let fullscreen = state
        .twm
        .panes
        .get(&id)
        .map(|p| p.fullscreen)
        .unwrap_or(false);
    let bw = state.config.border_width as i32;
    let pane_rect = state.twm.panes.get(&id).map(|p| p.rect).unwrap_or_default();
    let rect = state.anim.get_rect(id, pane_rect);

    if fullscreen || bw == 0 {
        smithay::utils::Rectangle::from_loc_and_size(
            (rect.x as i32, rect.y as i32),
            (rect.w.max(1) as i32, rect.h.max(1) as i32),
        )
    } else {
        smithay::utils::Rectangle::from_loc_and_size(
            (rect.x as i32 + bw, rect.y as i32 + bw),
            (
                (rect.w as i32 - bw * 2).max(1),
                (rect.h as i32 - bw * 2).max(1),
            ),
        )
    }
}

fn resolve_window_rect(state: &Trixie, window: &Window) -> Option<TwmRect> {
    let surf = window.wl_surface()?;
    let surf_id = surf.as_ref().id();

    let pane_id = state.surface_to_pane.get(&surf_id).copied().or_else(|| {
        let app_id = smithay::wayland::compositor::with_states(surf.as_ref(), |states| {
            states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok())
                .and_then(|l| l.app_id.clone())
        })
        .unwrap_or_default();

        if app_id.is_empty() {
            return None;
        }

        let ws = &state.twm.workspaces[state.twm.active_ws];
        ws.panes.iter().find_map(|&id| {
            let pane = state.twm.panes.get(&id)?;
            if pane.content.app_id() == app_id {
                Some(id)
            } else {
                None
            }
        })
    })?;

    let pane = state.twm.panes.get(&pane_id)?;
    let bw = state.twm.border_w;
    let animated_rect = state.anim.get_rect(pane_id, pane.rect);

    let inner = if pane.fullscreen || bw == 0 {
        animated_rect
    } else {
        animated_rect.inset(bw)
    };

    Some(inner)
}

// ── Window position sync ──────────────────────────────────────────────────────

fn sync_window_positions(state: &mut Trixie) {
    // During layout morphs we still apply configure so clients resize to the
    // new size while the animation plays — the visual position comes from
    // the render element offset, not the Wayland surface location.
    let windows: Vec<Window> = state.space.elements().cloned().collect();
    let morphing = state.anim.morphing_panes();

    for window in windows {
        let Some(inner) = resolve_window_rect(state, &window) else {
            continue;
        };

        let loc = smithay::utils::Point::<i32, smithay::utils::Logical>::from((
            inner.x as i32,
            inner.y as i32,
        ));
        let new_size = smithay::utils::Size::<i32, smithay::utils::Logical>::from((
            inner.w as i32,
            inner.h as i32,
        ));

        let current_loc = state.space.element_location(&window);
        if current_loc != Some(loc) {
            state.space.map_element(window.clone(), loc, false);
        }

        let Some(toplevel) = window.toplevel() else {
            continue;
        };

        // During a morph, send configure to pre-size the client at the final size,
        // but don't spam every frame — only send when size actually changes.
        let surf_id = window.wl_surface().map(|s| s.as_ref().id());
        let pane_id = surf_id
            .as_ref()
            .and_then(|id| state.surface_to_pane.get(id).copied());
        let is_morphing = pane_id.map(|id| morphing.contains(&id)).unwrap_or(false);

        let already_pending = toplevel.with_pending_state(|s| s.size == Some(new_size));
        let already_committed = window.geometry().size == new_size;

        if !already_pending && (!already_committed || is_morphing) {
            toplevel.with_pending_state(|s| s.size = Some(new_size));
            toplevel.send_configure();
        }
    }
}

// ── Element list ──────────────────────────────────────────────────────────────

fn build_elements(state: &mut Trixie, node: DrmNode) -> Vec<TrixieElement> {
    struct WinLoc {
        window: Window,
        loc: smithay::utils::Point<i32, Physical>,
    }

    let scale = Scale::from(1.0_f64);

    // Workspace transition X offset — slides the entire incoming workspace in.
    let (incoming_x, _) = state.anim.ws_offsets();

    let win_locs: Vec<WinLoc> = state
        .space
        .elements()
        .filter_map(|w| {
            let inner = resolve_window_rect(state, w)?;
            let geom = w.geometry();
            // Apply the workspace slide offset.
            let base_x = inner.x as i32 - geom.loc.x + incoming_x;
            let loc = smithay::utils::Point::<i32, smithay::utils::Logical>::from((
                base_x,
                inner.y as i32 - geom.loc.y,
            ))
            .to_physical_precise_round(scale);
            rdebug!(
                "window: geometry={:?} inner={:?} ws_offset={incoming_x} loc={:?}",
                geom,
                inner,
                loc,
            );
            Some(WinLoc {
                window: w.clone(),
                loc,
            })
        })
        .collect();

    rdebug!(
        "build_elements: {} windows resolved from {} in space",
        win_locs.len(),
        state.space.elements().count(),
    );

    let out_w = state.twm.screen_w.max(1) as i32;
    let out_h = state.twm.screen_h.max(1) as i32;

    let bg_buf = smithay::backend::renderer::element::solid::SolidColorBuffer::new(
        (out_w, out_h),
        clear_color(&state.config.colors.pane_bg),
    );
    let bg_elem = TrixieElement::Cursor(SolidColorRenderElement::from_buffer(
        &bg_buf,
        smithay::utils::Point::<i32, Physical>::from((0, 0)),
        scale,
        1.0,
        smithay::backend::renderer::element::Kind::Unspecified,
    ));

    let backend = match state.backends.get_mut(&node) {
        Some(b) => b,
        None => return vec![],
    };

    // Elements are drawn back-to-front: the LAST element in the vec is the
    // bottom-most layer.  So we build: [cursor, ...windows..., background]
    // which renders as: background → windows → cursor (cursor on top).

    let mut elements: Vec<TrixieElement> = Vec::new();

    // ── Cursor (top layer) ────────────────────────────────────────────────────
    // Software cursor when hardware plane is unavailable.
    if !state.cursor.hw_cursor_ok {
        let sw_cursor = state.cursor.software_element(scale);
        elements.push(TrixieElement::Cursor(sw_cursor));
    }

    // ── Windows (middle layer) ────────────────────────────────────────────────
    elements.extend(
        win_locs
            .iter()
            .flat_map(|wl| {
                wl.window
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                        &mut backend.renderer,
                        wl.loc,
                        scale,
                        1.0,
                    )
            })
            .map(TrixieElement::Space),
    );

    // ── Background (bottom layer) ─────────────────────────────────────────────
    elements.push(bg_elem);

    elements
}

// ── Colour helpers ────────────────────────────────────────────────────────────

fn clear_color(c: &crate::config::Color) -> [f32; 4] {
    [
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        1.0,
    ]
}
