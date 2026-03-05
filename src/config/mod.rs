// config/mod.rs — typed config, hot-reload, bar module registry

mod lsp;
pub mod parser;
pub use parser::{parse, ConfigFile, Value};

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

// ── Paths ─────────────────────────────────────────────────────────────────────

pub fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            PathBuf::from(home).join(".config")
        });
    base.join("trixie")
}

pub fn config_path() -> PathBuf {
    config_dir().join("trixie.conf")
}

// ── Colors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub fn hex(v: u32) -> Self {
        Self::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub fn to_f32(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }
    pub fn to_trixui(self) -> trixui::PixColor {
        trixui::PixColor::rgba(self.r, self.g, self.b, self.a)
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::rgb(0, 0, 0)
    }
}

impl From<[u8; 4]> for Color {
    fn from([r, g, b, a]: [u8; 4]) -> Self {
        Self { r, g, b, a }
    }
}

fn color_from_value(v: &Value) -> Option<Color> {
    v.as_color().map(Color::from)
}

// ── Keybinds ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub mods: Modifiers,
    pub key: String,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Modifiers: u8 {
        const SUPER = 0b0001;
        const CTRL  = 0b0010;
        const ALT   = 0b0100;
        const SHIFT = 0b1000;
    }
}

impl KeyCombo {
    /// Parses both formats:
    ///   - `SUPER+SHIFT:Return`  (trixie.conf colon-separated key)
    ///   - `Super+Shift+Return`  (legacy plus-only format, last token is key)
    pub fn parse(s: &str) -> Option<Self> {
        // Colon format: everything before the last ':' is mods, after is key.
        // Plus-only format (no ':'): everything before the last '+' is mods.
        let (mods_part, key_part) = if let Some(colon) = s.rfind(':') {
            (&s[..colon], &s[colon + 1..])
        } else if let Some(plus) = s.rfind('+') {
            (&s[..plus], &s[plus + 1..])
        } else {
            // No separator — bare key with no mods.
            return Some(Self {
                mods: Modifiers::empty(),
                key: s.to_lowercase(),
            });
        };

        if key_part.is_empty() {
            return None;
        }

        let mut mods = Modifiers::empty();
        for m in mods_part.split('+') {
            match m.to_lowercase().as_str() {
                "super" | "mod4" | "logo" => mods |= Modifiers::SUPER,
                "ctrl" | "control" => mods |= Modifiers::CTRL,
                "alt" | "mod1" => mods |= Modifiers::ALT,
                "shift" => mods |= Modifiers::SHIFT,
                "" => {} // leading/trailing '+' — ignore
                other => tracing::warn!("unknown modifier '{}' in keybind '{}'", other, s),
            }
        }

        Some(Self {
            mods,
            key: key_part.to_lowercase(),
        })
    }
}

#[derive(Debug, Clone)]
pub enum KeyAction {
    Exec(String, Vec<String>),
    Close,
    Fullscreen,
    ToggleBar,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    Workspace(u8),
    MoveToWorkspace(u8),
    NextLayout,
    PrevLayout,
    GrowMain,
    ShrinkMain,
    NextWorkspace,
    PrevWorkspace,
    Quit,
    Reload,
    SwitchVt(i32),
    EmergencyQuit,
    Custom(String, Vec<String>),
}

