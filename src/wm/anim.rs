// src/wm/anim.rs — Window animation system (spring physics).

use std::collections::HashMap;
use std::time::Instant;

use super::{Rect, WindowId};

#[derive(Clone, Debug)]
pub struct Spring {
    pub value: f64,
    pub target: f64,
    pub vel: f64,
    pub stiff: f64,
    pub damp: f64,
}

impl Spring {
    pub fn new(value: f64, stiff: f64, damp: f64) -> Self {
        let damp = if damp == 0.0 {
            2.0 * stiff.sqrt()
        } else {
            damp
        };
        Self {
            value,
            target: value,
            vel: 0.0,
            stiff,
            damp,
        }
    }
    pub fn snappy(v: f64) -> Self {
        Self::new(v, 320.0, 0.0)
    }
    pub fn smooth(v: f64) -> Self {
        Self::new(v, 80.0, 0.0)
    }
    pub fn bouncy(v: f64) -> Self {
        Self::new(v, 200.0, 18.0)
    }

    pub fn set_target(&mut self, t: f64) {
        self.target = t;
    }
    pub fn snap(&mut self, v: f64) {
        self.value = v;
        self.target = v;
        self.vel = 0.0;
    }
    pub fn as_i32(&self) -> i32 {
        self.value.round() as i32
    }
    pub fn as_f32_clamped(&self) -> f32 {
        self.value.clamp(0.0, 1.0) as f32
    }

    pub fn tick(&mut self, dt: f64) -> bool {
        let delta = self.target - self.value;
        if delta.abs() < 0.5 && self.vel.abs() < 0.5 {
            self.value = self.target;
            self.vel = 0.0;
            return false;
        }
        let accel = self.stiff * delta - self.damp * self.vel;
        self.vel += accel * dt;
        self.value += self.vel * dt;
        true
    }
}

impl Default for Spring {
    fn default() -> Self {
        Self::snappy(0.0)
    }
}

#[derive(Debug)]
pub struct WindowAnim {
    pub x: Spring,
    pub y: Spring,
    pub w: Spring,
    pub h: Spring,
    /// Fade-in/out opacity (0 → 1 on open, 1 → 0 on close).
    pub opacity: Spring,
    /// True while the close animation is running — renderer skips the window
    /// once opacity reaches zero.
    pub closing: bool,
}

impl WindowAnim {
    /// Create a new animation **already at the correct geometry**.
    /// Opacity starts at 0 and springs to 1 for a smooth fade-in.
    /// Geometry springs are snapped to the real values so the window never
    /// slides in from (0,0).
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        let mut a = Self {
            x: Spring::snappy(x as f64),
            y: Spring::snappy(y as f64),
            w: Spring::snappy(w as f64),
            h: Spring::snappy(h as f64),
            opacity: Spring::smooth(0.0),
            closing: false,
        };
        // Geometry is already at the target — snap so springs are settled.
        a.x.snap(x as f64);
        a.y.snap(y as f64);
        a.w.snap(w as f64);
        a.h.snap(h as f64);
        // Kick off the fade-in.
        a.opacity.set_target(1.0);
        a
    }

    pub fn set_geometry(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.x.set_target(x as f64);
        self.y.set_target(y as f64);
        self.w.set_target(w as f64);
        self.h.set_target(h as f64);
    }

    /// Snap geometry instantly (no spring travel) — use when a window first
    /// appears so it doesn't slide from its old position.
    pub fn snap_geometry(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.x.snap(x as f64);
        self.y.snap(y as f64);
        self.w.snap(w as f64);
        self.h.snap(h as f64);
    }

    pub fn close(&mut self) {
        self.closing = true;
        self.opacity.set_target(0.0);
    }

    pub fn is_done_closing(&self) -> bool {
        self.closing && self.opacity.value < 0.01
    }

    pub fn tick(&mut self, dt: f64) -> bool {
        let mut m = false;
        m |= self.x.tick(dt);
        m |= self.y.tick(dt);
        m |= self.w.tick(dt);
        m |= self.h.tick(dt);
        m |= self.opacity.tick(dt);
        m
    }

    pub fn current_rect(&self) -> (i32, i32, i32, i32) {
        (
            self.x.as_i32(),
            self.y.as_i32(),
            self.w.as_i32(),
            self.h.as_i32(),
        )
    }
}

#[derive(Debug)]
pub struct AnimSet {
    pub windows: HashMap<WindowId, WindowAnim>,
    pub workspace_offset: Spring,
    last_tick: Instant,
}

impl Default for AnimSet {
    fn default() -> Self {
        Self::new()
    }
}

impl AnimSet {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
            workspace_offset: Spring::bouncy(0.0),
            last_tick: Instant::now(),
        }
    }

    pub fn insert(&mut self, id: WindowId, x: i32, y: i32, w: i32, h: i32) {
        self.windows.insert(id, WindowAnim::new(x, y, w, h));
    }

    pub fn remove(&mut self, id: WindowId) {
        self.windows.remove(&id);
    }

    pub fn begin_close(&mut self, id: WindowId) {
        if let Some(a) = self.windows.get_mut(&id) {
            a.close();
        }
    }

    /// Update geometry target.  If the window has no anim entry yet, create
    /// one snapped to the given rect (no slide-from-origin).
    pub fn set_geometry(&mut self, id: WindowId, x: i32, y: i32, w: i32, h: i32) {
        match self.windows.get_mut(&id) {
            Some(a) => {
                // If the anim was previously at (0,0,0,0) — i.e. the window
                // was just created and reflow fired before set_geometry —
                // snap rather than spring so it doesn't fly across the screen.
                if a.w.target < 1.0 && a.h.target < 1.0 {
                    a.snap_geometry(x, y, w, h);
                } else {
                    a.set_geometry(x, y, w, h);
                }
            }
            None => {
                self.insert(id, x, y, w, h);
            }
        }
    }

    /// Returns true if any animation is still running.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;

        // Remove windows whose close animation has fully faded out.
        self.windows.retain(|_, a| !a.is_done_closing());

        let mut any = false;
        for anim in self.windows.values_mut() {
            any |= anim.tick(dt);
        }
        any |= self.workspace_offset.tick(dt);
        any
    }

    pub fn tick_dt(&mut self, dt: f64) -> bool {
        self.windows.retain(|_, a| !a.is_done_closing());
        let mut any = false;
        for anim in self.windows.values_mut() {
            any |= anim.tick(dt);
        }
        any |= self.workspace_offset.tick(dt);
        any
    }

    /// Get the current animated rect, falling back to the WM rect.
    pub fn get_rect(&self, id: WindowId, fallback: Rect) -> Rect {
        if let Some(a) = self.windows.get(&id) {
            let (x, y, w, h) = a.current_rect();
            // Never return a degenerate rect — fall back if the anim hasn't
            // settled to a valid size yet.
            if w > 0 && h > 0 {
                return Rect::new(x, y, w, h);
            }
        }
        fallback
    }

    /// Current opacity for a window (0.0–1.0).
    pub fn get_opacity(&self, id: WindowId) -> f32 {
        self.windows
            .get(&id)
            .map(|a| a.opacity.as_f32_clamped())
            .unwrap_or(1.0)
    }

    pub fn begin_workspace_switch(&mut self, dx: f64) {
        self.workspace_offset.snap(dx);
        self.workspace_offset.set_target(0.0);
    }
}
