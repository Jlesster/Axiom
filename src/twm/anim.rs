// twm/anim.rs — slide-from-edge open/close animations.
//
// AnimSet tracks one AnimState per PaneId.
//
// Open:  slide in from nearest screen edge, 160 ms, cubic ease-out.
// Close: slide out to nearest edge, 120 ms, cubic ease-in.
//        The actual twm.close_pane() call is deferred until after the
//        animation finishes — handlers.rs inserts a 150 ms one-shot timer.
//
// render.rs calls get_rect(id, twm_rect) to obtain the interpolated rect
// for a pane that is currently animating, falling back to twm_rect when
// the pane is idle.

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

// ── Edge ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

fn nearest_edge(rect: Rect, screen_w: u32, screen_h: u32) -> Edge {
    // Use the pane's center position relative to the screen center.
    // This gives a consistent, visible slide direction regardless of pane size —
    // the old distance-to-edge approach produced nearly invisible animations for
    // large panes because they were equidistant from all edges.
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;

    // Signed offset from screen center, normalised to [0,1] range per axis.
    let dx = cx as f32 / screen_w.max(1) as f32 - 0.5; // negative = left half
    let dy = cy as f32 / screen_h.max(1) as f32 - 0.5; // negative = top half

    // Fullscreen pane is centered on the screen — dx and dy are both zero,
    // so there's no meaningful direction. Slide from the bottom.
    if dx == 0.0 && dy == 0.0 {
        return Edge::Bottom;
    }

    // Pick the axis with the larger offset, then the sign gives the edge.
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

/// Compute the off-screen starting rect for an open animation,
/// or the off-screen ending rect for a close animation.
/// Uses signed coordinates so off-screen positions don't wrap.
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

// Lerp between two signed [x,y,w,h] rects, storing result in Rect (u32 wrapping).
// Negative x/y during slide-in wraps in u32 — render.rs casts back to i32 for
// positioning so the wrapping cancels out and the slide is correct on screen.
fn lerp_rect_render(a: [i32; 4], b: [i32; 4], t: f32) -> Rect {
    let x = lerp_i32(a[0], b[0], t) as u32;
    let y = lerp_i32(a[1], b[1], t) as u32;
    let w = lerp_i32(a[2], b[2], t).max(1) as u32;
    let h = lerp_i32(a[3], b[3], t).max(1) as u32;
    Rect::new(x, y, w, h)
}

// ── AnimState ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimKind {
    Open,
    Close,
}

#[derive(Debug, Clone)]
struct AnimState {
    kind: AnimKind,
    start: Instant,
    duration_ms: u64,
    /// The pane's final on-screen rect (open destination / close source).
    target_rect: Rect,
    /// The off-screen rect as signed [x, y, w, h] so negative coords don't wrap.
    edge_rect: [i32; 4],
}

impl AnimState {
    fn progress(&self) -> f32 {
        let elapsed = self.start.elapsed().as_millis() as f32;
        let total = self.duration_ms as f32;
        (elapsed / total).clamp(0.0, 1.0)
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
            AnimKind::Open => {
                let eased = ease_out_cubic(t);
                lerp_rect_render(self.edge_rect, target, eased)
            }
            AnimKind::Close => {
                let eased = ease_in_cubic(t);
                lerp_rect_render(target, self.edge_rect, eased)
            }
        }
    }
}

// ── AnimSet ───────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct AnimSet {
    anims: std::collections::HashMap<PaneId, AnimState>,
    screen_w: u32,
    screen_h: u32,
}

impl AnimSet {
    pub fn resize(&mut self, screen_w: u32, screen_h: u32) {
        self.screen_w = screen_w;
        self.screen_h = screen_h;
    }

    /// Begin an open animation for `id` sliding in to `rect`.
    pub fn open(&mut self, id: PaneId, rect: Rect) {
        let edge = nearest_edge(rect, self.screen_w, self.screen_h);
        let off = edge_rect_signed(rect, edge, self.screen_w, self.screen_h);
        self.anims.insert(
            id,
            AnimState {
                kind: AnimKind::Open,
                start: Instant::now(),
                duration_ms: 160,
                target_rect: rect,
                edge_rect: off,
            },
        );
    }

    /// Begin a close animation for `id` sliding out from `rect`.
    pub fn close(&mut self, id: PaneId, rect: Rect) {
        let edge = nearest_edge(rect, self.screen_w, self.screen_h);
        let off = edge_rect_signed(rect, edge, self.screen_w, self.screen_h);
        self.anims.insert(
            id,
            AnimState {
                kind: AnimKind::Close,
                start: Instant::now(),
                duration_ms: 120,
                target_rect: rect,
                edge_rect: off,
            },
        );
    }

    /// Advance all animations. Returns `true` if any animation is still running
    /// (i.e. a follow-up frame should be scheduled).
    pub fn tick(&mut self) -> bool {
        self.anims.retain(|_, s| !s.is_done());
        !self.anims.is_empty()
    }

    /// Returns the interpolated rect for `id` if it is currently animating,
    /// otherwise returns `twm_rect` unchanged.
    pub fn get_rect(&self, id: PaneId, twm_rect: Rect) -> Rect {
        self.anims
            .get(&id)
            .map(|s| s.current_rect())
            .unwrap_or(twm_rect)
    }

    /// True if `id` is mid-animation (used to skip layout syncing during close).
    pub fn is_animating(&self, id: PaneId) -> bool {
        self.anims.contains_key(&id)
    }

    /// True if any pane is currently animating (used to keep the render loop alive).
    pub fn any_animating(&self) -> bool {
        !self.anims.is_empty()
    }

    /// True if the pane is playing a close animation
    /// (handlers.rs uses this to defer twm.close_pane()).
    pub fn is_closing(&self, id: PaneId) -> bool {
        self.anims
            .get(&id)
            .map(|s| s.kind == AnimKind::Close)
            .unwrap_or(false)
    }
}