impl KeyAction {
    fn parse(action: &str, args: &[&str]) -> Option<Self> {
        match action {
            "exec" => Some(Self::Exec(
                args.first()?.to_string(),
                args.get(1..)
                    .unwrap_or(&[])
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            )),
            "close" => Some(Self::Close),
            "fullscreen" => Some(Self::Fullscreen),
            "toggle_bar" => Some(Self::ToggleBar),
            "focus" => match args.first().copied() {
                Some("left") => Some(Self::FocusLeft),
                Some("right") => Some(Self::FocusRight),
                Some("up") => Some(Self::FocusUp),
                Some("down") => Some(Self::FocusDown),
                _ => None,
            },
            "move" => match args.first().copied() {
                Some("left") => Some(Self::MoveLeft),
                Some("right") => Some(Self::MoveRight),
                Some("up") => Some(Self::MoveUp),
                Some("down") => Some(Self::MoveDown),
                _ => None,
            },
            "workspace" => args.first()?.parse().ok().map(Self::Workspace),
            "move_to_workspace" => args.first()?.parse().ok().map(Self::MoveToWorkspace),
            "next_layout" => Some(Self::NextLayout),
            "prev_layout" => Some(Self::PrevLayout),
            "grow_main" => Some(Self::GrowMain),
            "shrink_main" => Some(Self::ShrinkMain),
            "next_workspace" => Some(Self::NextWorkspace),
            "prev_workspace" => Some(Self::PrevWorkspace),
            "quit" => Some(Self::Quit),
            "reload" => Some(Self::Reload),
            "switch_vt" => args.first()?.parse().ok().map(Self::SwitchVt),
            other => Some(Self::Custom(
                other.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            )),
        }
    }
}

// ── Window rules ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WindowRule {
    pub matcher: RuleMatcher,
    pub effects: Vec<RuleEffect>,
}

#[derive(Debug, Clone)]
pub enum RuleMatcher {
    Class(String),
    Title(String),
    AppId(String),
}

impl RuleMatcher {
    fn parse(s: &str) -> Option<Self> {
        if let Some(rest) = s.strip_prefix("class:") {
            return Some(Self::Class(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("title:") {
            return Some(Self::Title(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("app_id:") {
            return Some(Self::AppId(rest.to_string()));
        }
        Some(Self::AppId(s.to_string()))
    }

    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        match self {
            Self::Class(c) | Self::AppId(c) => app_id.contains(c.as_str()),
            Self::Title(t) => title.contains(t.as_str()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RuleEffect {
    Float,
    Fullscreen,
    Size(u32, u32),
    Position(i32, i32),
    Workspace(u8),
    NoBorder,
    NoTitle,
    Opacity(f32),
}

impl RuleEffect {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "float" => return Some(Self::Float),
            "fullscreen" => return Some(Self::Fullscreen),
            "noborder" => return Some(Self::NoBorder),
            "notitle" => return Some(Self::NoTitle),
            _ => {}
        }
        if let Some(rest) = s.strip_prefix("size ") {
            let parts: Vec<&str> = rest.split('x').collect();
            if parts.len() == 2 {
                let w = parts[0].parse().ok()?;
                let h = parts[1].parse().ok()?;
                return Some(Self::Size(w, h));
            }
        }
        if let Some(rest) = s.strip_prefix("workspace ") {
            return rest.parse().ok().map(Self::Workspace);
        }
        if let Some(rest) = s.strip_prefix("opacity ") {
            return rest.parse().ok().map(Self::Opacity);
        }
        None
    }
}

// ── Bar module definition ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BarModuleDef {
    pub name: String,
    pub kind: BarModuleKind,
    pub props: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarModuleKind {
    Workspaces,
    Clock,
    Layout,
    Systray,
    Custom,
}

impl BarModuleKind {
    pub fn from_name(s: &str) -> Self {
        match s {
            "workspaces" => Self::Workspaces,
            "clock" => Self::Clock,
            "layout" => Self::Layout,
            "systray" => Self::Systray,
            _ => Self::Custom,
        }
    }
}

// ── Bar config ────────────────────────────────────────────────────────────────

/// All fields that bar.conf can set inside the `bar { }` block.
#[derive(Debug, Clone)]
pub struct BarConfig {
    pub position: BarPosition,
    pub height: u32,
    pub padding: u32,
    pub item_spacing: u32,
    pub font_size: Option<f32>, // overrides global font_size for bar
    pub glyph_y_offset: i32,
    pub modules_left: Vec<String>,
    pub modules_center: Vec<String>,
    pub modules_right: Vec<String>,
    // Colors
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub dim: Color,
    // Separator
    pub separator: bool,
    pub separator_top: bool,
    pub separator_color: Color,
    // Workspace pills
    pub active_ws_fg: Color,
    pub active_ws_bg: Color,
    pub occupied_ws_fg: Color,
    pub inactive_ws_fg: Color,
    // Pill geometry
    pub pill_radius: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPosition {
    Top,
    Bottom,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            position: BarPosition::Bottom,
            height: 28,
            padding: 10,
            item_spacing: 4,
            font_size: None,
            glyph_y_offset: 0,
            modules_left: vec!["workspaces".into()],
            modules_center: vec!["clock".into()],
            modules_right: vec!["layout".into()],
            bg: Color::hex(0x181825),
            fg: Color::hex(0xa6adc8),
            accent: Color::hex(0xb4befe),
            dim: Color::hex(0x585b70),
            separator: false,
            separator_top: false,
            separator_color: Color::hex(0x313244),
            active_ws_fg: Color::hex(0x11111b),
            active_ws_bg: Color::hex(0xb4befe),
            occupied_ws_fg: Color::hex(0xb4befe),
            inactive_ws_fg: Color::hex(0x585b70),
            pill_radius: 4,
        }
    }
}

// ── Keyboard config ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct KeyboardConfig {
    pub layout: Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
    pub repeat_rate: u32,
    pub repeat_delay: u32,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            layout: None,
            variant: None,
            options: None,
            repeat_rate: 25,
            repeat_delay: 600,
        }
    }
}

// ── Monitor config ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    pub position: (i32, i32),
    pub scale: f32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            name: "primary".into(),
            width: 1920,
            height: 1080,
            refresh: 60,
            position: (0, 0),
            scale: 1.0,
        }
    }
}

