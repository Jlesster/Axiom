// twm/anim.rs — Slide-from-edge open/close, workspace transitions, layout morphs.
//
// AnimSet tracks per-pane animations (open/close) and a global workspace
// transition animation (slide the entire frame left/right when switching
// workspaces).  Layout change animations morph each pane's rect from its old
// position to its new tiled position over 200 ms with a cubic ease-out.
//
// API surface (called from the compositor layer):
//   anim.open(id, rect)              — new pane slides in from nearest edge
//   anim.close(id, rect)             — pane slides out and disappears
//   anim.workspace_transition(dir)   — begin a full-screen slide (left/right)
//   anim.layout_morph(id, old, new)  — pane rect morphs between layout positions
//   anim.tick() → bool               — advance all animations, true = still running
//   anim.get_rect(id, twm_rect)      — current interpolated rect for a pane
//   anim.ws_offset()                 — current pixel X offset for workspace slide
//   anim.is_closing(id)              — true if the pane is mid-close animation

use std::time::Instant;

use super::{PaneId, Rect};

// ── Easing ────────────────────────────────────────────────────────────────────

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_cubic(t: f32) -> f32 {
    t.clamp(0.0, 1.0).powi(3)
}

fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0f32).powi(3) / 2.0
    }
}

fn ease_out_quint(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(5)
}

// ── Edge ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

fn nearest_edge(rect: Rect, screen_w: u32, screen_h: u32) -> Edge {
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    let dx = cx as f32 / screen_w.max(1) as f32 - 0.5;
    let dy = cy as f32 / screen_h.max(1) as f32 - 0.5;

    if dx == 0.0 && dy == 0.0 {
        return Edge::Bottom;
    }

    if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            Edge::Right
        } else {
            Edge::Left
        }
    } else {
        if dy >= 0.0 {
            Edge::Bottom
        } else {
            Edge::Top
        }
    }
}

fn edge_rect_signed(rect: Rect, edge: Edge, screen_w: u32, screen_h: u32) -> [i32; 4] {
    let (x, y, w, h) = (rect.x as i32, rect.y as i32, rect.w as i32, rect.h as i32);
    match edge {
        Edge::Top => [x, -h, w, h],
        Edge::Bottom => [x, screen_h as i32, w, h],
        Edge::Left => [-w, y, w, h],
        Edge::Right => [screen_w as i32, y, w, h],
    }
}

fn lerp_i32(a: i32, b: i32, t: f32) -> i32 {
    (a as f32 + (b as f32 - a as f32) * t).round() as i32
}

fn lerp_rect_render(a: [i32; 4], b: [i32; 4], t: f32) -> Rect {
    let x = lerp_i32(a[0], b[0], t) as u32;
    let y = lerp_i32(a[1], b[1], t) as u32;
    let w = lerp_i32(a[2], b[2], t).max(1) as u32;
    let h = lerp_i32(a[3], b[3], t).max(1) as u32;
    Rect::new(x, y, w, h)
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    lerp_rect_render(
        [a.x as i32, a.y as i32, a.w as i32, a.h as i32],
        [b.x as i32, b.y as i32, b.w as i32, b.h as i32],
        t,
    )
}

// ── Per-pane AnimState ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimKind {
    Open,
    Close,
    /// Layout morph: smoothly move pane from old tiled rect to new tiled rect.
    LayoutMorph,
}

#[derive(Debug, Clone)]
struct PaneAnim {
    kind: AnimKind,
    start: Instant,
    duration_ms: u64,
    /// Final on-screen rect (open destination / close source / morph destination).
    target_rect: Rect,
    /// Start position — for open/close this is the off-screen edge rect (as signed [x,y,w,h]).
    /// For layout morphs this is the previous on-screen rect (directly usable as Rect).
    from_rect: [i32; 4],
}

