// render.rs — per-frame render logic.

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

use crate::{
    state::{Trixie, TrixieElement},
    twm::{PaneId, Rect as TwmRect},
};

// ── Public entry ──────────────────────────────────────────────────────────────

pub fn render(
    state: &mut Trixie,
    node: DrmNode,
    crtc: crtc::Handle,
    chrome_cmds: Vec<trixui::DrawCmd>,
) -> bool {
    sync_window_positions(state);

    let elements = build_elements(state, node);

    let backend = match state.backends.get_mut(&node) {
        Some(b) => b,
        None => return false,
    };
    let surface = match backend.surfaces.get_mut(&crtc) {
        Some(s) => s,
        None => return false,
    };

    let clear = clear_color(&state.config.colors.pane_bg);
    // NOTE: reset_buffers() must NOT be called here. Doing so every frame
    // destroys DRM buffer-age tracking, forcing a full-screen recomposite on
    // every vblank. When a new window is opening this creates a feedback loop:
    // partial surface state → full recomposite → incomplete frame queued →
    // vblank → repeat → visible seizure/flash.
    // reset_buffers() belongs only in the VT-resume / device-reactivation path
    // where the underlying framebuffer has actually been externally invalidated.

    let flags = smithay::backend::drm::compositor::FrameFlags::empty();

    match surface.compositor.render_frame::<_, TrixieElement>(
        &mut backend.renderer,
        &elements,
        clear,
        flags,
    ) {
        Ok(frame) => {
            // Always flush chrome — trixui repaints the bar every frame
            // (clock updates, workspace state, etc.) and manages its own
            // damage internally. Gating on frame.is_empty caused the bar
            // to disappear when no client surfaces were producing damage.
            if let Some(ui) = &mut state.ui {
                ui.flush_collected(chrome_cmds);
            }

            match surface.compositor.queue_frame(()) {
                Ok(()) => {
                    let output = surface.output.clone();
                    let time = state.clock.now();
                    let windows: Vec<Window> = state.space.elements().cloned().collect();
                    for window in &windows {
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

                    // pending_frame is owned here — set it exactly once on
                    // successful queue_frame. state.rs must NOT also set it.
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

// ── Window → pane resolution ──────────────────────────────────────────────────

fn resolve_pane_inner_rect(state: &Trixie, window: &Window) -> Option<TwmRect> {
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

    // Route through the animation system — returns the interpolated rect
    // while the pane is animating, or the real TWM rect when idle.
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
    let windows: Vec<Window> = state.space.elements().cloned().collect();

    for window in windows {
        let Some(inner) = resolve_pane_inner_rect(state, &window) else {
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

        let already_pending = toplevel.with_pending_state(|s| s.size == Some(new_size));
        let already_committed = window.geometry().size == new_size;

        if !already_pending && !already_committed {
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

    let win_locs: Vec<WinLoc> = state
        .space
        .elements()
        .filter_map(|w| {
            let inner = resolve_pane_inner_rect(state, w)?;
            let loc = smithay::utils::Point::<i32, smithay::utils::Logical>::from((
                inner.x as i32,
                inner.y as i32,
            ))
            .to_physical_precise_round(scale);
            Some(WinLoc {
                window: w.clone(),
                loc,
            })
        })
        .collect();

    let backend = match state.backends.get_mut(&node) {
        Some(b) => b,
        None => return vec![],
    };

    // Full-screen background element at the bottom of the stack.
    //
    // Without this, the DRM compositor only marks regions covered by Wayland
    // surfaces or SolidColorRenderElements as damaged. The padding/gap areas
    // between windows have no element covering them, so the compositor skips
    // repainting them — leaving stale content from previous frames (e.g. a
    // window title that persists after close). The clear colour only fills
    // actually-damaged regions, not the entire framebuffer.
    //
    // A zero-alpha 1×1 element is not sufficient — it must cover the full
    // output so every pixel is included in the damage region each frame.
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

    let mut elements: Vec<TrixieElement> = win_locs
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
        .map(TrixieElement::Space)
        .collect();

    elements.extend(border_elements(state, scale));
    // Background goes last so it is below everything else in the element stack.
    elements.push(bg_elem);
    elements
}

// ── Border elements ───────────────────────────────────────────────────────────

fn border_elements(state: &Trixie, scale: Scale<f64>) -> Vec<TrixieElement> {
    let border_w = state.twm.border_w;
    if border_w == 0 {
        return vec![];
    }

    let ws = &state.twm.workspaces[state.twm.active_ws];
    let focused_id = ws.focused;
    let active_col = srgb(state.config.colors.active_border);
    let inactive_col = srgb(state.config.colors.inactive_border);

    ws.panes
        .iter()
        .filter_map(|&id| {
            let pane = state.twm.panes.get(&id)?;
            if pane.fullscreen {
                return None;
            }
            // Don't render borders for panes playing a close animation — the
            // Wayland surface is already destroyed so there's no window content
            // to frame. Without this the border/title lingers on the clear color
            // for the full 150 ms deferred-close window.
            if state.anim.is_closing(id) {
                return None;
            }
            // Use the animated rect for borders too so they slide with the window.
            let rect = state.anim.get_rect(id, pane.rect);
            let color = if Some(id) == focused_id {
                active_col
            } else {
                inactive_col
            };
            Some(border_rects(rect, border_w, color, scale))
        })
        .flatten()
        .collect()
}

fn border_rects(rect: TwmRect, bw: u32, color: [f32; 4], scale: Scale<f64>) -> Vec<TrixieElement> {
    let strips: [(i32, i32, i32, i32); 4] = [
        (rect.x as i32, rect.y as i32, rect.w as i32, bw as i32),
        (
            rect.x as i32,
            (rect.y + rect.h - bw) as i32,
            rect.w as i32,
            bw as i32,
        ),
        (rect.x as i32, rect.y as i32, bw as i32, rect.h as i32),
        (
            (rect.x + rect.w - bw) as i32,
            rect.y as i32,
            bw as i32,
            rect.h as i32,
        ),
    ];

    strips
        .into_iter()
        .map(|(x, y, w, h)| {
            let buf =
                smithay::backend::renderer::element::solid::SolidColorBuffer::new((w, h), color);
            let loc = smithay::utils::Point::<i32, Physical>::from((
                (x as f64 * scale.x) as i32,
                (y as f64 * scale.y) as i32,
            ));
            TrixieElement::Cursor(SolidColorRenderElement::from_buffer(
                &buf,
                loc,
                scale,
                1.0,
                smithay::backend::renderer::element::Kind::Unspecified,
            ))
        })
        .collect()
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

fn srgb(c: crate::config::Color) -> [f32; 4] {
    [
        (c.r as f32 / 255.0).powf(2.2),
        (c.g as f32 / 255.0).powf(2.2),
        (c.b as f32 / 255.0).powf(2.2),
        1.0,
    ]
}
