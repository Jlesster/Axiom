// src/wm/mod.rs — pure window manager state.

pub mod anim;
pub mod layout;
pub mod rules;

pub use anim::AnimSet;
pub use layout::Layout;
pub use rules::{Effect, Matcher, RuleEngine, WindowRule};

use std::collections::HashMap;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::Resource as _;

// ── Rect ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn inset(self, px: i32) -> Self {
        Self {
            x: self.x + px,
            y: self.y + px,
            w: (self.w - px * 2).max(1),
            h: (self.h - px * 2).max(1),
        }
    }
    pub fn center(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
    pub fn is_zero(self) -> bool {
        self.w == 0 && self.h == 0
    }
}

// ── WindowId ──────────────────────────────────────────────────────────────────

pub type WindowId = u32;
static NEXT_WIN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
pub fn new_window_id() -> WindowId {
    NEXT_WIN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ── Window ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Window {
    pub id: WindowId,
    pub app_id: String,
    pub title: String,
    pub rect: Rect,
    pub float_rect: Rect,
    pub floating: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub surface_id: u32,
    pub surface: Option<WlSurface>,
}

impl Window {
    pub fn new(surface_id: u32, app_id: String) -> Self {
        let id = new_window_id();
        Self {
            id,
            surface_id,
            title: app_id.clone(),
            app_id,
            rect: Rect::default(),
            float_rect: Rect::default(),
            floating: false,
            fullscreen: false,
            maximized: false,
            surface: None,
        }
    }
    pub fn inner_rect(&self, border_w: i32) -> Rect {
        if self.fullscreen || border_w == 0 {
            self.rect
        } else {
            self.rect.inset(border_w)
        }
    }
}

// ── Workspace ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Workspace {
    pub index: usize,
    pub windows: Vec<WindowId>,
    pub focused: Option<WindowId>,
    pub layout: Layout,
    pub main_ratio: f32,
    pub gap: i32,
    pub master_n: usize,
}

impl Workspace {
    pub fn new(index: usize, gap: i32) -> Self {
        Self {
            index,
            windows: Vec::new(),
            focused: None,
            layout: Layout::MasterStack,
            main_ratio: 0.55,
            gap,
            master_n: 1,
        }
    }
    pub fn focus_idx(&self) -> Option<usize> {
        let f = self.focused?;
        self.windows.iter().position(|&w| w == f)
    }
    pub fn cycle_focus(&mut self, delta: i32) {
        let n = self.windows.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.focus_idx().map(|i| i as i32).unwrap_or(0);
        self.focused = Some(self.windows[((cur + delta).rem_euclid(n)) as usize]);
    }
}

// ── WmConfig ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WmConfig {
    pub border_w: i32,
    pub gap: i32,
    pub bar_height: i32,
    pub bar_at_bottom: bool,
    pub active_border: [f32; 4],
    pub inactive_border: [f32; 4],
    pub bar_bg: [f32; 4],
    pub workspaces_count: usize,
    pub rules: Vec<rules::WindowRule>,
}

impl Default for WmConfig {
    fn default() -> Self {
        Self {
            border_w: 2,
            gap: 6,
            bar_height: 0,
            bar_at_bottom: false,
            active_border: [0.706, 0.745, 0.996, 1.0],
            inactive_border: [0.271, 0.278, 0.353, 1.0],
            bar_bg: [0.094, 0.094, 0.141, 1.0],
            workspaces_count: 9,
            rules: Vec::new(),
        }
    }
}

// ── Interactive grab ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GrabKind {
    Move,
    Resize,
}

#[derive(Debug, Clone)]
pub struct InteractiveGrab {
    pub win_id: WindowId,
    pub kind: GrabKind,
    pub start_x: f64,
    pub start_y: f64,
    pub start_rect: Rect,
}

// ── Scratchpad ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Scratchpad {
    pub name: String,
    pub app_id: String,
    pub win_id: Option<WindowId>,
    pub visible: bool,
    pub w_pct: f32,
    pub h_pct: f32,
}

// ── Monitor ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Monitor {
    pub output_id: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub active_ws: usize,
    pub usable: Rect,
}

