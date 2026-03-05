// chrome/mod.rs — TrixieDE: DeApp implementation for compositor chrome.
//
// Handles the bar pipeline: built-in modules (workspaces, clock, layout,
// battery, network, volume) plus fully modular custom shell-command modules
// backed by CustomModuleCache.

mod custom;
pub use custom::CustomModuleCache;

use std::sync::Arc;

use trixui::{
    app::{Cmd, Event},
    layout::Rect,
    pipelines::de::{DeApp, DeFrame, WindowInfo},
    widget::chrome::PaneOpts,
    PixColor, Theme,
};

use crate::config::{BarModuleKind, Config};
use crate::twm::TwmSnapshot;

// ── ChromeMsg ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum ChromeMsg {
    Snapshot(TwmSnapshot),
    ConfigReloaded(Arc<Config>),
}

// ── TrixieDE ──────────────────────────────────────────────────────────────────

pub struct TrixieDE {
    pub snap: TwmSnapshot,
    pub config: Arc<Config>,
    pub theme: Theme,
    /// Live output from shell-command bar modules.
    pub custom: Arc<CustomModuleCache>,
}

impl TrixieDE {
    pub fn new(config: Arc<Config>) -> Self {
        let theme = build_theme(&config);
        let custom = Arc::new(CustomModuleCache::from_config(&config));
        custom.start_all();
        Self {
            snap: TwmSnapshot::default(),
            theme,
            custom,
            config,
        }
    }

    /// Rebuild the custom module cache from a new config (on hot-reload).
    pub fn reload_custom(&mut self) {
        self.custom.stop_all();
        let cache = CustomModuleCache::from_config(&self.config);
        cache.start_all();
        self.custom = Arc::new(cache);
    }
}

// ── DeApp impl ────────────────────────────────────────────────────────────────

impl DeApp for TrixieDE {
    type Message = ChromeMsg;

    fn theme(&self) -> Theme {
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
                self.reload_custom();
            }
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut DeFrame) {
        let snap = &self.snap;
        let cfg = &self.config;
        let bar = &cfg.bar;

        let t: Theme = frame.theme().clone();

        // ── Status bar ────────────────────────────────────────────────────────
        let bar_area = frame.bar_area();
        if bar_area.w == 0 || bar_area.h == 0 {
            // No bar — still draw pane decorations.
            render_panes(frame, cfg);
            return;
        }

        let left_items = build_zone(&bar.modules_left, cfg, snap, &t, &self.custom);
        let center_items = build_zone(&bar.modules_center, cfg, snap, &t, &self.custom);
        let right_items = build_zone(&bar.modules_right, cfg, snap, &t, &self.custom);

        frame
            .bar()
            .left(|b| left_items.iter().cloned().fold(b, |b, item| b.item(item)))
            .center(|b| center_items.iter().cloned().fold(b, |b, item| b.item(item)))
            .right(|b| right_items.iter().cloned().fold(b, |b, item| b.item(item)))
            .finish();

        render_panes(frame, cfg);
    }
}

fn render_panes(frame: &mut DeFrame, cfg: &Config) {
    let win_data: Vec<(Rect, String, bool)> = frame
        .windows()
        .iter()
        .map(|w| (w.rect, w.title.clone(), w.focused))
        .collect();
    for (rect, title, focused) in win_data {
        frame.pane(
            rect,
            PaneOpts::new(&title)
                .focused(focused)
                .border_w(cfg.border_width),
        );
    }
}

// ── Theme builder ─────────────────────────────────────────────────────────────

pub fn build_theme(cfg: &Config) -> Theme {
    let c = &cfg.colors;
    let b = &cfg.bar;
    let mut t = Theme::default();

    t.active_border = cfg_color(c.active_border);
    t.inactive_border = cfg_color(c.inactive_border);
    t.active_title = cfg_color(c.active_title);
    t.inactive_title = cfg_color(c.inactive_title);
    t.pane_bg = cfg_color(c.pane_bg);

    t.bar_bg = cfg_color(b.bg);
    t.bar_fg = cfg_color(b.fg);
    t.bar_accent = cfg_color(b.accent);
    t.bar_dim = cfg_color(b.dim);

    t.ws_active_fg = cfg_color(b.active_ws_fg);
    t.ws_active_bg = cfg_color(b.active_ws_bg);

    t
}

// ── Zone builder ──────────────────────────────────────────────────────────────

