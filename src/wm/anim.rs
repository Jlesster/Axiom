// src/wm/anim.rs — Window animation system (spring physics).
// See previous session for Spring implementation details.

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
    pub opacity: Spring,
    pub scale: Spring,
}

impl WindowAnim {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        let mut a = Self {
            x: Spring::snappy(x as f64),
            y: Spring::snappy(y as f64),
            w: Spring::snappy(w as f64),
            h: Spring::snappy(h as f64),
            opacity: Spring::smooth(0.0),
            scale: Spring::snappy(0.9),
        };
        a.show();
        a
    }
    pub fn set_geometry(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.x.set_target(x as f64);
        self.y.set_target(y as f64);
        self.w.set_target(w as f64);
        self.h.set_target(h as f64);
    }
    pub fn show(&mut self) {
        self.opacity.set_target(1.0);
        self.scale.set_target(1.0);
    }
    pub fn close(&mut self) {
        self.opacity.set_target(0.0);
        self.scale.set_target(0.92);
    }
    pub fn tick(&mut self, dt: f64) -> bool {
        let mut m = false;
        m |= self.x.tick(dt);
        m |= self.y.tick(dt);
        m |= self.w.tick(dt);
        m |= self.h.tick(dt);
        m |= self.opacity.tick(dt);
        m |= self.scale.tick(dt);
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
    /// Tracks when the last tick happened so tick() can compute dt internally.
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

    pub fn set_geometry(&mut self, id: WindowId, x: i32, y: i32, w: i32, h: i32) {
        if let Some(a) = self.windows.get_mut(&id) {
            a.set_geometry(x, y, w, h);
        } else {
            self.insert(id, x, y, w, h);
        }
    }

    /// No-arg tick — computes dt from wall clock. Returns true if still animating.
    /// Called by main.rs as `state.anim.tick()`.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        let mut any = false;
        for anim in self.windows.values_mut() {
            any |= anim.tick(dt);
        }
        any |= self.workspace_offset.tick(dt);
        any
    }

    /// Explicit-dt tick for tests.
    pub fn tick_dt(&mut self, dt: f64) -> bool {
        let mut any = false;
        for anim in self.windows.values_mut() {
            any |= anim.tick(dt);
        }
        any |= self.workspace_offset.tick(dt);
        any
    }

    /// Get the current animated rect for a window, falling back to the WM rect.
    /// Called by renderer as `state.anim.get_rect(win_id, win.rect)`.
    pub fn get_rect(&self, id: WindowId, fallback: Rect) -> Rect {
        if let Some(a) = self.windows.get(&id) {
            let (x, y, w, h) = a.current_rect();
            Rect::new(x, y, w, h)
        } else {
            fallback
        }
    }

    pub fn begin_workspace_switch(&mut self, dx: f64) {
        self.workspace_offset.snap(dx);
        self.workspace_offset.set_target(0.0);
    }
}