impl Monitor {
    pub fn new(output_id: u32, x: i32, y: i32, w: i32, h: i32, first_ws: usize) -> Self {
        Self {
            output_id,
            x,
            y,
            width: w,
            height: h,
            active_ws: first_ws,
            usable: Rect::new(x, y, w, h),
        }
    }
    pub fn geometry(&self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

// ── WmState ───────────────────────────────────────────────────────────────────

pub struct WmState {
    pub windows: HashMap<WindowId, Window>,
    pub workspaces: Vec<Workspace>,
    pub active_monitor: usize,
    pub monitors: Vec<Monitor>,
    pub screen_w: i32,
    pub screen_h: i32,
    pub content: Rect,
    pub config: WmConfig,
    pub scratchpads: HashMap<String, Scratchpad>,
    pub grab: Option<InteractiveGrab>,
}

impl WmState {
    pub fn new(screen_w: i32, screen_h: i32, cfg: WmConfig) -> Self {
        let n = cfg.workspaces_count;
        let workspaces = (0..n).map(|i| Workspace::new(i, cfg.gap)).collect();
        let monitor = Monitor::new(0, 0, 0, screen_w, screen_h, 0);
        let content = Rect::new(0, 0, screen_w, screen_h);
        let mut s = Self {
            windows: HashMap::new(),
            workspaces,
            active_monitor: 0,
            monitors: vec![monitor],
            screen_w,
            screen_h,
            content,
            config: cfg,
            scratchpads: HashMap::new(),
            grab: None,
        };
        s.reflow();
        s
    }

    // ── Monitor management ────────────────────────────────────────────────────

    pub fn add_monitor(&mut self, output_id: u32, x: i32, y: i32, w: i32, h: i32) {
        let used: std::collections::HashSet<usize> =
            self.monitors.iter().map(|m| m.active_ws).collect();
        let ws_idx = (0..self.workspaces.len())
            .find(|i| !used.contains(i))
            .unwrap_or(self.workspaces.len() - 1);
        self.monitors
            .push(Monitor::new(output_id, x, y, w, h, ws_idx));
        self.reflow();
    }

    pub fn remove_monitor(&mut self, output_id: u32) {
        if let Some(idx) = self.monitors.iter().position(|m| m.output_id == output_id) {
            self.monitors.swap_remove(idx);
            self.active_monitor = self
                .active_monitor
                .min(self.monitors.len().saturating_sub(1));
            self.reflow();
        }
    }

    pub fn update_monitor_usable(&mut self, output_id: u32, usable: Rect) {
        if let Some(m) = self.monitors.iter_mut().find(|m| m.output_id == output_id) {
            m.usable = usable;
            self.reflow();
        }
    }

    pub fn monitor_at(&self, x: i32, y: i32) -> usize {
        self.monitors
            .iter()
            .position(|m| m.geometry().contains(x, y))
            .unwrap_or(self.active_monitor)
    }

    pub fn active_ws(&self) -> usize {
        self.monitors
            .get(self.active_monitor)
            .map(|m| m.active_ws)
            .unwrap_or(0)
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        self.screen_w = w;
        self.screen_h = h;
        if let Some(m) = self.monitors.first_mut() {
            m.width = w;
            m.height = h;
            m.usable = Rect::new(0, 0, w, h);
        }
        self.reflow();
    }

    // ── Focus ─────────────────────────────────────────────────────────────────

    pub fn focused_window(&self) -> Option<WindowId> {
        self.workspaces[self.active_ws()].focused
    }
    pub fn set_focused(&mut self, id: WindowId) {
        let aws = self.active_ws();
        let ws = &mut self.workspaces[aws];
        if ws.windows.contains(&id) {
            ws.focused = Some(id);
        }
    }
    pub fn focus_window(&mut self, id: WindowId) {
        self.set_focused(id);
    }
    pub fn window(&self, id: WindowId) -> &Window {
        self.windows.get(&id).expect("window not found")
    }
    pub fn window_mut(&mut self, id: WindowId) -> &mut Window {
        self.windows.get_mut(&id).expect("window not found")
    }

    pub fn window_at(&self, px: i32, py: i32) -> Option<WindowId> {
        let aws = self.active_ws();
        let ws = &self.workspaces[aws];
        let (tiled, floating): (Vec<_>, Vec<_>) = ws
            .windows
            .iter()
            .copied()
            .partition(|&id| self.windows.get(&id).map(|w| !w.floating).unwrap_or(true));
        for &id in floating.iter().rev().chain(tiled.iter().rev()) {
            if self
                .windows
                .get(&id)
                .map(|w| w.rect.contains(px, py))
                .unwrap_or(false)
            {
                return Some(id);
            }
        }
        None
    }

    // ── Spatial helpers ───────────────────────────────────────────────────────

    fn nearest_in_dir(&self, cur_id: WindowId, dx: i32, dy: i32) -> Option<WindowId> {
        let aws = self.active_ws();
        let ws = &self.workspaces[aws];
        let (cx, cy) = self.windows.get(&cur_id)?.rect.center();
        ws.windows
            .iter()
            .filter(|&&id| id != cur_id)
            .filter_map(|&id| self.windows.get(&id).map(|w| (id, w.rect)))
            .filter(|(_, r)| {
                let (nx, ny) = r.center();
                (nx - cx) * dx + (ny - cy) * dy > 0
            })
            .min_by_key(|(_, r)| {
                let (nx, ny) = r.center();
                (nx - cx).pow(2) + (ny - cy).pow(2)
            })
            .map(|(id, _)| id)
    }

    pub fn focus_in_dir(&mut self, dx: i32, dy: i32) {
        let Some(cur_id) = self.workspaces[self.active_ws()].focused else {
            return;
        };
        if let Some(id) = self.nearest_in_dir(cur_id, dx, dy) {
            let aws = self.active_ws();
            self.workspaces[aws].focused = Some(id);
        }
    }
    pub fn focus_direction(&mut self, dir: u8) {
        let (dx, dy) = dir_to_vec(dir);
        self.focus_in_dir(dx, dy);
    }
    pub fn move_direction(&mut self, dir: u8) {
        let aws = self.active_ws();
        let Some(cur_id) = self.workspaces[aws].focused else {
            return;
        };
        let (dx, dy) = dir_to_vec(dir);
        if let Some(target_id) = self.nearest_in_dir(cur_id, dx, dy) {
            let ws = &mut self.workspaces[aws];
            let ai = ws.windows.iter().position(|&w| w == cur_id).unwrap();
            let bi = ws.windows.iter().position(|&w| w == target_id).unwrap();
            ws.windows.swap(ai, bi);
            self.reflow();
        }
    }
    pub fn relayout_focused_workspace(&mut self) {
        self.reflow();
    }

    // ── Window management ─────────────────────────────────────────────────────

    pub fn add_window(&mut self, win: Window) -> WindowId {
        let id = win.id;
        let aid = win.app_id.clone();
        let ttl = win.title.clone();
        let floating =
            self.config.rules.iter().any(|r| {
                r.matcher.matches(&aid, &ttl) && r.effects.contains(&rules::Effect::Float)
            });
        let mut win = win;
        win.floating = floating;
        self.windows.insert(id, win);
        let aws = self.active_ws();
        let ws = &mut self.workspaces[aws];
        ws.windows.push(id);
        ws.focused = Some(id);
        self.reflow();
        id
    }

    pub fn remove_window(&mut self, id: WindowId) {
        self.windows.remove(&id);
        for ws in &mut self.workspaces {
            ws.windows.retain(|&w| w != id);
            if ws.focused == Some(id) {
                ws.focused = ws.windows.last().copied();
            }
        }
        for sp in self.scratchpads.values_mut() {
            if sp.win_id == Some(id) {
                sp.win_id = None;
                sp.visible = false;
            }
        }
        self.reflow();
    }

    pub fn set_title(&mut self, id: WindowId, title: String) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.title = title;
        }
    }