fn build_zone(
    names: &[String],
    cfg: &Config,
    snap: &TwmSnapshot,
    t: &Theme,
    custom: &CustomModuleCache,
) -> Vec<trixui::BarItem> {
    let mut items = Vec::new();

    for name in names {
        let kind = cfg
            .bar_modules
            .get(name.as_str())
            .map(|d| d.kind.clone())
            .unwrap_or_else(|| BarModuleKind::from_name(name));

        match kind {
            // ── Built-ins ─────────────────────────────────────────────────────
            BarModuleKind::Workspaces => {
                for ws in &snap.workspaces {
                    let active = ws.index == snap.active_ws;
                    let label = format!(" {} ", ws.index + 1);
                    let item = if active {
                        trixui::BarItem::pill(label, t.ws_active_fg, t.ws_active_bg, 4).bold(true)
                    } else if ws.occupied {
                        trixui::BarItem::text(label).fg(t.active_border).padding(4)
                    } else {
                        trixui::BarItem::text(label).fg(t.bar_dim).padding(4)
                    };
                    items.push(item);
                }
            }

            BarModuleKind::Clock => {
                let fmt = prop_str(cfg, name, "format").unwrap_or("%H:%M");
                let text = chrono::Local::now().format(fmt).to_string();
                items.push(
                    trixui::BarItem::pill(format!("  {text} "), t.ws_active_fg, t.ws_active_bg, 0)
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
                items.push(trixui::BarItem::accent(
                    format!("{icon}{layout}"),
                    t.bar_accent,
                ));
            }

            BarModuleKind::Battery => {
                if let Some(info) = read_battery() {
                    let icon = battery_icon(info.pct, info.charging);
                    let color = if info.pct <= 20 {
                        PixColor::rgba(0xf3, 0x8b, 0xa8, 0xff) // warn red
                    } else if info.charging {
                        PixColor::rgba(0xa6, 0xe3, 0xa1, 0xff) // charging green
                    } else {
                        t.bar_fg
                    };
                    let label = if info.charging {
                        format!("{icon} {}% ⚡", info.pct)
                    } else {
                        format!("{icon} {}%", info.pct)
                    };
                    items.push(trixui::BarItem::text(label).fg(color));
                }
            }

            BarModuleKind::Network => {
                if let Some(info) = read_network(cfg) {
                    let icon = if info.connected { "󰖩 " } else { "󰖪 " };
                    let color = if info.connected { t.bar_fg } else { t.bar_dim };
                    // Show upload/download rates if available.
                    let label = if let Some(rates) = &info.rates {
                        format!("{icon}{} ↑{} ↓{}", info.label, rates.tx, rates.rx)
                    } else {
                        format!("{icon}{}", info.label)
                    };
                    items.push(trixui::BarItem::text(label).fg(color));
                }
            }

            BarModuleKind::Volume => {
                if let Some(info) = read_volume() {
                    let icon = if info.muted {
                        "󰝟"
                    } else if info.pct >= 70 {
                        "󰕾"
                    } else if info.pct >= 30 {
                        "󰖀"
                    } else {
                        "󰕿"
                    };
                    let color = if info.muted { t.bar_dim } else { t.bar_fg };
                    let label = if info.muted {
                        format!("{icon} mute")
                    } else {
                        format!("{icon} {}%", info.pct)
                    };
                    items.push(trixui::BarItem::text(label).fg(color));
                }
            }

            BarModuleKind::Systray => {
                tracing::trace!("bar module 'systray' not yet implemented");
            }

            // ── Custom shell command ───────────────────────────────────────────
            BarModuleKind::Custom => {
                // 1. Static text prop.
                if let Some(text) = prop_str(cfg, name, "text") {
                    items.push(styled_custom_item(text, cfg, name, t));
                    continue;
                }

                // 2. Polled command output from CustomModuleCache.
                if let Some(text) = custom.get(name) {
                    items.push(styled_custom_item(&text, cfg, name, t));
                    continue;
                }

                // 3. Legacy `cached_output` prop (for backward compat).
                if let Some(text) = prop_str(cfg, name, "cached_output") {
                    items.push(styled_custom_item(text, cfg, name, t));
                    continue;
                }

                tracing::trace!("custom bar module '{name}': no output yet");
            }
        }
    }

    items
}

fn styled_custom_item(text: &str, cfg: &Config, name: &str, t: &Theme) -> trixui::BarItem {
    // Optional per-module styling.
    let color: PixColor = cfg
        .bar_modules
        .get(name)
        .and_then(|d| d.props.get("color"))
        .and_then(|v| v.as_color())
        .map(|c| PixColor::rgba(c[0], c[1], c[2], c[3]))
        .unwrap_or(t.bar_fg);

    let icon: Option<&str> = cfg
        .bar_modules
        .get(name)
        .and_then(|d| d.props.get("icon"))
        .and_then(|v| v.as_str());

    let label = if let Some(ic) = icon {
        format!("{ic} {text}")
    } else {
        text.to_string()
    };

    trixui::BarItem::text(label).fg(color)
}

fn prop_str<'a>(cfg: &'a Config, name: &str, key: &str) -> Option<&'a str> {
    cfg.bar_modules
        .get(name)
        .and_then(|d| d.props.get(key))
        .and_then(|v| v.as_str())
}