// ── Exec entries ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExecEntry {
    pub command: String,
    pub args: Vec<String>,
}

impl ExecEntry {
    /// Parse a command string. Handles simple `"cmd arg1 arg2"` splitting.
    /// Shell pipes (`|`) and redirects are passed verbatim as args — the
    /// compositor hands the full argv to exec, not a shell.
    fn parse(s: &str) -> Self {
        let mut parts = s.split_whitespace();
        let command = parts.next().unwrap_or("").to_string();
        let args = parts.map(|s| s.to_string()).collect();
        Self { command, args }
    }
}

// ── Colors block ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Colors {
    pub active_border: Color,
    pub inactive_border: Color,
    pub active_title: Color,
    pub inactive_title: Color,
    pub pane_bg: Color,
    pub bar_bg: Color,
    pub bar_fg: Color,
    pub bar_accent: Color,
    pub focus_ring: Color,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            active_border: Color::hex(0xb4befe),
            inactive_border: Color::hex(0x45475a),
            active_title: Color::hex(0xb4befe),
            inactive_title: Color::hex(0x585b70),
            pane_bg: Color::hex(0x11111b),
            bar_bg: Color::hex(0x181825),
            bar_fg: Color::hex(0xa6adc8),
            bar_accent: Color::hex(0xb4befe),
            focus_ring: Color::hex(0xb4befe),
        }
    }
}

// ── Main Config ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub font_path: PathBuf,
    pub font_size: f32,
    pub gap: u32,
    pub border_width: u32,
    pub colors: Colors,
    pub bar: BarConfig,
    pub bar_modules: HashMap<String, BarModuleDef>,
    pub keybinds: Vec<(KeyCombo, KeyAction)>,
    pub window_rules: Vec<WindowRule>,
    pub keyboard: KeyboardConfig,
    pub monitors: Vec<MonitorConfig>,
    pub exec_once: Vec<ExecEntry>,
    pub exec: Vec<ExecEntry>,
    pub seat_name: String,
    pub workspaces: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_path: PathBuf::from("/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf"),
            font_size: 14.0,
            gap: 4,
            border_width: 1,
            colors: Colors::default(),
            bar: BarConfig::default(),
            bar_modules: HashMap::new(),
            keybinds: default_keybinds(),
            window_rules: Vec::new(),
            keyboard: KeyboardConfig::default(),
            monitors: Vec::new(),
            exec_once: Vec::new(),
            exec: Vec::new(),
            seat_name: "seat0".into(),
            workspaces: 9,
        }
    }
}

