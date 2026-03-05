// chrome/mod.rs — ChromeApp: trixui App that renders all compositor chrome.

mod custom;

use trixui::{
    app::{App, Cmd, Event, Frame},
    widget::chrome::PaneOpts,
    BarItem, PixColor,
};

use crate::config::{BarModuleKind, Config};
use crate::twm::TwmSnapshot;

// ── ChromeMsg ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum ChromeMsg {
    Snapshot(TwmSnapshot),
    ConfigReloaded(std::sync::Arc<Config>),
}

// ── ChromeApp ─────────────────────────────────────────────────────────────────

pub struct ChromeApp {
    snap: TwmSnapshot,
    config: std::sync::Arc<Config>,
    theme: trixui::Theme,
}

impl ChromeApp {
    pub fn new(config: std::sync::Arc<Config>) -> Self {
        let theme = build_theme(&config);
        Self {
            snap: TwmSnapshot::default(),
            theme,
            config,
        }
    }
}

// ── App impl ──────────────────────────────────────────────────────────────────

impl App for ChromeApp {
    type Message = ChromeMsg;

    fn theme(&self) -> trixui::Theme {
        self.theme.clone()
    }

    fn update(&mut self, event: Event<ChromeMsg>) -> Cmd<ChromeMsg> {
        match event {
            Event::Message(ChromeMsg::Snapshot(snap)) => {
                self.snap = snap;
            }
            Event::Message(ChromeMsg::ConfigReloaded(cfg)) => {
                self.theme = build_theme(&cfg);
                self.config = cfg;
            }
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let snap = &self.snap;
        let cfg = &self.config;
        let bar = &cfg.bar;

        // Clone the theme up front so we hold no borrow on `frame` while
        // calling the mutable draw_pane / bar_area / bar() methods below.
        let t: trixui::Theme = frame.theme().clone();

        // ── Pane borders + titles ─────────────────────────────────────────────
        // draw_pane renders the title notch only. The colored border strips are
        // DRM SolidColorRenderElements built in render.rs::border_elements.
        if let Some(ws) = snap.workspaces.get(snap.active_ws) {
            for pane in &ws.panes {
                if pane.fullscreen {
                    continue;
                }
                let area =
                    trixui::layout::Rect::new(pane.rect.x, pane.rect.y, pane.rect.w, pane.rect.h);
                frame.draw_pane(
                    area,
                    PaneOpts::new(&pane.title)
                        .focused(Some(pane.id) == snap.focused_id)
                        .border_w(cfg.border_width),
                );
            }
        }

        // ── Status bar ────────────────────────────────────────────────────────
        let bar_area = frame.bar_area();
        if bar_area.w == 0 || bar_area.h == 0 {
            return;
        }

        // Build all three zones from config module lists.
        let left_items = build_zone(&bar.modules_left, cfg, snap, &t);
        let center_items = build_zone(&bar.modules_center, cfg, snap, &t);
        let right_items = build_zone(&bar.modules_right, cfg, snap, &t);

        frame
            .bar(bar_area)
            .left(|b| left_items.iter().cloned().fold(b, |b, item| b.item(item)))
            .center(|b| center_items.iter().cloned().fold(b, |b, item| b.item(item)))
            .right(|b| right_items.iter().cloned().fold(b, |b, item| b.item(item)))
            .finish();
    }
}

// ── Theme builder ─────────────────────────────────────────────────────────────

fn build_theme(cfg: &Config) -> trixui::Theme {
    let c = &cfg.colors;
    let b = &cfg.bar;
    let mut t = trixui::Theme::default();

    // Pane chrome
    t.active_border = cfg_color(c.active_border);
    t.inactive_border = cfg_color(c.inactive_border);
    t.active_title = cfg_color(c.active_title);
    t.inactive_title = cfg_color(c.inactive_title);
    t.pane_bg = cfg_color(c.pane_bg);

    // Bar chrome — from bar {} block which has its own color fields
    t.bar_bg = cfg_color(b.bg);
    t.bar_fg = cfg_color(b.fg);
    t.bar_accent = cfg_color(b.accent);
    t.bar_dim = cfg_color(b.dim);

    // Workspace pills
    t.ws_active_fg = cfg_color(b.active_ws_fg);
    t.ws_active_bg = cfg_color(b.active_ws_bg);

    tracing::debug!(
        "build_theme: active_border=#{:02x}{:02x}{:02x} bar_bg=#{:02x}{:02x}{:02x} ws_active_bg=#{:02x}{:02x}{:02x}",
        c.active_border.r, c.active_border.g, c.active_border.b,
        b.bg.r, b.bg.g, b.bg.b,
        b.active_ws_bg.r, b.active_ws_bg.g, b.active_ws_bg.b,
    );

    t
}

// ── Zone builder ──────────────────────────────────────────────────────────────
//
// Converts a list of module names (from modules_left / modules_center /
// modules_right in config) into BarItems. Each name is resolved against
// the bar_modules registry and rendered according to its kind.

fn build_zone(
    names: &[String],
    cfg: &Config,
    snap: &TwmSnapshot,
    t: &trixui::Theme,
) -> Vec<BarItem> {
    let mut items = Vec::new();

    for name in names {
        // Look up the module definition. Built-ins may have no explicit
        // bar_module block in the config — fall back to kind-by-name.
        let kind = cfg
            .bar_modules
            .get(name.as_str())
            .map(|d| d.kind.clone())
            .unwrap_or_else(|| BarModuleKind::from_name(name));

        match kind {
            BarModuleKind::Workspaces => {
                for ws in &snap.workspaces {
                    let idx = (ws.index + 1) as u8;
                    let active = ws.index == snap.active_ws;
                    let occupied = ws.occupied;
                    let label = format!(" {} ", idx);

                    let item = if active {
                        BarItem::pill(label, t.ws_active_fg, t.ws_active_bg, 4).bold(true)
                    } else if occupied {
                        BarItem::text(label).fg(t.active_border).padding(4)
                    } else {
                        BarItem::text(label).fg(t.bar_dim).padding(4)
                    };
                    items.push(item);
                }
            }

            BarModuleKind::Clock => {
                let fmt = cfg
                    .bar_modules
                    .get(name.as_str())
                    .and_then(|d| d.props.get("format"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("%H:%M");
                let text = chrono::Local::now().format(fmt).to_string();
                items.push(
                    BarItem::pill(format!("  {} ", text), t.ws_active_fg, t.ws_active_bg, 0)
                        .bold(true),
                );
            }

            BarModuleKind::Layout => {
                let layout = snap
                    .workspaces
                    .get(snap.active_ws)
                    .map(|w| w.layout)
                    .unwrap_or("BSP");
                let icon = match layout {
                    "BSP" => "󰙀 ",
                    "Columns" => "󰕘 ",
                    "Rows" => "󰕛 ",
                    "ThreeCol" => "󱗼 ",
                    "Monocle" => "󱕻 ",
                    _ => "  ",
                };
                items.push(BarItem::accent(format!("{}{}", icon, layout), t.bar_accent));
            }

            BarModuleKind::Systray => {
                tracing::trace!("bar module 'systray' not yet implemented");
            }

            BarModuleKind::Custom => {
                if let Some(text) = cfg
                    .bar_modules
                    .get(name.as_str())
                    .and_then(|d| d.props.get("text"))
                    .and_then(|v| v.as_str())
                {
                    items.push(BarItem::text(text.to_string()).fg(t.bar_fg));
                    continue;
                }

                // Command-based: wire up CustomModuleCache when ready.
                tracing::trace!("custom bar module '{}': no text or command output", name);
            }
        }
    }

    items
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn cfg_color(c: crate::config::Color) -> PixColor {
    PixColor::rgba(c.r, c.g, c.b, c.a)
}
