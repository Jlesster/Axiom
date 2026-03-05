// twm/mod.rs — Trixie Window Manager state. Pure pixel coordinates throughout.

pub mod anim;
pub mod layout;
pub use anim::{AnimSet, WsDir};
pub use layout::Layout;

use std::collections::HashMap;

// ── Rect ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn contains(self, px: u32, py: u32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn center(self) -> (i32, i32) {
        ((self.x + self.w / 2) as i32, (self.y + self.h / 2) as i32)
    }
    pub fn inset(self, px: u32) -> Self {
        Self {
            x: self.x + px,
            y: self.y + px,
            w: self.w.saturating_sub(px * 2),
            h: self.h.saturating_sub(px * 2),
        }
    }
    pub fn clamp_to(self, bounds: Rect) -> Self {
        let x = self
            .x
            .max(bounds.x)
            .min(bounds.x + bounds.w.saturating_sub(self.w));
        let y = self
            .y
            .max(bounds.y)
            .min(bounds.y + bounds.h.saturating_sub(self.h));
        Self {
            x,
            y,
            w: self.w,
            h: self.h,
        }
    }
}

// ── PaneId ────────────────────────────────────────────────────────────────────

pub type PaneId = u32;

static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
pub fn new_pane_id() -> PaneId {
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ── PaneContent ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum PaneContent {
    Shell { app_id: String, title: String },
    Embedded { app_id: String },
    Empty,
}

impl PaneContent {
    pub fn app_id(&self) -> &str {
        match self {
            Self::Shell { app_id, .. } | Self::Embedded { app_id } => app_id,
            Self::Empty => "",
        }
    }
    pub fn title(&self) -> &str {
        match self {
            Self::Shell { title, .. } => title,
            Self::Embedded { app_id } => app_id,
            Self::Empty => "empty",
        }
    }
    pub fn is_embedded(&self) -> bool {
        matches!(self, Self::Embedded { .. })
    }
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

// ── Pane ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Pane {
    pub id: PaneId,
    pub content: PaneContent,
    pub rect: Rect,
    pub fullscreen: bool,
    pub floating: bool,
    /// Saved rect when floating — persists across workspace switches.
    pub float_rect: Rect,
}

impl Pane {
    pub fn new(content: PaneContent) -> Self {
        Self {
            id: new_pane_id(),
            content,
            rect: Rect::default(),
            fullscreen: false,
            floating: false,
            float_rect: Rect::default(),
        }
    }
}

// ── Scratchpad ────────────────────────────────────────────────────────────────

/// A named scratchpad. The pane is hidden (not in any workspace pane list)
/// when `visible = false`.
#[derive(Debug, Clone)]
pub struct Scratchpad {
    pub name: String,
    /// app_id to match when a new window is assigned here.
    pub app_id: String,
    pub pane_id: Option<PaneId>,
    pub visible: bool,
    /// Where to show it (centered + sized relative to screen).
    pub width_pct: f32,
    pub height_pct: f32,
}

impl Scratchpad {
    pub fn new(name: impl Into<String>, app_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            app_id: app_id.into(),
            pane_id: None,
            visible: false,
            width_pct: 0.6,
            height_pct: 0.6,
        }
    }

    pub fn with_size(mut self, w: f32, h: f32) -> Self {
        self.width_pct = w.clamp(0.1, 1.0);
        self.height_pct = h.clamp(0.1, 1.0);
        self
    }
}

// ── Workspace ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Workspace {
    pub index: usize,
    pub panes: Vec<PaneId>,
    pub focused: Option<PaneId>,
    pub layout: Layout,
    pub main_ratio: f32,
    pub gap: u32,
}

impl Workspace {
    pub fn new(index: usize, gap: u32) -> Self {
        Self {
            index,
            panes: Vec::new(),
            focused: None,
            layout: Layout::Bsp,
            main_ratio: 0.55,
            gap,
        }
    }

    pub fn focus_idx(&self) -> Option<usize> {
        let fid = self.focused?;
        self.panes.iter().position(|&p| p == fid)
    }

    pub fn cycle_focus(&mut self, delta: i32) {
        let n = self.panes.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.focus_idx().map(|i| i as i32).unwrap_or(0);
        self.focused = Some(self.panes[((cur + delta).rem_euclid(n)) as usize]);
    }

