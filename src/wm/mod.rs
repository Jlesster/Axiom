use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type WindowId = u32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn inset(&self, a: i32) -> Self {
        Self {
            x: self.x + a,
            y: self.y + a,
            w: (self.w - 2 * a).max(0),
            h: (self.h - 2 * a).max(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    MasterStack,
    Bsp,
    Monocle,
    Float,
}
impl Layout {
    pub fn from_str(s: &str) -> Self {
        match s {
            "bsp" => Self::Bsp,
            "monocle" => Self::Monocle,
            "float" => Self::Float,
            _ => Self::MasterStack,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Window {
    pub id: WindowId,
    pub app_id: String,
    pub title: String,
    pub rect: Rect,
    pub floating: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub workspace: usize,
    saved_rect: Option<Rect>,
}
impl Window {
    fn new(id: WindowId, ws: usize) -> Self {
        Self {
            id,
            app_id: String::new(),
            title: String::new(),
            rect: Rect::default(),
            floating: false,
            fullscreen: false,
            maximized: false,
            workspace: ws,
            saved_rect: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub windows: Vec<WindowId>,
    pub focused: Option<WindowId>,
    pub layout: Layout,
    pub master_count: usize,
    pub master_ratio: f32,
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            windows: vec![],
            focused: None,
            layout: Layout::default(),
            master_count: 1,
            master_ratio: 0.55,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub active_ws: usize,
    pub bar_height: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WmConfig {
    pub border_w: u32,
    pub gap: u32,
    pub outer_gap: u32,
    pub bar_height: u32,
    pub workspaces_count: usize,
    pub bar_at_bottom: bool,
    pub active_border: [u8; 4],
    pub inactive_border: [u8; 4],
    pub bar_bg: [u8; 4],
}
impl Default for WmConfig {
    fn default() -> Self {
        Self {
            border_w: 2,
            gap: 6,
            outer_gap: 0,
            bar_height: 24,
            workspaces_count: 9,
            bar_at_bottom: false,
            active_border: [122, 162, 247, 255], // #7aa2f7
            inactive_border: [59, 66, 97, 255],  // #3b4261
            bar_bg: [30, 30, 46, 255],           // #1e1e2e
        }
    }
}
impl WmConfig {
    pub fn active_border_f32(&self) -> [f32; 4] {
        u8x4_to_f32(self.active_border)
    }
    pub fn inactive_border_f32(&self) -> [f32; 4] {
        u8x4_to_f32(self.inactive_border)
    }
    pub fn bar_bg_f32(&self) -> [f32; 4] {
        u8x4_to_f32(self.bar_bg)
    }

    /// Called from lua_api hex colours
    pub fn set_active_border_rgba(&mut self, rgba: [f32; 4]) {
        self.active_border = f32x4_to_u8(rgba);
    }
    pub fn set_inactive_border_rgba(&mut self, rgba: [f32; 4]) {
        self.inactive_border = f32x4_to_u8(rgba);
    }
    pub fn set_bar_bg_rgba(&mut self, rgba: [f32; 4]) {
        self.bar_bg = f32x4_to_u8(rgba);
    }
}

fn u8x4_to_f32(c: [u8; 4]) -> [f32; 4] {
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}
fn f32x4_to_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8,
        (c[3] * 255.0) as u8,
    ]
}

pub struct WmState {
    pub config: WmConfig,
    pub workspaces: Vec<Workspace>,
    pub windows: HashMap<WindowId, Window>,
    pub monitors: Vec<Monitor>,
    active_ws: usize,
    next_id: WindowId,
}

impl WmState {
    pub fn new() -> Self {
        let cfg = WmConfig::default();
        let ws_count = cfg.workspaces_count;
        Self {
            config: cfg,
            workspaces: (0..ws_count).map(|_| Workspace::default()).collect(),
            windows: HashMap::new(),
            monitors: vec![],
            active_ws: 0,
            next_id: 1,
        }
    }

    pub fn active_ws(&self) -> usize {
        self.active_ws
    }
    pub fn focused_window(&self) -> Option<WindowId> {
        self.workspaces.get(self.active_ws)?.focused
    }
    pub fn window(&self, id: WindowId) -> &Window {
        self.windows.get(&id).unwrap()
    }
    pub fn window_mut(&mut self, id: WindowId) -> &mut Window {
        self.windows.get_mut(&id).unwrap()
    }

    pub fn add_window(&mut self) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;
        let ws = self.active_ws;
        self.windows.insert(id, Window::new(id, ws));
        self.workspaces[ws].windows.push(id);
        if self.workspaces[ws].focused.is_none() {
            self.workspaces[ws].focused = Some(id);
        }
        self.reflow();
        id
    }

    pub fn remove_window(&mut self, id: WindowId) {
        if let Some(win) = self.windows.remove(&id) {
            let ws = win.workspace;
            if let Some(wsp) = self.workspaces.get_mut(ws) {
                wsp.windows.retain(|&w| w != id);
                if wsp.focused == Some(id) {
                    wsp.focused = wsp.windows.last().copied();
                }
            }
        }
        self.reflow();
    }

    pub fn focus_window(&mut self, id: WindowId) {
        if let Some(win) = self.windows.get(&id) {
            let ws = win.workspace;
            self.active_ws = ws;
            if let Some(wsp) = self.workspaces.get_mut(ws) {
                wsp.focused = Some(id);
            }
        }
    }

    pub fn switch_workspace(&mut self, ws: usize) {
        if ws < self.workspaces.len() {
            self.active_ws = ws;
            if let Some(mon) = self.monitors.first_mut() {
                mon.active_ws = ws;
            }
        }
    }

    pub fn move_to_workspace(&mut self, id: WindowId, ws: usize) {
        if ws >= self.workspaces.len() {
            return;
        }
        if let Some(win) = self.windows.get_mut(&id) {
            let old = win.workspace;
            win.workspace = ws;
            if let Some(w) = self.workspaces.get_mut(old) {
                w.windows.retain(|&x| x != id);
                if w.focused == Some(id) {
                    w.focused = w.windows.last().copied();
                }
            }
            self.workspaces[ws].windows.push(id);
        }
        self.reflow();
    }

    pub fn fullscreen_window(&mut self, id: WindowId, on: bool) {
        if let Some(win) = self.windows.get_mut(&id) {
            if on && win.saved_rect.is_none() {
                win.saved_rect = Some(win.rect);
            }
            win.fullscreen = on;
            if !on {
                if let Some(r) = win.saved_rect.take() {
                    win.rect = r;
                }
            }
        }
        self.reflow();
    }

    pub fn toggle_float(&mut self, id: WindowId) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.floating = !win.floating;
        }
        self.reflow();
    }

    pub fn set_title(&mut self, id: WindowId, title: String) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.title = title;
        }
    }
    pub fn set_app_id(&mut self, id: WindowId, app_id: String) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.app_id = app_id;
        }
    }

    pub fn inc_master(&mut self) {
        if let Some(ws) = self.workspaces.get_mut(self.active_ws) {
            ws.master_count += 1;
        }
        self.reflow();
    }
    pub fn dec_master(&mut self) {
        if let Some(ws) = self.workspaces.get_mut(self.active_ws) {
            ws.master_count = ws.master_count.saturating_sub(1).max(1);
        }
        self.reflow();
    }

    pub fn focus_direction(&mut self, dir: u8) {
        let Some(fid) = self.focused_window() else {
            return;
        };
        let cur = self.windows[&fid].rect;
        let ws_idx = self.active_ws;
        let best = self.workspaces[ws_idx]
            .windows
            .iter()
            .copied()
            .filter(|&id| id != fid)
            .filter_map(|id| {
                let r = self.windows[&id].rect;
                let ok = match dir {
                    0 => r.x + r.w <= cur.x,
                    1 => r.x >= cur.x + cur.w,
                    2 => r.y + r.h <= cur.y,
                    3 => r.y >= cur.y + cur.h,
                    _ => false,
                };
                if ok {
                    let dx = (r.x + r.w / 2) - (cur.x + cur.w / 2);
                    let dy = (r.y + r.h / 2) - (cur.y + cur.h / 2);
                    Some((id, dx * dx + dy * dy))
                } else {
                    None
                }
            })
            .min_by_key(|&(_, d)| d)
            .map(|(id, _)| id);
        if let Some(id) = best {
            self.workspaces[ws_idx].focused = Some(id);
        }
    }

    pub fn cycle_focus(&mut self, delta: i32) {
        let ws = &mut self.workspaces[self.active_ws];
        if ws.windows.is_empty() {
            return;
        }
        let cur = ws
            .focused
            .and_then(|f| ws.windows.iter().position(|&id| id == f))
            .unwrap_or(0);
        let n = ws.windows.len() as i32;
        ws.focused = Some(ws.windows[((cur as i32 + delta).rem_euclid(n)) as usize]);
    }

    pub fn add_monitor(&mut self, x: i32, y: i32, w: i32, h: i32) -> usize {
        let idx = self.monitors.len();
        self.monitors.push(Monitor {
            x,
            y,
            width: w,
            height: h,
            active_ws: idx.min(self.workspaces.len() - 1),
            bar_height: self.config.bar_height as i32,
        });
        idx
    }

    pub fn apply_config(&mut self, cfg: WmConfig) {
        let ws_count = cfg.workspaces_count;
        self.config = cfg;
        while self.workspaces.len() < ws_count {
            self.workspaces.push(Workspace::default());
        }
        self.reflow();
    }

    pub fn reflow(&mut self) {
        let mon = self.monitors.first().cloned().unwrap_or(Monitor {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            active_ws: 0,
            bar_height: 24,
        });
        let cfg = self.config.clone();
        let ws_count = self.workspaces.len();

        for ws_idx in 0..ws_count {
            let tiled: Vec<WindowId> = self.workspaces[ws_idx]
                .windows
                .iter()
                .copied()
                .filter(|id| {
                    self.windows
                        .get(id)
                        .map(|w| !w.floating && !w.fullscreen)
                        .unwrap_or(false)
                })
                .collect();

            let layout = self.workspaces[ws_idx].layout;
            let bar_h = cfg.bar_height as i32;
            let (ax, ay, aw, ah) = if cfg.bar_at_bottom {
                (
                    mon.x + cfg.outer_gap as i32,
                    mon.y + cfg.outer_gap as i32,
                    mon.width - 2 * cfg.outer_gap as i32,
                    mon.height - bar_h - 2 * cfg.outer_gap as i32,
                )
            } else {
                (
                    mon.x + cfg.outer_gap as i32,
                    mon.y + bar_h + cfg.outer_gap as i32,
                    mon.width - 2 * cfg.outer_gap as i32,
                    mon.height - bar_h - 2 * cfg.outer_gap as i32,
                )
            };

            let rects = compute_layout(
                layout,
                &tiled,
                ax,
                ay,
                aw,
                ah,
                cfg.gap as i32,
                cfg.border_w as i32,
                self.workspaces[ws_idx].master_count,
                self.workspaces[ws_idx].master_ratio,
            );

            for (id, rect) in tiled.iter().zip(rects.iter()) {
                if let Some(w) = self.windows.get_mut(id) {
                    w.rect = *rect;
                }
            }

            // Fullscreen windows
            let fs: Vec<WindowId> = self.workspaces[ws_idx]
                .windows
                .iter()
                .copied()
                .filter(|id| self.windows.get(id).map(|w| w.fullscreen).unwrap_or(false))
                .collect();
            for id in fs {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.rect = Rect::new(mon.x, mon.y, mon.width, mon.height);
                }
            }
        }
    }
}