fn default_keybinds() -> Vec<(KeyCombo, KeyAction)> {
    vec![
        (
            KeyCombo::parse("Super:Return").unwrap(),
            KeyAction::Exec("foot".into(), vec![]),
        ),
        (KeyCombo::parse("Super:q").unwrap(), KeyAction::Close),
        (KeyCombo::parse("Super:f").unwrap(), KeyAction::Fullscreen),
        (
            KeyCombo::parse("Super+Shift:b").unwrap(),
            KeyAction::ToggleBar,
        ),
        (KeyCombo::parse("Super:h").unwrap(), KeyAction::FocusLeft),
        (KeyCombo::parse("Super:l").unwrap(), KeyAction::FocusRight),
        (KeyCombo::parse("Super:k").unwrap(), KeyAction::FocusUp),
        (KeyCombo::parse("Super:j").unwrap(), KeyAction::FocusDown),
        (
            KeyCombo::parse("Super+Shift:h").unwrap(),
            KeyAction::MoveLeft,
        ),
        (
            KeyCombo::parse("Super+Shift:l").unwrap(),
            KeyAction::MoveRight,
        ),
        (KeyCombo::parse("Super:Tab").unwrap(), KeyAction::NextLayout),
        (KeyCombo::parse("Super:equal").unwrap(), KeyAction::GrowMain),
        (
            KeyCombo::parse("Super:minus").unwrap(),
            KeyAction::ShrinkMain,
        ),
        (
            KeyCombo::parse("Super+Ctrl:Right").unwrap(),
            KeyAction::NextWorkspace,
        ),
        (
            KeyCombo::parse("Super+Ctrl:Left").unwrap(),
            KeyAction::PrevWorkspace,
        ),
    ]
}

// ── Known key sets (for unknown-key warnings) ─────────────────────────────────

const KNOWN_TOP_LEVEL: &[&str] = &[
    "font",
    "font_size",
    "gap",
    "border_width",
    "workspaces",
    "seat_name",
    "keybind",
    "window_rule",
    "exec_once",
    "exec",
    "source",
];

const KNOWN_COLORS: &[&str] = &[
    "active_border",
    "inactive_border",
    "active_title",
    "inactive_title",
    "pane_bg",
    "bar_bg",
    "bar_fg",
    "bar_accent",
    "focus_ring",
];

const KNOWN_KEYBOARD: &[&str] = &[
    "layout",
    "variant",
    "options",
    "repeat_rate",
    "repeat_delay",
];

const KNOWN_BAR: &[&str] = &[
    "position",
    "height",
    "padding",
    "item_spacing",
    "font_size",
    "glyph_y_offset",
    "modules_left",
    "modules_center",
    "modules_right",
    "bg",
    "fg",
    "accent",
    "dim",
    "separator",
    "separator_top",
    "separator_color",
    "active_ws_fg",
    "active_ws_bg",
    "occupied_ws_fg",
    "inactive_ws_fg",
    "pill_radius",
];

const KNOWN_MONITOR: &[&str] = &["width", "height", "refresh", "position", "scale"];

// Silently accepted blocks — no unknown-key warnings for their contents.
const SILENT_BLOCKS: &[&str] = &["animations", "general"];

fn warn_unknown_keys(block_name: &str, b: &parser::Block, known: &[&str]) {
    for k in b.assignment_keys() {
        if !known.contains(&k.value.as_str()) {
            tracing::warn!(
                "config: unknown key '{}' in '{}' block (line {})",
                k.value,
                block_name,
                k.span.line
            );
        }
    }
}