    pub fn swap_focused(&mut self, forward: bool) {
        let n = self.panes.len();
        if n < 2 {
            return;
        }
        let Some(cur) = self.focus_idx() else { return };
        let tgt = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        self.panes.swap(cur, tgt);
    }
}

// ── TwmAction ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TwmAction {
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    Close,
    Fullscreen,
    ToggleFloat,
    NextLayout,
    PrevLayout,
    GrowMain,
    ShrinkMain,
    Workspace(u8),
    MoveToWorkspace(u8),
    NextWorkspace,
    PrevWorkspace,
    ToggleBar,
    OpenShell(String),
    SetTitle(PaneId, String),
    AssignEmbedded(String),
    CloseAppId(String),
    // Floating move/resize — driven from input.rs during drag.
    FloatMove(PaneId, i32, i32),
    FloatResize(PaneId, i32, i32),
    // Scratchpad.
    ToggleScratchpad(String),
}

// ── TwmSnapshot ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TwmSnapshot {
    pub workspaces: Vec<WsSnap>,
    pub active_ws: usize,
    pub bar_rect: Rect,
    pub screen_rect: Rect,
    pub focused_id: Option<PaneId>,
}

#[derive(Debug, Clone)]
pub struct WsSnap {
    pub index: usize,
    pub panes: Vec<PaneSnap>,
    pub focused: Option<PaneId>,
    pub layout: &'static str,
    pub occupied: bool,
}

#[derive(Debug, Clone)]
pub struct PaneSnap {
    pub id: PaneId,
    pub rect: Rect,
    pub title: String,
    pub app_id: String,
    pub fullscreen: bool,
    pub floating: bool,
    pub is_embedded: bool,
}

// ── TwmState ─────────────────────────────────────────────────────────────────

pub struct TwmState {
    pub panes: HashMap<PaneId, Pane>,
    pub workspaces: Vec<Workspace>,
    pub active_ws: usize,
    pub screen_w: u32,
    pub screen_h: u32,
    pub content_rect: Rect,
    pub bar_rect: Rect,
    pub bar_visible: bool,
    pub gap: u32,
    pub border_w: u32,
    pub padding: u32,
    pub workspaces_count: u8,
    pub scratchpads: Vec<Scratchpad>,
}

impl TwmState {
    pub fn new(
        screen_w: u32,
        screen_h: u32,
        bar_h: u32,
        bar_at_bottom: bool,
        gap: u32,
        border_w: u32,
        padding: u32,
        workspaces_count: u8,
    ) -> Self {
        let (content_rect, bar_rect) =
            compute_rects(screen_w, screen_h, bar_h, bar_at_bottom, padding);
        let workspaces = (0..workspaces_count as usize)
            .map(|i| Workspace::new(i, gap))
            .collect();

        let mut s = Self {
            panes: HashMap::new(),
            workspaces,
            active_ws: 0,
            screen_w,
            screen_h,
            content_rect,
            bar_rect,
            bar_visible: true,
            gap,
            border_w,
            padding,
            workspaces_count,
            scratchpads: Vec::new(),
        };
        s.reflow();
        s
    }

    // ── Scratchpad registration ───────────────────────────────────────────────

    pub fn register_scratchpad(&mut self, name: impl Into<String>, app_id: impl Into<String>) {
        let name = name.into();
        if !self.scratchpads.iter().any(|s| s.name == name) {
            self.scratchpads.push(Scratchpad::new(name, app_id));
        }
    }

    pub fn register_scratchpad_sized(
        &mut self,
        name: impl Into<String>,
        app_id: impl Into<String>,
        w_pct: f32,
        h_pct: f32,
    ) {
        let name = name.into();
        if !self.scratchpads.iter().any(|s| s.name == name) {
            self.scratchpads
                .push(Scratchpad::new(name, app_id).with_size(w_pct, h_pct));
        }
    }

    /// Called when a new window opens — assigns it to a matching scratchpad
    /// if one exists and is unclaimed. Returns true if assigned.
    pub fn try_assign_scratchpad(&mut self, pane_id: PaneId, app_id: &str) -> bool {
        let idx = self
            .scratchpads
            .iter()
            .position(|s| s.app_id == app_id && s.pane_id.is_none());
        if let Some(i) = idx {
            self.scratchpads[i].pane_id = Some(pane_id);
            let rect = self.scratchpad_rect(&self.scratchpads[i].clone());
            if let Some(p) = self.panes.get_mut(&pane_id) {
                p.floating = true;
                p.rect = rect;
                p.float_rect = rect;
            }
            for ws in &mut self.workspaces {
                ws.panes.retain(|&id| id != pane_id);
                if ws.focused == Some(pane_id) {
                    ws.focused = ws.panes.last().copied();
                }
            }
            return true;
        }
        false
    }

