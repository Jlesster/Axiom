// chrome/bar.rs — bar module trait + built-in modules.

use trixui::layout::Rect;
use trixui::{PixColor, PixelCanvas, TextStyle};

use crate::config::{BarModuleDef, BarModuleKind, Color, Colors};
use crate::twm::TwmSnapshot;

// ── Pixel helpers ─────────────────────────────────────────────────────────────

fn col(c: Color) -> PixColor {
    c.to_trixui()
}

fn text_style(fg: Color, bg: Color) -> TextStyle {
    TextStyle {
        fg: col(fg),
        bg: col(bg),
        bold: false,
        italic: false,
    }
}

fn bold_style(fg: Color, bg: Color) -> TextStyle {
    TextStyle {
        fg: col(fg),
        bg: col(bg),
        bold: true,
        italic: false,
    }
}

// ── BarModule trait ──────────────────────────────────────────────────────────

pub trait BarModule: Send + Sync {
    fn min_width(&self, cell_w: u32) -> u32;
    fn draw(
        &self,
        canvas: &mut PixelCanvas,
        rect: Rect,
        snap: &TwmSnapshot,
        colors: &Colors,
        cell_w: u32,
        cell_h: u32,
    );
}

// ── WorkspacesModule ──────────────────────────────────────────────────────────

pub struct WorkspacesModule {
    pub active_fg: Color,
    pub active_bg: Color,
    pub inactive_fg: Color,
    pub inactive_bg: Color,
    pub occupied_fg: Color,
    pub padding: u32,
}

impl WorkspacesModule {
    pub fn new(colors: &Colors) -> Self {
        Self {
            active_fg: Color::hex(0x11111b),
            active_bg: colors.bar_accent,
            inactive_fg: colors.bar_fg,
            inactive_bg: Color::rgba(0, 0, 0, 0),
            occupied_fg: colors.active_border,
            padding: 6,
        }
    }
    pub fn from_def(def: &BarModuleDef, colors: &Colors) -> Self {
        let mut m = Self::new(colors);
        if let Some(v) = def.props.get("active_fg") {
            if let Some(c) = v.as_color() {
                m.active_fg = c.into();
            }
        }
        if let Some(v) = def.props.get("active_bg") {
            if let Some(c) = v.as_color() {
                m.active_bg = c.into();
            }
        }
        if let Some(v) = def.props.get("inactive_fg") {
            if let Some(c) = v.as_color() {
                m.inactive_fg = c.into();
            }
        }
        if let Some(v) = def.props.get("padding") {
            if let Some(n) = v.as_px() {
                m.padding = n;
            }
        }
        m
    }
}

impl BarModule for WorkspacesModule {
    fn min_width(&self, cell_w: u32) -> u32 {
        9 * (cell_w + self.padding * 2)
    }

    fn draw(
        &self,
        canvas: &mut PixelCanvas,
        rect: Rect,
        snap: &TwmSnapshot,
        _colors: &Colors,
        cell_w: u32,
        cell_h: u32,
    ) {
        let mut x = rect.x;
        let ty = rect.y + rect.h.saturating_sub(cell_h) / 2;

        for ws in &snap.workspaces {
            let label = format!(" {} ", ws.index + 1);
            let lw = label.chars().count() as u32 * cell_w;
            let bw = lw + self.padding * 2;
            if x + bw > rect.x + rect.w {
                break;
            }

            let is_active = ws.index == snap.active_ws;
            let is_occupied = ws.occupied;

            let (fg, bg) = if is_active {
                (self.active_fg, self.active_bg)
            } else {
                let fg = if is_occupied {
                    self.occupied_fg
                } else {
                    self.inactive_fg
                };
                (fg, self.inactive_bg)
            };

            if bg.a > 0 {
                canvas.fill(x, rect.y, bw, rect.h, col(bg));
            }
            canvas.text_maxw(x + self.padding, ty, &label, text_style(fg, bg), lw + 2);
            x += bw;
        }
    }
}

// ── ClockModule ───────────────────────────────────────────────────────────────

pub struct ClockModule {
    pub format: String,
    pub fg: Color,
    pub bg: Color,
}

impl ClockModule {
    pub fn new(colors: &Colors) -> Self {
        Self {
            format: "%a %b %-e  %H:%M".into(),
            fg: colors.bar_fg,
            bg: Color::rgba(0, 0, 0, 0),
        }
    }
    pub fn from_def(def: &BarModuleDef, colors: &Colors) -> Self {
        let mut m = Self::new(colors);
        if let Some(v) = def.props.get("format") {
            if let Some(s) = v.as_str() {
                m.format = s.to_string();
            }
        }
        if let Some(v) = def.props.get("fg") {
            if let Some(c) = v.as_color() {
                m.fg = c.into();
            }
        }
        m
    }
    fn current_text(&self) -> String {
        chrono::Local::now().format(&self.format).to_string()
    }
}