    pub fn toggle_float(&mut self, id: WindowId) {
        let Some(win) = self.windows.get_mut(&id) else {
            return;
        };
        win.floating = !win.floating;
        if win.floating {
            if win.float_rect.is_zero() {
                let r = win.rect;
                win.float_rect = if r.is_zero() {
                    let w = (self.screen_w / 2).max(400);
                    let h = (self.screen_h / 2).max(300);
                    Rect::new(
                        ((self.screen_w - w) / 2).max(0),
                        ((self.screen_h - h) / 2).max(0),
                        w,
                        h,
                    )
                } else {
                    r
                };
            }
            win.rect = win.float_rect;
        }
        self.reflow();
    }

    pub fn toggle_fullscreen(&mut self, id: WindowId) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.fullscreen = !win.fullscreen;
        }
        self.reflow();
    }

    pub fn move_float(&mut self, id: WindowId, dx: i32, dy: i32) {
        let Some(win) = self.windows.get_mut(&id) else {
            return;
        };
        if !win.floating {
            return;
        }
        win.rect.x = (win.rect.x + dx).max(0).min(self.screen_w - win.rect.w);
        win.rect.y = (win.rect.y + dy).max(0).min(self.screen_h - win.rect.h);
        win.float_rect = win.rect;
    }

    pub fn resize_float(&mut self, id: WindowId, dw: i32, dh: i32) {
        let Some(win) = self.windows.get_mut(&id) else {
            return;
        };
        if !win.floating {
            return;
        }
        win.rect.w = (win.rect.w + dw).max(80).min(self.screen_w);
        win.rect.h = (win.rect.h + dh).max(60).min(self.screen_h);
        win.float_rect = win.rect;
    }

    // ── Master count ──────────────────────────────────────────────────────────

    pub fn inc_master(&mut self) {
        let aws = self.active_ws();
        self.workspaces[aws].master_n = self.workspaces[aws].master_n.saturating_add(1);
    }
    pub fn dec_master(&mut self) {
        let aws = self.active_ws();
        self.workspaces[aws].master_n = self.workspaces[aws].master_n.saturating_sub(1).max(1);
    }

    // ── Workspace switching ───────────────────────────────────────────────────

    pub fn switch_workspace(&mut self, n: usize) {
        let n = n.min(self.workspaces.len() - 1);
        let am = self.active_monitor;
        self.monitors[am].active_ws = n;
        self.reflow();
    }

    pub fn move_to_workspace(&mut self, win_id: WindowId, n: usize) {
        let n = n.min(self.workspaces.len() - 1);
        let aws = self.active_ws();
        if n == aws {
            return;
        }
        let ws = &mut self.workspaces[aws];
        ws.windows.retain(|&w| w != win_id);
        if ws.focused == Some(win_id) {
            ws.focused = ws.windows.last().copied();
        }
        let ws = &mut self.workspaces[n];
        ws.windows.push(win_id);
        ws.focused = Some(win_id);
        self.reflow();
    }

    // ── Interactive grab ──────────────────────────────────────────────────────

    pub fn start_move(&mut self, win_id: WindowId, start_x: f64, start_y: f64) {
        let rect = self
            .windows
            .get(&win_id)
            .map(|w| w.rect)
            .unwrap_or_default();
        self.grab = Some(InteractiveGrab {
            win_id,
            kind: GrabKind::Move,
            start_x,
            start_y,
            start_rect: rect,
        });
    }
    pub fn start_resize(&mut self, win_id: WindowId, start_x: f64, start_y: f64) {
        let rect = self
            .windows
            .get(&win_id)
            .map(|w| w.rect)
            .unwrap_or_default();
        self.grab = Some(InteractiveGrab {
            win_id,
            kind: GrabKind::Resize,
            start_x,
            start_y,
            start_rect: rect,
        });
    }
    pub fn update_grab(&mut self, px: f64, py: f64) {
        let Some(grab) = &self.grab else { return };
        let (dx, dy) = ((px - grab.start_x) as i32, (py - grab.start_y) as i32);
        let (id, base, kind) = (grab.win_id, grab.start_rect, grab.kind);
        match kind {
            GrabKind::Move => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.rect.x = base.x + dx;
                    w.rect.y = base.y + dy;
                    w.float_rect = w.rect;
                    w.floating = true;
                }
            }
            GrabKind::Resize => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.rect.w = (base.w + dx).max(80);
                    w.rect.h = (base.h + dy).max(60);
                    w.float_rect = w.rect;
                    w.floating = true;
                }
            }
        }
    }
    pub fn end_grab(&mut self) {
        self.grab = None;
    }

    // ── Scratchpads ───────────────────────────────────────────────────────────

    pub fn register_scratchpad(&mut self, name: String, app_id: String, w: f32, h: f32) {
        self.scratchpads.entry(name.clone()).or_insert(Scratchpad {
            name,
            app_id,
            win_id: None,
            visible: false,
            w_pct: w,
            h_pct: h,
        });
    }

    pub fn toggle_scratchpad(&mut self, name: &str) {
        // Read active_ws before taking a mutable borrow on scratchpads.
        let aws = self.active_ws();

        let Some(sp) = self.scratchpads.get_mut(name) else {
            return;
        };
        let Some(win_id) = sp.win_id else { return };
        if sp.visible {
            let ws = &mut self.workspaces[aws];
            ws.windows.retain(|&w| w != win_id);
            if ws.focused == Some(win_id) {
                ws.focused = ws.windows.last().copied();
            }
            sp.visible = false;
        } else {
            let (w_pct, h_pct) = (sp.w_pct, sp.h_pct);
            let w = (self.screen_w as f32 * w_pct) as i32;
            let h = (self.screen_h as f32 * h_pct) as i32;
            let x = ((self.screen_w - w) / 2).max(0);
            let y = ((self.screen_h - h) / 2).max(0);
            sp.visible = true;
            // Drop the scratchpad borrow before touching windows/workspaces.
            drop(sp);
            if let Some(win) = self.windows.get_mut(&win_id) {
                win.rect = Rect::new(x, y, w, h);
                win.float_rect = win.rect;
                win.floating = true;
            }
            for ws in &mut self.workspaces {
                ws.windows.retain(|&w| w != win_id);
            }
            let ws = &mut self.workspaces[aws];
            ws.windows.push(win_id);
            ws.focused = Some(win_id);
        }
        self.reflow();
    }

    // ── Reflow ────────────────────────────────────────────────────────────────

    pub fn reflow(&mut self) {
        // Collect everything we need from monitors+workspaces in one pass,
        // producing plain data with no borrows into self, before we touch
        // self.windows mutably.
        struct Task {
            usable: Rect,
            ox: i32,
            oy: i32,
            sw: i32,
            sh: i32,
            layout: Layout,
            ratio: f32,
            gap: i32,
            tiled: Vec<WindowId>,
            all_wins: Vec<WindowId>,
        }

        let tasks: Vec<Task> = self
            .monitors
            .iter()
            .map(|m| {
                let ws = &self.workspaces[m.active_ws];
                let tiled: Vec<WindowId> = ws
                    .windows
                    .iter()
                    .copied()
                    .filter(|&id| {
                        self.windows
                            .get(&id)
                            .map(|w| !w.floating && !w.fullscreen)
                            .unwrap_or(false)
                    })
                    .collect();
                Task {
                    usable: m.usable,
                    ox: m.x,
                    oy: m.y,
                    sw: m.width,
                    sh: m.height,
                    layout: ws.layout,
                    ratio: ws.main_ratio,
                    gap: ws.gap,
                    tiled,
                    all_wins: ws.windows.clone(),
                }
            })
            .collect();

        // Now mutate self.windows freely — no borrows on monitors/workspaces.
        for task in &tasks {
            let rects = layout::compute(
                task.layout,
                task.usable,
                task.tiled.len(),
                task.ratio,
                task.gap,
            );
            for (i, &id) in task.tiled.iter().enumerate() {
                if let Some(win) = self.windows.get_mut(&id) {
                    win.rect = rects.get(i).copied().unwrap_or(task.usable);
                }
            }
            for &id in &task.all_wins {
                if let Some(win) = self.windows.get_mut(&id) {
                    if win.fullscreen {
                        win.rect = Rect::new(task.ox, task.oy, task.sw, task.sh);
                    } else if win.floating && !win.float_rect.is_zero() {
                        win.rect = win.float_rect;
                    }
                }
            }
        }

        if let Some(m) = self.monitors.first() {
            self.content = m.usable;
        }
    }

    pub fn rect_snapshot(&self) -> Vec<(WindowId, Rect)> {
        let aws = self.active_ws();
        self.workspaces[aws]
            .windows
            .iter()
            .filter_map(|&id| self.windows.get(&id).map(|w| (id, w.rect)))
            .collect()
    }

    // ── xdg_shell shims ───────────────────────────────────────────────────────

    pub fn new_toplevel_pending(
        &mut self,
        _toplevel: wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel,
    ) {
    }

    pub fn set_window_geometry(&mut self, id: WindowId, _x: i32, _y: i32, w: i32, h: i32) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.rect.w = w;
            win.rect.h = h;
        }
    }
    pub fn set_window_title(&mut self, id: WindowId, title: String) {
        self.set_title(id, title);
    }
    pub fn set_window_app_id(&mut self, id: WindowId, app_id: String) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.app_id = app_id;
        }
    }
    pub fn set_window_parent(&mut self, _id: WindowId, _parent: Option<WindowId>) {}
    pub fn maximize_window(&mut self, id: WindowId, on: bool) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.maximized = on;
            if on {
                win.floating = false;
            }
        }
        self.reflow();
    }
    pub fn fullscreen_window(&mut self, id: WindowId, on: bool) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.fullscreen = on;
        }
        self.reflow();
    }
    pub fn minimize_window(&mut self, id: WindowId) {
        let aws = self.active_ws();
        let ws = &mut self.workspaces[aws];
        ws.windows.retain(|&w| w != id);
        if ws.focused == Some(id) {
            ws.focused = ws.windows.last().copied();
        }
        self.reflow();
    }
}

fn dir_to_vec(dir: u8) -> (i32, i32) {
    match dir {
        0 => (-1, 0),
        1 => (1, 0),
        2 => (0, -1),
        3 => (0, 1),
        _ => (0, 0),
    }
}