    fn scratchpad_rect(&self, sp: &Scratchpad) -> Rect {
        let w = (self.screen_w as f32 * sp.width_pct) as u32;
        let h = (self.screen_h as f32 * sp.height_pct) as u32;
        let x = (self.screen_w.saturating_sub(w)) / 2;
        let y = (self.screen_h.saturating_sub(h)) / 2;
        Rect::new(x, y, w, h)
    }

    pub fn toggle_scratchpad(&mut self, name: &str) {
        let idx = match self.scratchpads.iter().position(|s| s.name == name) {
            Some(i) => i,
            None => {
                tracing::warn!("toggle_scratchpad: no scratchpad named '{name}'");
                return;
            }
        };

        let sp = self.scratchpads[idx].clone();

        if sp.pane_id.is_none() {
            tracing::debug!("toggle_scratchpad '{name}': no pane assigned yet");
            return;
        }

        let pane_id = sp.pane_id.unwrap();

        if sp.visible {
            let ws = &mut self.workspaces[self.active_ws];
            ws.panes.retain(|&id| id != pane_id);
            if ws.focused == Some(pane_id) {
                ws.focused = ws.panes.last().copied();
            }
            self.scratchpads[idx].visible = false;
        } else {
            let rect = self.scratchpad_rect(&sp);
            if let Some(p) = self.panes.get_mut(&pane_id) {
                p.rect = rect;
                p.float_rect = rect;
            }
            for ws in &mut self.workspaces {
                ws.panes.retain(|&id| id != pane_id);
            }
            let ws = &mut self.workspaces[self.active_ws];
            ws.panes.push(pane_id);
            ws.focused = Some(pane_id);
            self.scratchpads[idx].visible = true;
        }

        self.reflow();
    }

    // ── Resize ────────────────────────────────────────────────────────────────

    pub fn resize(&mut self, screen_w: u32, screen_h: u32) {
        self.screen_w = screen_w;
        self.screen_h = screen_h;
        let bar_h = self.bar_rect.h;
        let bar_at_bottom = self.bar_rect.y > 0;
        let (cr, br) = compute_rects(screen_w, screen_h, bar_h, bar_at_bottom, self.padding);
        self.content_rect = cr;
        self.bar_rect = br;
        self.reflow();
    }

    pub fn set_bar_height(&mut self, h: u32, at_bottom: bool) {
        let (cr, br) = compute_rects(self.screen_w, self.screen_h, h, at_bottom, self.padding);
        self.content_rect = cr;
        self.bar_rect = br;
        self.reflow();
    }

    // ── Pane management ───────────────────────────────────────────────────────

    pub fn open_shell(&mut self, app_id: &str) -> PaneId {
        let p = Pane::new(PaneContent::Shell {
            app_id: app_id.to_owned(),
            title: app_id.to_owned(),
        });
        let id = p.id;
        tracing::info!(
            "open_shell: inserting pane id={id}, panes before={}",
            self.panes.len()
        );
        self.panes.insert(id, p);
        let ws = &mut self.workspaces[self.active_ws];
        ws.panes.push(id);
        ws.focused = Some(id);
        self.reflow();
        id
    }

    pub fn assign_embedded(&mut self, app_id: &str) -> PaneId {
        let focused = self.workspaces[self.active_ws].focused;
        if let Some(fid) = focused {
            if let Some(p) = self.panes.get_mut(&fid) {
                if p.content.is_empty() {
                    p.content = PaneContent::Embedded {
                        app_id: app_id.to_owned(),
                    };
                    self.reflow();
                    return fid;
                }
            }
        }
        let p = Pane::new(PaneContent::Embedded {
            app_id: app_id.to_owned(),
        });
        let id = p.id;
        self.panes.insert(id, p);
        let ws = &mut self.workspaces[self.active_ws];
        ws.panes.push(id);
        ws.focused = Some(id);
        self.reflow();
        id
    }