impl BarModule for ClockModule {
    fn min_width(&self, cell_w: u32) -> u32 {
        (self.current_text().chars().count() as u32 + 2) * cell_w
    }
    fn draw(
        &self,
        canvas: &mut PixelCanvas,
        rect: Rect,
        _snap: &TwmSnapshot,
        _colors: &Colors,
        cell_w: u32,
        cell_h: u32,
    ) {
        let text = self.current_text();
        let tw = text.chars().count() as u32 * cell_w;
        let tx = rect.x + rect.w.saturating_sub(tw) / 2;
        let ty = rect.y + rect.h.saturating_sub(cell_h) / 2;
        canvas.text_maxw(tx, ty, &text, text_style(self.fg, self.bg), rect.w);
    }
}

// ── LayoutModule ──────────────────────────────────────────────────────────────

pub struct LayoutModule {
    pub fg: Color,
    pub bg: Color,
    pub padding: u32,
}

impl LayoutModule {
    pub fn new(colors: &Colors) -> Self {
        Self {
            fg: colors.bar_accent,
            bg: Color::rgba(0, 0, 0, 0),
            padding: 8,
        }
    }
    pub fn from_def(def: &BarModuleDef, colors: &Colors) -> Self {
        let mut m = Self::new(colors);
        if let Some(v) = def.props.get("fg") {
            if let Some(c) = v.as_color() {
                m.fg = c.into();
            }
        }
        if let Some(v) = def.props.get("padding") {
            if let Some(n) = v.as_px() {
                m.padding = n;
            }
        }
        m
    }
}

impl BarModule for LayoutModule {
    fn min_width(&self, cell_w: u32) -> u32 {
        8 * cell_w + self.padding * 2
    }
    fn draw(
        &self,
        canvas: &mut PixelCanvas,
        rect: Rect,
        snap: &TwmSnapshot,
        _colors: &Colors,
        cell_w: u32,
        cell_h: u32,
    ) {
        let label = snap
            .workspaces
            .get(snap.active_ws)
            .map(|ws| ws.layout)
            .unwrap_or("BSP");
        let tw = label.chars().count() as u32 * cell_w;
        let tx = rect.x + self.padding;
        let ty = rect.y + rect.h.saturating_sub(cell_h) / 2;
        canvas.text_maxw(tx, ty, label, bold_style(self.fg, self.bg), tw + 2);
    }
}

// ── CustomModule ──────────────────────────────────────────────────────────────

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct CustomModule {
    pub static_text: Option<String>,
    pub command: Option<String>,
    pub interval: Duration,
    pub fg: Color,
    pub bg: Color,
    pub padding: u32,
    pub align: Align,
    cache: Arc<Mutex<CachedOutput>>,
}

#[derive(Clone, Copy, Default)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

struct CachedOutput {
    text: String,
    last_poll: Option<Instant>,
}

impl CustomModule {
    pub fn from_def(def: &BarModuleDef, colors: &Colors) -> Self {
        let mut m = Self {
            static_text: None,
            command: None,
            interval: Duration::from_secs(5),
            fg: colors.bar_fg,
            bg: Color::rgba(0, 0, 0, 0),
            padding: 8,
            align: Align::Center,
            cache: Arc::new(Mutex::new(CachedOutput {
                text: String::new(),
                last_poll: None,
            })),
        };
        if let Some(v) = def.props.get("text") {
            if let Some(s) = v.as_str() {
                m.static_text = Some(s.to_string());
            }
        }
        if let Some(v) = def.props.get("command") {
            if let Some(s) = v.as_str() {
                m.command = Some(s.to_string());
            }
        }
        if let Some(v) = def.props.get("interval") {
            if let crate::config::Value::Dimension(n, crate::config::parser::Unit::Ms) = v {
                m.interval = Duration::from_millis(*n as u64);
            }
        }
        if let Some(v) = def.props.get("fg") {
            if let Some(c) = v.as_color() {
                m.fg = c.into();
            }
        }
        if let Some(v) = def.props.get("bg") {
            if let Some(c) = v.as_color() {
                m.bg = c.into();
            }
        }
        if let Some(v) = def.props.get("padding") {
            if let Some(n) = v.as_px() {
                m.padding = n;
            }
        }
        if let Some(v) = def.props.get("align") {
            m.align = match v.as_str() {
                Some("left") => Align::Left,
                Some("right") => Align::Right,
                _ => Align::Center,
            };
        }
        m
    }

    fn current_text(&self) -> String {
        if let Some(t) = &self.static_text {
            return t.clone();
        }
        let Some(cmd) = &self.command else {
            return String::new();
        };

        let mut cache = self.cache.lock().unwrap();
        let now = Instant::now();
        let stale = cache
            .last_poll
            .map(|t| now.duration_since(t) >= self.interval)
            .unwrap_or(true);

        if stale {
            cache.last_poll = Some(now);
            let cmd = cmd.clone();
            let cache2 = Arc::clone(&self.cache);
            std::thread::spawn(move || {
                if let Ok(out) = std::process::Command::new("sh").args(["-c", &cmd]).output() {
                    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    cache2.lock().unwrap().text = text;
                }
            });
        }
        cache.text.clone()
    }
}