fn compute_layout(
    layout: Layout,
    ids: &[WindowId],
    ax: i32,
    ay: i32,
    aw: i32,
    ah: i32,
    gap: i32,
    _border: i32,
    master_count: usize,
    master_ratio: f32,
) -> Vec<Rect> {
    let n = ids.len();
    if n == 0 {
        return vec![];
    }
    match layout {
        Layout::MasterStack => {
            let mc = master_count.min(n);
            let sc = n - mc;
            let mut rects = vec![];
            if sc == 0 {
                let each_h = (ah - gap * (mc as i32 - 1)) / mc as i32;
                for i in 0..mc {
                    rects.push(Rect::new(ax, ay + i as i32 * (each_h + gap), aw, each_h));
                }
            } else {
                let mw = ((aw as f32 * master_ratio) as i32 - gap / 2).max(1);
                let sw = aw - mw - gap;
                let each_mh = (ah - gap * (mc as i32 - 1)) / mc as i32;
                for i in 0..mc {
                    rects.push(Rect::new(ax, ay + i as i32 * (each_mh + gap), mw, each_mh));
                }
                let each_sh = (ah - gap * (sc as i32 - 1)) / sc as i32;
                for i in 0..sc {
                    rects.push(Rect::new(
                        ax + mw + gap,
                        ay + i as i32 * (each_sh + gap),
                        sw,
                        each_sh,
                    ));
                }
            }
            rects
        }
        Layout::Bsp => bsp_layout(ax, ay, aw, ah, gap, n),
        Layout::Monocle => vec![Rect::new(ax, ay, aw, ah); n],
        Layout::Float => ids
            .iter()
            .enumerate()
            .map(|(i, _)| Rect::new(ax + i as i32 * 30, ay + i as i32 * 30, aw / 2, ah / 2))
            .collect(),
    }
}

fn bsp_layout(x: i32, y: i32, w: i32, h: i32, gap: i32, n: usize) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![Rect::new(x, y, w, h)];
    }
    let half = n / 2;
    let (r1, r2) = if w >= h {
        let lw = (w - gap) / 2;
        ((x, y, lw, h), (x + lw + gap, y, w - lw - gap, h))
    } else {
        let th = (h - gap) / 2;
        ((x, y, w, th), (x, y + th + gap, w, h - th - gap))
    };
    let mut rects = bsp_layout(r1.0, r1.1, r1.2, r1.3, gap, half);
    rects.extend(bsp_layout(r2.0, r2.1, r2.2, r2.3, gap, n - half));
    rects
}