    pub fn set_title(&mut self, id: PaneId, title: String) {
        if let Some(p) = self.panes.get_mut(&id) {
            if let PaneContent::Shell { title: t, .. } = &mut p.content {
                *t = title;
            }
        }
    }

    pub fn close_pane(&mut self, id: PaneId) {
        self.panes.remove(&id);
        for ws in &mut self.workspaces {
            ws.panes.retain(|&p| p != id);
            if ws.focused == Some(id) {
                ws.focused = ws.panes.last().copied();
            }
        }
        for sp in &mut self.scratchpads {
            if sp.pane_id == Some(id) {
                sp.pane_id = None;
                sp.visible = false;
            }
        }
        self.reflow();
    }

    pub fn close_by_app_id(&mut self, app_id: &str) {
        let ids: Vec<PaneId> = self
            .panes
            .values()
            .filter(|p| p.content.app_id() == app_id)
            .map(|p| p.id)
            .collect();
        for id in ids {
            self.close_pane(id);
        }
    }

    // ── Float ─────────────────────────────────────────────────────────────────

    pub fn toggle_float(&mut self) {
        let Some(id) = self.focused_id() else { return };
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.floating = !pane.floating;
        if pane.floating {
            if pane.float_rect.is_empty() {
                pane.float_rect = pane.rect;
            }
            pane.rect = pane.float_rect;
        }
        self.reflow();
    }

    /// Move a floating pane by (dx, dy) in pixels.
    pub fn float_move(&mut self, id: PaneId, dx: i32, dy: i32) {
        let screen = Rect::new(0, 0, self.screen_w, self.screen_h);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        if !pane.floating {
            return;
        }
        let nx = (pane.rect.x as i32 + dx).max(0) as u32;
        let ny = (pane.rect.y as i32 + dy).max(0) as u32;
        pane.rect.x = nx.min(screen.w.saturating_sub(pane.rect.w));
        pane.rect.y = ny.min(screen.h.saturating_sub(pane.rect.h));
        pane.float_rect = pane.rect;
    }