impl PaneAnim {
    fn progress(&self) -> f32 {
        let elapsed = self.start.elapsed().as_millis() as f32;
        (elapsed / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    fn is_done(&self) -> bool {
        self.progress() >= 1.0
    }

    fn current_rect(&self) -> Rect {
        let t = self.progress();
        let target = [
            self.target_rect.x as i32,
            self.target_rect.y as i32,
            self.target_rect.w as i32,
            self.target_rect.h as i32,
        ];
        match self.kind {
            AnimKind::Open => lerp_rect_render(self.from_rect, target, ease_out_cubic(t)),
            AnimKind::Close => lerp_rect_render(target, self.from_rect, ease_in_cubic(t)),
            AnimKind::LayoutMorph => lerp_rect_render(self.from_rect, target, ease_out_quint(t)),
        }
    }
}

// ── Workspace transition ──────────────────────────────────────────────────────

/// Direction of a workspace switch animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsDir {
    Left,  // switching to a lower-numbered workspace
    Right, // switching to a higher-numbered workspace
}

#[derive(Debug, Clone)]
struct WsTransition {
    dir: WsDir,
    start: Instant,
    duration_ms: u64,
    screen_w: u32,
}

impl WsTransition {
    fn progress(&self) -> f32 {
        let elapsed = self.start.elapsed().as_millis() as f32;
        (elapsed / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    fn is_done(&self) -> bool {
        self.progress() >= 1.0
    }

    /// Returns the X pixel offset to apply to the *incoming* workspace layer.
    /// Positive = shifted right (slides in from left), negative = shifted left.
    pub fn incoming_offset_x(&self) -> i32 {
        let t = ease_out_quint(self.progress());
        let sw = self.screen_w as i32;
        match self.dir {
            // New workspace slides in from the right → starts at +screen_w, ends at 0.
            WsDir::Right => (sw as f32 * (1.0 - t)).round() as i32,
            // New workspace slides in from the left → starts at -screen_w, ends at 0.
            WsDir::Left => (-(sw as f32) * (1.0 - t)).round() as i32,
        }
    }

    /// Returns the X pixel offset for the *outgoing* workspace layer.
    pub fn outgoing_offset_x(&self) -> i32 {
        let t = ease_out_quint(self.progress());
        let sw = self.screen_w as i32;
        match self.dir {
            // Old workspace slides out to the left.
            WsDir::Right => (-(sw as f32) * t).round() as i32,
            // Old workspace slides out to the right.
            WsDir::Left => ((sw as f32) * t).round() as i32,
        }
    }
}

// ── AnimSet ───────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct AnimSet {
    panes: std::collections::HashMap<PaneId, PaneAnim>,
    ws_transition: Option<WsTransition>,
    pub screen_w: u32,
    pub screen_h: u32,
}

impl AnimSet {
    pub fn resize(&mut self, screen_w: u32, screen_h: u32) {
        self.screen_w = screen_w;
        self.screen_h = screen_h;
    }

    // ── Per-pane animations ───────────────────────────────────────────────────

    /// Begin an open animation for `id` sliding in to `rect`.
    pub fn open(&mut self, id: PaneId, rect: Rect) {
        let edge = nearest_edge(rect, self.screen_w, self.screen_h);
        let off = edge_rect_signed(rect, edge, self.screen_w, self.screen_h);
        self.panes.insert(
            id,
            PaneAnim {
                kind: AnimKind::Open,
                start: Instant::now(),
                duration_ms: 200,
                target_rect: rect,
                from_rect: off,
            },
        );
    }

    /// Begin a close animation for `id` sliding out from `rect`.
    pub fn close(&mut self, id: PaneId, rect: Rect) {
        let edge = nearest_edge(rect, self.screen_w, self.screen_h);
        let off = edge_rect_signed(rect, edge, self.screen_w, self.screen_h);
        self.panes.insert(
            id,
            PaneAnim {
                kind: AnimKind::Close,
                start: Instant::now(),
                duration_ms: 150,
                target_rect: rect,
                from_rect: off,
            },
        );
    }

