// twm/mod.rs — Trixie Window Manager state. Pure pixel coordinates throughout.

pub mod anim;
pub mod layout;
pub use anim::AnimSet;
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
    /// Content area: screen minus bar minus outer padding.
    pub content_rect: Rect,
    pub bar_rect: Rect,
    pub bar_visible: bool,
    pub gap: u32,
    pub border_w: u32,
    /// Outer padding in pixels — insets the entire tiling area from all edges.
    pub padding: u32,
    pub workspaces_count: u8,
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
        };
        s.reflow();
        s
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

        // Fullscreen panes cover the full content area (ignoring padding).
        for &pid in &ws.panes {
            if let Some(p) = self.panes.get_mut(&pid) {
                if p.fullscreen {
                    p.rect = Rect::new(0, 0, self.screen_w, self.screen_h);
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

/// Compute content and bar rects, then inset content by `padding` on all sides.
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

    // Inset the content area by padding on all four sides.
    let padded = if padding > 0 {
        content.inset(padding)
    } else {
        content
    };
    (padded, bar)
}