    /// Resize a floating pane by (dw, dh) in pixels.
    pub fn float_resize(&mut self, id: PaneId, dw: i32, dh: i32) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        if !pane.floating {
            return;
        }
        let nw = (pane.rect.w as i32 + dw).max(80) as u32;
        let nh = (pane.rect.h as i32 + dh).max(60) as u32;
        pane.rect.w = nw.min(self.screen_w);
        pane.rect.h = nh.min(self.screen_h);
        pane.float_rect = pane.rect;
    }

    // ── Focus ─────────────────────────────────────────────────────────────────

    pub fn focused_id(&self) -> Option<PaneId> {
        self.workspaces[self.active_ws].focused
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.focused_id().and_then(|id| self.panes.get(&id))
    }

    pub fn pane_by_app_id(&self, app_id: &str) -> Option<&Pane> {
        self.panes.values().find(|p| p.content.app_id() == app_id)
    }

    pub fn set_focused(&mut self, id: PaneId) {
        let ws = &mut self.workspaces[self.active_ws];
        if ws.panes.contains(&id) {
            ws.focused = Some(id);
        }
    }

    fn focus_dir(&mut self, dx: i32, dy: i32) {
        let ws = &self.workspaces[self.active_ws];
        let Some(cur_id) = ws.focused else { return };
        let Some(cur) = self.panes.get(&cur_id) else {
            return;
        };
        let (cx, cy) = cur.rect.center();

        let best = ws
            .panes
            .iter()
            .filter(|&&id| id != cur_id)
            .filter_map(|&id| self.panes.get(&id).map(|p| (id, p.rect)))
            .filter(|(_, r)| {
                let (nx, ny) = r.center();
                (nx - cx) * dx + (ny - cy) * dy > 0
            })
            .min_by_key(|(_, r)| {
                let (nx, ny) = r.center();
                (nx - cx).pow(2) + (ny - cy).pow(2)
            })
            .map(|(id, _)| id);

        if let Some(id) = best {
            self.workspaces[self.active_ws].focused = Some(id);
        }
    }

    // ── Layout morph snapshot ─────────────────────────────────────────────────

    /// Snapshot all current pane rects on the active workspace.
    /// Call BEFORE reflow() to capture pre-change positions, then AFTER to get
    /// post-change positions, and pass both to `anim::diff_and_morph()`.
    pub fn pane_rects_snapshot(&self) -> Vec<(PaneId, Rect)> {
        let ws = &self.workspaces[self.active_ws];
        ws.panes
            .iter()
            .filter_map(|&id| self.panes.get(&id).map(|p| (id, p.rect)))
            .collect()
    }

    // ── Actions ───────────────────────────────────────────────────────────────

    pub fn dispatch(&mut self, action: TwmAction) {
        match action {
            TwmAction::FocusLeft => self.focus_dir(-1, 0),
            TwmAction::FocusRight => self.focus_dir(1, 0),
            TwmAction::FocusUp => self.focus_dir(0, -1),
            TwmAction::FocusDown => self.focus_dir(0, 1),

            TwmAction::MoveLeft | TwmAction::MoveUp => {
                self.workspaces[self.active_ws].swap_focused(false)
            }
            TwmAction::MoveRight | TwmAction::MoveDown => {
                self.workspaces[self.active_ws].swap_focused(true)
            }

            TwmAction::Close => {
                if let Some(id) = self.focused_id() {
                    self.close_pane(id);
                }
                return;
            }

            TwmAction::Fullscreen => {
                if let Some(id) = self.focused_id() {
                    if let Some(p) = self.panes.get_mut(&id) {
                        p.fullscreen = !p.fullscreen;
                    }
                }
            }

            TwmAction::ToggleFloat => {
                self.toggle_float();
                return;
            }

            TwmAction::FloatMove(id, dx, dy) => {
                self.float_move(id, dx, dy);
                return;
            }

            TwmAction::FloatResize(id, dw, dh) => {
                self.float_resize(id, dw, dh);
                return;
            }

            TwmAction::ToggleScratchpad(name) => {
                self.toggle_scratchpad(&name);
                return;
            }

            TwmAction::NextLayout => {
                self.workspaces[self.active_ws].layout =
                    self.workspaces[self.active_ws].layout.next()
            }
            TwmAction::PrevLayout => {
                self.workspaces[self.active_ws].layout =
                    self.workspaces[self.active_ws].layout.prev()
            }

            TwmAction::GrowMain => {
                let ws = &mut self.workspaces[self.active_ws];
                ws.main_ratio = (ws.main_ratio + 0.05).min(0.9);
            }
            TwmAction::ShrinkMain => {
                let ws = &mut self.workspaces[self.active_ws];
                ws.main_ratio = (ws.main_ratio - 0.05).max(0.1);
            }

            TwmAction::Workspace(n) => {
                // Hide any visible scratchpads when switching workspaces.
                for sp in &mut self.scratchpads {
                    if sp.visible {
                        if let Some(pid) = sp.pane_id {
                            let ws = &mut self.workspaces[self.active_ws];
                            ws.panes.retain(|&id| id != pid);
                            if ws.focused == Some(pid) {
                                ws.focused = ws.panes.last().copied();
                            }
                        }
                        sp.visible = false;
                    }
                }
                self.active_ws = (n as usize)
                    .saturating_sub(1)
                    .min(self.workspaces.len() - 1);
            }

            TwmAction::MoveToWorkspace(n) => {
                let idx = (n as usize)
                    .saturating_sub(1)
                    .min(self.workspaces.len() - 1);
                if idx != self.active_ws {
                    if let Some(id) = self.focused_id() {
                        self.workspaces[self.active_ws].panes.retain(|&p| p != id);
                        self.workspaces[self.active_ws].focused =
                            self.workspaces[self.active_ws].panes.last().copied();
                        self.workspaces[idx].panes.push(id);
                        self.workspaces[idx].focused = Some(id);
                    }
                }
            }

            TwmAction::NextWorkspace => {
                self.active_ws = (self.active_ws + 1) % self.workspaces.len();
            }
            TwmAction::PrevWorkspace => {
                let n = self.workspaces.len();
                self.active_ws = (self.active_ws + n - 1) % n;
            }

            TwmAction::ToggleBar => {
                self.bar_visible = !self.bar_visible;
                let bar_h = if self.bar_visible { self.bar_rect.h } else { 0 };
                let at_bottom = self.bar_rect.y > 0;
                let (cr, _) =
                    compute_rects(self.screen_w, self.screen_h, bar_h, at_bottom, self.padding);
                self.content_rect = cr;
            }

            TwmAction::OpenShell(app_id) => {
                self.open_shell(&app_id);
                return;
            }
            TwmAction::AssignEmbedded(app_id) => {
                self.assign_embedded(&app_id);
                return;
            }
            TwmAction::CloseAppId(app_id) => {
                self.close_by_app_id(&app_id);
                return;
            }
            TwmAction::SetTitle(id, title) => {
                self.set_title(id, title);
                return;
            }
        }
        self.reflow();
    }

    // ── Layout / reflow ───────────────────────────────────────────────────────

    pub fn reflow(&mut self) {
        let content = self.content_rect;
        let gap = self.gap;
        let ws = &self.workspaces[self.active_ws];

        if ws.panes.is_empty() {
            return;
        }

        let tiled: Vec<PaneId> = ws
            .panes
            .iter()
            .copied()
            .filter(|id| {
                self.panes
                    .get(id)
                    .map_or(false, |p| !p.floating && !p.fullscreen)
            })
            .collect();

        let rects = if tiled.is_empty() {
            vec![]
        } else {
            layout::compute(ws.layout, content, tiled.len(), ws.main_ratio, gap)
        };

        for (i, &pid) in tiled.iter().enumerate() {
            if let Some(pane) = self.panes.get_mut(&pid) {
                pane.rect = rects.get(i).copied().unwrap_or(content);
            }
        }

        // Fullscreen panes cover the entire screen.
        for &pid in &ws.panes {
            if let Some(p) = self.panes.get_mut(&pid) {
                if p.fullscreen {
                    p.rect = Rect::new(0, 0, self.screen_w, self.screen_h);
                }
                // Floating panes keep their float_rect (set by toggle/drag).
                if p.floating && !p.fullscreen {
                    p.rect = p.float_rect;
                }
            }
        }
    }

    // ── Snapshot ──────────────────────────────────────────────────────────────

    pub fn snapshot(&self) -> TwmSnapshot {
        let ws = &self.workspaces[self.active_ws];
        let focused = ws.focused;

        let ws_snaps = self
            .workspaces
            .iter()
            .map(|w| {
                let panes = w
                    .panes
                    .iter()
                    .filter_map(|&id| {
                        let p = self.panes.get(&id)?;
                        Some(PaneSnap {
                            id: p.id,
                            rect: p.rect,
                            title: p.content.title().to_owned(),
                            app_id: p.content.app_id().to_owned(),
                            fullscreen: p.fullscreen,
                            floating: p.floating,
                            is_embedded: p.content.is_embedded(),
                        })
                    })
                    .collect();
                WsSnap {
                    index: w.index,
                    panes,
                    focused: w.focused,
                    layout: w.layout.label(),
                    occupied: !w.panes.is_empty(),
                }
            })
            .collect();

        TwmSnapshot {
            workspaces: ws_snaps,
            active_ws: self.active_ws,
            bar_rect: if self.bar_visible {
                self.bar_rect
            } else {
                Rect::default()
            },
            screen_rect: Rect::new(0, 0, self.screen_w, self.screen_h),
            focused_id: focused,
        }
    }

    pub fn embedded_rects(&self) -> Vec<(String, Rect)> {
        let ws = &self.workspaces[self.active_ws];
        ws.panes
            .iter()
            .filter_map(|&id| {
                let p = self.panes.get(&id)?;
                if let PaneContent::Embedded { app_id } = &p.content {
                    Some((app_id.clone(), p.rect))
                } else {
                    None
                }
            })
            .collect()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn compute_rects(sw: u32, sh: u32, bar_h: u32, bar_at_bottom: bool, padding: u32) -> (Rect, Rect) {
    let (content, bar) = if bar_h == 0 {
        (Rect::new(0, 0, sw, sh), Rect::new(0, 0, 0, 0))
    } else {
        let bar_h = bar_h.min(sh);
        let content_h = sh - bar_h;
        if bar_at_bottom {
            (
                Rect::new(0, 0, sw, content_h),
                Rect::new(0, content_h, sw, bar_h),
            )
        } else {
            (
                Rect::new(0, bar_h, sw, content_h),
                Rect::new(0, 0, sw, bar_h),
            )
        }
    };

    let padded = if padding > 0 {
        content.inset(padding)
    } else {
        content
    };
    (padded, bar)
}