fn resolve_font_path(s: &str) -> PathBuf {
    // 1. Already an absolute path?
    let p = PathBuf::from(s);
    if p.is_absolute() {
        if p.exists() {
            return p;
        }
        tracing::warn!("config: font path {:?} does not exist", p);
        return p; // return as-is and let trixui fail with a clear error
    }

    // 2. Home-relative path?
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let p = PathBuf::from(home).join(rest);
        if p.exists() {
            return p;
        }
        tracing::warn!("config: font path {:?} does not exist", p);
        return p;
    }

    // 3. Search font directories for a matching filename (or name + extension).
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let xdg_data =
        std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", home));

    let search_dirs: &[&str] = &[
        // User font dirs
        &format!("{}/fonts", xdg_data),
        &format!("{}/.fonts", home),
        &format!("{}/.local/share/fonts", home),
        // System font dirs
        "/usr/share/fonts",
        "/usr/share/fonts/Iosevka",
        "/usr/local/share/fonts",
        "/usr/share/fonts/TTF",
        "/usr/share/fonts/OTF",
        "/usr/share/fonts/truetype",
        "/usr/share/fonts/opentype",
    ];

    // Candidate filenames to try — with and without common extensions.
    let mut candidates: Vec<String> = vec![s.to_string()];
    if !s.ends_with(".ttf") && !s.ends_with(".otf") && !s.ends_with(".TTF") && !s.ends_with(".OTF")
    {
        candidates.push(format!("{}.ttf", s));
        candidates.push(format!("{}.otf", s));
        candidates.push(format!("{}.TTF", s));
        candidates.push(format!("{}.OTF", s));
    }

    for dir in search_dirs {
        let base = Path::new(dir);
        if !base.exists() {
            continue;
        }
        // Try direct match first (flat dir).
        for cand in &candidates {
            let p = base.join(cand);
            if p.exists() {
                tracing::info!("config: resolved font {:?} → {:?}", s, p);
                return p;
            }
        }
        // Walk one level of subdirectories (e.g. /usr/share/fonts/nerd-fonts/…).
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    for cand in &candidates {
                        let p = entry.path().join(cand);
                        if p.exists() {
                            tracing::info!("config: resolved font {:?} → {:?}", s, p);
                            return p;
                        }
                    }
                }
            }
        }
    }

    tracing::warn!(
        "config: could not find font {:?} in any font directory — using default",
        s
    );
    PathBuf::from("/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf")
}