    /// Begin a layout morph for `id` moving from `old_rect` to `new_rect`.
    /// Called by twm reflow whenever a layout change is detected.
    pub fn layout_morph(&mut self, id: PaneId, old_rect: Rect, new_rect: Rect) {
        // Don't stomp an in-progress open/close.
        if let Some(existing) = self.panes.get(&id) {
            if matches!(existing.kind, AnimKind::Open | AnimKind::Close) {
                return;
            }
        }
        self.panes.insert(
            id,
            PaneAnim {
                kind: AnimKind::LayoutMorph,
                start: Instant::now(),
                duration_ms: 200,
                target_rect: new_rect,
                from_rect: [
                    old_rect.x as i32,
                    old_rect.y as i32,
                    old_rect.w as i32,
                    old_rect.h as i32,
                ],
            },
        );
    }

    // ── Workspace transition ──────────────────────────────────────────────────

    /// Begin a workspace slide transition.
    /// `dir`: which direction the new workspace comes from.
    pub fn workspace_transition(&mut self, dir: WsDir) {
        self.ws_transition = Some(WsTransition {
            dir,
            start: Instant::now(),
            duration_ms: 250,
            screen_w: self.screen_w,
        });
    }

    /// Returns `(incoming_x, outgoing_x)` offsets for the current workspace transition,
    /// or `(0, 0)` if no transition is active.
    pub fn ws_offsets(&self) -> (i32, i32) {
        self.ws_transition
            .as_ref()
            .map(|t| (t.incoming_offset_x(), t.outgoing_offset_x()))
            .unwrap_or((0, 0))
    }

    /// True if a workspace transition is currently running.
    pub fn ws_transitioning(&self) -> bool {
        self.ws_transition.is_some()
    }

    // ── Tick ──────────────────────────────────────────────────────────────────

    /// Advance all animations. Returns `true` if any are still running.
    pub fn tick(&mut self) -> bool {
        self.panes.retain(|_, s| !s.is_done());
        if let Some(ref t) = self.ws_transition {
            if t.is_done() {
                self.ws_transition = None;
            }
        }
        !self.panes.is_empty() || self.ws_transition.is_some()
    }

    // ── Rect queries ──────────────────────────────────────────────────────────

    /// Returns the interpolated rect for `id` if it is currently animating,
    /// otherwise returns `twm_rect` unchanged.
    pub fn get_rect(&self, id: PaneId, twm_rect: Rect) -> Rect {
        self.panes
            .get(&id)
            .map(|s| s.current_rect())
            .unwrap_or(twm_rect)
    }

    /// True if `id` is mid-animation (any kind).
    pub fn is_animating(&self, id: PaneId) -> bool {
        self.panes.contains_key(&id)
    }

    /// True if any pane is currently animating.
    pub fn any_animating(&self) -> bool {
        !self.panes.is_empty() || self.ws_transition.is_some()
    }

    /// True if the pane is playing a close animation.
    pub fn is_closing(&self, id: PaneId) -> bool {
        self.panes
            .get(&id)
            .map(|s| s.kind == AnimKind::Close)
            .unwrap_or(false)
    }

    /// Returns the list of pane IDs currently in a layout morph.
    /// Used by render.rs to skip configure sends during the morph.
    pub fn morphing_panes(&self) -> Vec<PaneId> {
        self.panes
            .iter()
            .filter(|(_, s)| s.kind == AnimKind::LayoutMorph)
            .map(|(&id, _)| id)
            .collect()
    }
}

// ── Layout morph helper ───────────────────────────────────────────────────────

/// Compare two rect lists (indexed by pane order) and start morph animations
/// for any pane whose rect has changed.  Call this from `twm::reflow()` after
/// layout recalculation.
///
/// `old_rects`: (PaneId, Rect) before the layout change.
/// `new_rects`: (PaneId, Rect) after the layout change.
pub fn diff_and_morph(
    anim: &mut AnimSet,
    old_rects: &[(PaneId, Rect)],
    new_rects: &[(PaneId, Rect)],
) {
    let old_map: std::collections::HashMap<PaneId, Rect> = old_rects.iter().copied().collect();
    for &(id, new_rect) in new_rects {
        if let Some(&old_rect) = old_map.get(&id) {
            if old_rect != new_rect {
                anim.layout_morph(id, old_rect, new_rect);
            }
        }
    }
}