impl BarModule for CustomModule {
    fn min_width(&self, cell_w: u32) -> u32 {
        self.current_text().chars().count() as u32 * cell_w + self.padding * 2
    }
    fn draw(
        &self,
        canvas: &mut PixelCanvas,
        rect: Rect,
        _snap: &TwmSnapshot,
        _colors: &Colors,
        cell_w: u32,
        cell_h: u32,
    ) {
        let text = self.current_text();
        if text.is_empty() {
            return;
        }
        let tw = text.chars().count() as u32 * cell_w;
        let tx = match self.align {
            Align::Left => rect.x + self.padding,
            Align::Center => rect.x + rect.w.saturating_sub(tw) / 2,
            Align::Right => rect.x + rect.w.saturating_sub(tw + self.padding),
        };
        let ty = rect.y + rect.h.saturating_sub(cell_h) / 2;
        if self.bg.a > 0 {
            canvas.fill(rect.x, rect.y, rect.w, rect.h, col(self.bg));
        }
        canvas.text_maxw(tx, ty, &text, text_style(self.fg, self.bg), tw + 4);
    }
}

// ── BarModuleSet ──────────────────────────────────────────────────────────────

pub struct BarModuleSet {
    pub left: Vec<Box<dyn BarModule>>,
    pub center: Vec<Box<dyn BarModule>>,
    pub right: Vec<Box<dyn BarModule>>,
}

impl BarModuleSet {
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        let mk = |names: &[String]| -> Vec<Box<dyn BarModule>> {
            names
                .iter()
                .map(|name| {
                    let def = cfg.bar_modules.get(name);
                    build_module(name, def, &cfg.colors)
                })
                .collect()
        };
        Self {
            left: mk(&cfg.bar.modules_left),
            center: mk(&cfg.bar.modules_center),
            right: mk(&cfg.bar.modules_right),
        }
    }

    pub fn draw(
        &self,
        canvas: &mut PixelCanvas,
        bar_rect: Rect,
        snap: &TwmSnapshot,
        colors: &crate::config::Colors,
        bar_cfg: &crate::config::BarConfig,
        cell_w: u32,
        cell_h: u32,
    ) {
        canvas.fill(
            bar_rect.x,
            bar_rect.y,
            bar_rect.w,
            bar_rect.h,
            col(bar_cfg.bg),
        );
        canvas.hline(
            bar_rect.x,
            bar_rect.y,
            bar_rect.w,
            PixColor::rgba(
                colors.inactive_border.r,
                colors.inactive_border.g,
                colors.inactive_border.b,
                colors.inactive_border.a,
            ),
        );

        let total_w = bar_rect.w;
        let right_w: u32 = self.right.iter().map(|m| m.min_width(cell_w)).sum();
        let center_w: u32 = self.center.iter().map(|m| m.min_width(cell_w)).sum();

        // Left
        let mut x = bar_rect.x;
        for m in &self.left {
            let mw = m.min_width(cell_w);
            m.draw(
                canvas,
                Rect::new(x, bar_rect.y, mw, bar_rect.h),
                snap,
                colors,
                cell_w,
                cell_h,
            );
            x += mw;
        }

        // Center
        let mut x = bar_rect.x + total_w.saturating_sub(center_w) / 2;
        for m in &self.center {
            let mw = m.min_width(cell_w);
            m.draw(
                canvas,
                Rect::new(x, bar_rect.y, mw, bar_rect.h),
                snap,
                colors,
                cell_w,
                cell_h,
            );
            x += mw;
        }

        // Right
        let mut x = bar_rect.x + total_w.saturating_sub(right_w);
        for m in &self.right {
            let mw = m.min_width(cell_w);
            m.draw(
                canvas,
                Rect::new(x, bar_rect.y, mw, bar_rect.h),
                snap,
                colors,
                cell_w,
                cell_h,
            );
            x += mw;
        }
    }
}

fn build_module(
    name: &str,
    def: Option<&BarModuleDef>,
    colors: &crate::config::Colors,
) -> Box<dyn BarModule> {
    let kind = def
        .map(|d| d.kind.clone())
        .unwrap_or_else(|| BarModuleKind::from_name(name));
    match kind {
        BarModuleKind::Workspaces => Box::new(
            def.map(|d| WorkspacesModule::from_def(d, colors))
                .unwrap_or_else(|| WorkspacesModule::new(colors)),
        ),
        BarModuleKind::Clock => Box::new(
            def.map(|d| ClockModule::from_def(d, colors))
                .unwrap_or_else(|| ClockModule::new(colors)),
        ),
        BarModuleKind::Layout => Box::new(
            def.map(|d| LayoutModule::from_def(d, colors))
                .unwrap_or_else(|| LayoutModule::new(colors)),
        ),
        BarModuleKind::Systray => Box::new(ClockModule::new(colors)),
        BarModuleKind::Custom => Box::new(
            def.map(|d| CustomModule::from_def(d, colors))
                .unwrap_or_else(|| {
                    CustomModule::from_def(
                        &BarModuleDef {
                            name: name.to_string(),
                            kind: BarModuleKind::Custom,
                            props: Default::default(),
                        },
                        colors,
                    )
                }),
        ),
    }
}