// ── Config impl ───────────────────────────────────────────────────────────────

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        tracing::info!("Config::load reading {:?}", path);
        if path.exists() {
            Self::from_path(&path)
        } else {
            tracing::warn!("Config file not found at {:?} — using defaults", path);
            Self::default()
        }
    }

    pub fn from_path(path: &Path) -> Self {
        let result = parser::parse_file(path);
        for e in &result.errors {
            tracing::warn!("Config parse error in {:?}: {}", path, e);
        }
        tracing::info!(
            "Config loaded: {} top-level items, {} errors",
            result.file.items.len(),
            result.errors.len()
        );
        // Log every top-level item at debug level so we can verify sourced
        // files are being inlined correctly.
        for item in &result.file.items {
            match item {
                parser::Item::Assignment(a) => {
                    tracing::debug!("config item: {} = ...", a.key.value);
                }
                parser::Item::Block(b) => {
                    tracing::debug!(
                        "config block: {} (label={:?})",
                        b.name.value,
                        b.label.as_ref().map(|l| &l.value)
                    );
                }
            }
        }
        Self::from_file(result.file)
    }

    // Keep from_source for callers that already have the src string (LSP etc.)
    pub fn from_source(src: &str, path: &Path) -> Self {
        let result = parse(src);
        for e in &result.errors {
            tracing::warn!("Config parse error in {:?}: {}", path, e);
        }
        Self::from_file(result.file)
    }

    pub fn from_file(f: ConfigFile) -> Self {
        tracing::info!(
            "from_file: {} items, keys: {:?}",
            f.items.len(),
            f.items
                .iter()
                .filter_map(|i| {
                    if let parser::Item::Assignment(a) = i {
                        Some(a.key.value.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        );
        let mut cfg = Self::default();

        // ── Warn on unknown top-level keys ────────────────────────────────────
        for item in &f.items {
            if let parser::Item::Assignment(a) = item {
                if !KNOWN_TOP_LEVEL.contains(&a.key.value.as_str()) {
                    tracing::warn!(
                        "config: unknown top-level key '{}' (line {})",
                        a.key.value,
                        a.key.span.line
                    );
                }
            }
        }

        // ── Core scalar fields ────────────────────────────────────────────────
        // last-wins: sourced files come before main file in item order,
        // so trixie.conf assignments naturally win.
        if let Some(v) = f.get_last("font") {
            tracing::info!(
                "config: font key present={} value={:?}",
                f.get_last("font").is_some(),
                f.get_last("font").map(|v| &v.value),
            );
            if let Some(s) = v.value.as_str() {
                cfg.font_path = resolve_font_path(s);
            }
        }
        if let Some(v) = f.get_last("font_size") {
            if let Some(n) = v.value.as_f64() {
                cfg.font_size = n as f32;
            }
        }
        if let Some(v) = f.get_last("gap") {
            if let Some(px) = v.value.as_px() {
                cfg.gap = px;
            }
        }
        if let Some(v) = f.get_last("border_width") {
            if let Some(px) = v.value.as_px() {
                cfg.border_width = px;
            }
        }
        // Both `seat_name` and bare `seat` are accepted.
        if let Some(v) = f.get_last("seat_name").or_else(|| f.get_last("seat")) {
            if let Some(s) = v.value.as_str() {
                cfg.seat_name = s.to_string();
            }
        }
        if let Some(v) = f.get_last("workspaces") {
            if let Some(n) = v.value.as_i64() {
                cfg.workspaces = n.clamp(1, 32) as u8;
            }
        }

        // ── colors { } ────────────────────────────────────────────────────────
        if let Some(b) = f.block_last("colors") {
            warn_unknown_keys("colors", b, KNOWN_COLORS);
            macro_rules! col {
                ($field:ident, $key:literal) => {
                    if let Some(v) = b.get($key) {
                        if let Some(c) = color_from_value(&v.value) {
                            cfg.colors.$field = c;
                        }
                    }
                };
            }
            col!(active_border, "active_border");
            col!(inactive_border, "inactive_border");
            col!(active_title, "active_title");
            col!(inactive_title, "inactive_title");
            col!(pane_bg, "pane_bg");
            col!(bar_bg, "bar_bg");
            col!(bar_fg, "bar_fg");
            col!(bar_accent, "bar_accent");
            col!(focus_ring, "focus_ring");
        }

        // ── keyboard { } ──────────────────────────────────────────────────────
        if let Some(b) = f.block_last("keyboard") {
            warn_unknown_keys("keyboard", b, KNOWN_KEYBOARD);
            if let Some(v) = b.get("layout") {
                // Empty string means "use system default" — store as None.
                cfg.keyboard.layout = v.value.as_str().filter(|s| !s.is_empty()).map(String::from);
            }
            if let Some(v) = b.get("variant") {
                cfg.keyboard.variant = v.value.as_str().filter(|s| !s.is_empty()).map(String::from);
            }
            if let Some(v) = b.get("options") {
                cfg.keyboard.options = v.value.as_str().filter(|s| !s.is_empty()).map(String::from);
            }
            if let Some(v) = b.get("repeat_rate") {
                if let Some(n) = v.value.as_i64() {
                    cfg.keyboard.repeat_rate = n as u32;
                }
            }
            if let Some(v) = b.get("repeat_delay") {
                if let Some(n) = v.value.as_i64() {
                    cfg.keyboard.repeat_delay = n as u32;
                }
            }
        }

        // ── bar { } ───────────────────────────────────────────────────────────
        if let Some(b) = f.block_last("bar") {
            warn_unknown_keys("bar", b, KNOWN_BAR);

            if let Some(v) = b.get("position") {
                cfg.bar.position = match v.value.as_str() {
                    Some("top") => BarPosition::Top,
                    _ => BarPosition::Bottom,
                };
            }
            if let Some(v) = b.get("height") {
                if let Some(px) = v.value.as_px() {
                    cfg.bar.height = px;
                }
            }
            if let Some(v) = b.get("padding") {
                if let Some(px) = v.value.as_px() {
                    cfg.bar.padding = px;
                }
            }
            if let Some(v) = b.get("item_spacing") {
                if let Some(px) = v.value.as_px() {
                    cfg.bar.item_spacing = px;
                }
            }
            if let Some(v) = b.get("font_size") {
                cfg.bar.font_size = v
                    .value
                    .as_f32()
                    .or_else(|| v.value.as_px().map(|p| p as f32));
            }
            if let Some(v) = b.get("glyph_y_offset") {
                if let Some(n) = v.value.as_i64() {
                    cfg.bar.glyph_y_offset = n as i32;
                }
            }

            macro_rules! modules {
                ($field:ident, $key:literal) => {
                    if let Some(v) = b.get($key) {
                        cfg.bar.$field = parse_module_list(&v.value);
                    }
                };
            }
            modules!(modules_left, "modules_left");
            modules!(modules_center, "modules_center");
            modules!(modules_right, "modules_right");

            macro_rules! col {
                ($field:ident, $key:literal) => {
                    if let Some(v) = b.get($key) {
                        if let Some(c) = color_from_value(&v.value) {
                            cfg.bar.$field = c;
                        }
                    }
                };
            }
            col!(bg, "bg");
            col!(fg, "fg");
            col!(accent, "accent");
            col!(dim, "dim");
            col!(separator_color, "separator_color");
            col!(active_ws_fg, "active_ws_fg");
            col!(active_ws_bg, "active_ws_bg");
            col!(occupied_ws_fg, "occupied_ws_fg");
            col!(inactive_ws_fg, "inactive_ws_fg");

            if let Some(v) = b.get("separator") {
                if let Some(b2) = v.value.as_bool() {
                    cfg.bar.separator = b2;
                }
            }
            if let Some(v) = b.get("separator_top") {
                if let Some(b2) = v.value.as_bool() {
                    cfg.bar.separator_top = b2;
                }
            }
            if let Some(v) = b.get("pill_radius") {
                if let Some(px) = v.value.as_px() {
                    cfg.bar.pill_radius = px;
                }
            }
        }

        // ── bar_module <name> { } ─────────────────────────────────────────────
        for b in f.blocks("bar_module") {
            let Some(label) = b.label.as_ref().map(|l| l.value.clone()) else {
                continue;
            };
            let kind = BarModuleKind::from_name(&label);
            let mut props = HashMap::new();
            for item in &b.items {
                if let parser::Item::Assignment(a) = item {
                    props.insert(a.key.value.clone(), a.value.value.clone());
                }
            }
            cfg.bar_modules.insert(
                label.clone(),
                BarModuleDef {
                    name: label,
                    kind,
                    props,
                },
            );
        }

        // ── monitor <name> { } ────────────────────────────────────────────────
        cfg.monitors.clear();
        for b in f.blocks("monitor") {
            warn_unknown_keys("monitor", b, KNOWN_MONITOR);

            let name = b
                .label
                .as_ref()
                .map(|l| l.value.clone())
                .unwrap_or_else(|| "unknown".into());
            let mut m = MonitorConfig {
                name,
                ..MonitorConfig::default()
            };

            if let Some(v) = b.get("width") {
                if let Some(px) = v.value.as_px() {
                    m.width = px;
                }
            }
            if let Some(v) = b.get("height") {
                if let Some(px) = v.value.as_px() {
                    m.height = px;
                }
            }
            if let Some(v) = b.get("refresh") {
                // Accept both `144hz` (Dimension) and bare `144` (Int).
                if let Some(hz) = v
                    .value
                    .as_hz()
                    .or_else(|| v.value.as_px())
                    .or_else(|| v.value.as_i64().map(|n| n as u32))
                {
                    m.refresh = hz;
                }
            }
            if let Some(v) = b.get("position") {
                // `position = x, y` parses as an implicit array of two ints.
                if let Value::Array(parts) = &v.value {
                    if parts.len() == 2 {
                        let x = parts[0].value.as_i64().unwrap_or(0) as i32;
                        let y = parts[1].value.as_i64().unwrap_or(0) as i32;
                        m.position = (x, y);
                    }
                } else if let Some(n) = v.value.as_i64() {
                    m.position = (n as i32, 0);
                }
            }
            if let Some(v) = b.get("scale") {
                if let Some(f) = v.value.as_f32() {
                    m.scale = f;
                }
            }

            cfg.monitors.push(m);
        }

        // Fall back to a single default monitor if none configured.
        if cfg.monitors.is_empty() {
            cfg.monitors.push(MonitorConfig::default());
        }

        // Fall back to hardcoded keybinds only if the config defined none at all.
        if cfg.keybinds.is_empty() {
            cfg.keybinds.extend(default_keybinds());
        }

        // ── Warn on silently accepted blocks ──────────────────────────────────
        // (animations, general — parse fine, no warnings needed)
        for block_name in SILENT_BLOCKS {
            let _ = f.block(block_name); // just touch it so the compiler knows it's used
        }

        // ── keybind = COMBO, action, args... ──────────────────────────────────
        cfg.keybinds.clear();
        for sv in f.get_all("keybind") {
            // Collect parts as owned strings — needed because as_arg_string can
            // return Cow::Owned for integer tokens (e.g. workspace numbers).
            let owned: Vec<String> = match &sv.value {
                Value::Array(items) => items
                    .iter()
                    .filter_map(|p| p.value.as_arg_string().map(|s| s.into_owned()))
                    .collect(),
                v => {
                    if let Some(s) = v.as_arg_string() {
                        vec![s.into_owned()]
                    } else {
                        continue;
                    }
                }
            };
            let parts: Vec<&str> = owned.iter().map(String::as_str).collect();
            if parts.len() < 2 {
                tracing::warn!(
                    "config: keybind needs at least combo + action (line {})",
                    sv.span.line
                );
                continue;
            }
            match KeyCombo::parse(parts[0]) {
                Some(combo) => match KeyAction::parse(parts[1], &parts[2..]) {
                    Some(action) => cfg.keybinds.push((combo, action)),
                    None => tracing::warn!(
                        "config: unknown keybind action '{}' (line {})",
                        parts[1],
                        sv.span.line
                    ),
                },
                None => tracing::warn!(
                    "config: could not parse key combo '{}' (line {})",
                    parts[0],
                    sv.span.line
                ),
            }
        }

        // ── window_rule ───────────────────────────────────────────────────────
        for sv in f.get_all("window_rule") {
            let strs: Vec<&str> = match &sv.value {
                Value::Array(items) => items.iter().filter_map(|p| p.value.as_str()).collect(),
                v => {
                    if let Some(s) = v.as_str() {
                        vec![s]
                    } else {
                        continue;
                    }
                }
            };
            if let Some(first) = strs.first() {
                if let Some(matcher) = RuleMatcher::parse(first) {
                    let effects = strs[1..]
                        .iter()
                        .filter_map(|s| RuleEffect::parse(s))
                        .collect();
                    cfg.window_rules.push(WindowRule { matcher, effects });
                }
            }
        }

        // ── exec_once / exec ──────────────────────────────────────────────────
        for sv in f.get_all("exec_once") {
            if let Some(s) = sv.value.as_str() {
                cfg.exec_once.push(ExecEntry::parse(s));
            }
        }
        for sv in f.get_all("exec") {
            if let Some(s) = sv.value.as_str() {
                cfg.exec.push(ExecEntry::parse(s));
            }
        }

        cfg
    }

    pub fn hot_reload(&mut self) {
        let new = Self::from_path(&config_path());
        // Intentionally NOT reloading font_path — a font change requires
        // re-initialising trixui which can't be done mid-session.
        self.font_size = new.font_size;
        self.gap = new.gap;
        self.border_width = new.border_width;
        self.colors = new.colors;
        self.bar = new.bar;
        self.bar_modules = new.bar_modules;
        self.keybinds = new.keybinds;
        self.window_rules = new.window_rules;
        self.keyboard = new.keyboard;
        self.monitors = new.monitors;
        self.seat_name = new.seat_name;
        self.workspaces = new.workspaces;
        self.exec = new.exec;
        // exec_once intentionally not reloaded — only runs at session start.
    }
}

fn parse_module_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items
            .iter()
            .filter_map(|sv| sv.value.as_str().map(String::from))
            .collect(),
        Value::Ident(s) | Value::String(s) => vec![s.clone()],
        _ => vec![],
    }
}