// ── Battery reader ────────────────────────────────────────────────────────────

struct BatteryInfo {
    pct: u32,
    charging: bool,
}

fn read_battery() -> Option<BatteryInfo> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in dir.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("BAT") {
            continue;
        }
        let base = entry.path();

        let pct = std::fs::read_to_string(base.join("capacity"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())?;

        let status = std::fs::read_to_string(base.join("status")).unwrap_or_default();
        let charging = matches!(status.trim(), "Charging" | "Full");

        return Some(BatteryInfo { pct, charging });
    }
    None
}

fn battery_icon(pct: u32, charging: bool) -> &'static str {
    if charging {
        return "󰂄";
    }
    match pct {
        91..=100 => "󰁹",
        81..=90 => "󰂂",
        61..=80 => "󰂀",
        41..=60 => "󰁾",
        21..=40 => "󰁼",
        11..=20 => "󰁺",
        _ => "󰂃",
    }
}

// ── Network reader ────────────────────────────────────────────────────────────

struct NetworkRates {
    tx: String,
    rx: String,
}

struct NetworkInfo {
    connected: bool,
    label: String,
    rates: Option<NetworkRates>,
}

fn read_network(cfg: &Config) -> Option<NetworkInfo> {
    let iface = cfg
        .bar_modules
        .get("network")
        .and_then(|d| d.props.get("interface"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(detect_active_iface);

    let operstate =
        std::fs::read_to_string(format!("/sys/class/net/{iface}/operstate")).unwrap_or_default();
    let connected = operstate.trim() == "up";

    let label = if iface.starts_with('w') {
        read_ssid(&iface).unwrap_or_else(|| iface.clone())
    } else {
        iface.clone()
    };

    // Read TX/RX bytes for rate estimation (snapshot — rates need two readings
    // for a meaningful delta, so we show the raw KB here as a lightweight proxy).
    let rates = read_net_bytes(&iface).map(|(tx, rx)| NetworkRates {
        tx: format_bytes(tx),
        rx: format_bytes(rx),
    });

    Some(NetworkInfo {
        connected,
        label,
        rates,
    })
}

fn read_net_bytes(iface: &str) -> Option<(u64, u64)> {
    let tx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/tx_bytes"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let rx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/rx_bytes"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some((tx, rx))
}

fn format_bytes(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1}GB", b as f64 / 1e9)
    } else if b >= 1_000_000 {
        format!("{:.1}MB", b as f64 / 1e6)
    } else if b >= 1_000 {
        format!("{:.0}KB", b as f64 / 1e3)
    } else {
        format!("{b}B")
    }
}

fn detect_active_iface() -> String {
    let routes = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    for line in routes.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        if let Ok(flags) = u32::from_str_radix(cols[3], 16) {
            if flags & 0x2 != 0 {
                return cols[0].to_string();
            }
        }
    }
    "eth0".to_string()
}

fn read_ssid(iface: &str) -> Option<String> {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/wireless/ssid"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── Volume reader ─────────────────────────────────────────────────────────────

struct VolumeInfo {
    pct: u32,
    muted: bool,
}

fn read_volume() -> Option<VolumeInfo> {
    // Try wireplumber/pipewire first.
    if let Some(info) = read_volume_wpctl() {
        return Some(info);
    }
    // Fall back to amixer (ALSA).
    read_volume_amixer()
}

fn read_volume_wpctl() -> Option<VolumeInfo> {
    let out = std::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    let vol_part = text.strip_prefix("Volume: ")?.split_whitespace().next()?;
    let vol: f32 = vol_part.parse().ok()?;
    let pct = (vol * 100.0).round() as u32;
    let muted = text.contains("[MUTED]");
    Some(VolumeInfo { pct, muted })
}

fn read_volume_amixer() -> Option<VolumeInfo> {
    let out = std::process::Command::new("amixer")
        .args(["sget", "Master"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // Look for: [75%] [on] or [75%] [off]
    let line = text.lines().find(|l| l.contains('%'))?;
    let pct_str = line.split('[').nth(1)?.trim_end_matches(['%', ']']);
    let pct: u32 = pct_str.trim().parse().ok()?;
    let muted = line.contains("[off]");
    Some(VolumeInfo { pct, muted })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn cfg_color(c: crate::config::Color) -> PixColor {
    PixColor::rgba(c.r, c.g, c.b, c.a)
}
