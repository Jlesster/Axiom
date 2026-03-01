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
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('+').collect();
        if parts.is_empty() {
            return None;
        }
        let key = parts.last()?.to_lowercase();
        let mut mods = Modifiers::empty();
        for m in &parts[..parts.len() - 1] {
            match m.to_lowercase().as_str() {
                "super" | "mod4" | "logo" => mods |= Modifiers::SUPER,
                "ctrl" | "control" => mods |= Modifiers::CTRL,
                "alt" | "mod1" => mods |= Modifiers::ALT,
                "shift" => mods |= Modifiers::SHIFT,
                _ => {}
            }
        }
        Some(Self { mods, key })
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
    Custom(String, Vec<String>),

    SwitchVt(i32),
    EmergencyQuit,
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

#[derive(Debug, Clone)]
pub struct BarConfig {
    pub position: BarPosition,
    pub height: u32,
    pub modules_left: Vec<String>,
    pub modules_center: Vec<String>,
    pub modules_right: Vec<String>,
    pub bg: Color,
    pub fg: Color,
    pub padding: u32,
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
            modules_left: vec!["workspaces".into()],
            modules_center: vec!["clock".into()],
            modules_right: vec!["layout".into()],
            bg: Color::hex(0x181825),
            fg: Color::hex(0xa6adc8),
            padding: 8,
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
            font_size: 20.0,
            gap: 4,
            border_width: 1,
            colors: Colors::default(),
            bar: BarConfig::default(),
            bar_modules: HashMap::new(),
            keybinds: default_keybinds(),
            window_rules: Vec::new(),
            keyboard: KeyboardConfig::default(),
            monitors: vec![MonitorConfig::default()],
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
            KeyCombo::parse("Super+Return").unwrap(),
            KeyAction::Exec("foot".into(), vec![]),
        ),
        (KeyCombo::parse("Super+q").unwrap(), KeyAction::Close),
        (KeyCombo::parse("Super+f").unwrap(), KeyAction::Fullscreen),
        (KeyCombo::parse("Super+b").unwrap(), KeyAction::ToggleBar),
        (KeyCombo::parse("Super+h").unwrap(), KeyAction::FocusLeft),
        (KeyCombo::parse("Super+l").unwrap(), KeyAction::FocusRight),
        (KeyCombo::parse("Super+k").unwrap(), KeyAction::FocusUp),
        (KeyCombo::parse("Super+j").unwrap(), KeyAction::FocusDown),
        (
            KeyCombo::parse("Super+Shift+h").unwrap(),
            KeyAction::MoveLeft,
        ),
        (
            KeyCombo::parse("Super+Shift+l").unwrap(),
            KeyAction::MoveRight,
        ),
        (KeyCombo::parse("Super+Tab").unwrap(), KeyAction::NextLayout),
        (KeyCombo::parse("Super+equal").unwrap(), KeyAction::GrowMain),
        (
            KeyCombo::parse("Super+minus").unwrap(),
            KeyAction::ShrinkMain,
        ),
        (
            KeyCombo::parse("Super+Right").unwrap(),
            KeyAction::NextWorkspace,
        ),
        (
            KeyCombo::parse("Super+Left").unwrap(),
            KeyAction::PrevWorkspace,
        ),
    ]
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        tracing::info!("Config::load reading {:?}", path); // add this
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                tracing::info!("Config::load read {} bytes, parsing", src.len()); // add this
                Self::from_source(&src, &path)
            }
            Err(e) => {
                tracing::warn!("Could not read config {:?}: {e} — using defaults", path);
                Self::default()
            }
        }
    }

    pub fn from_source(src: &str, path: &Path) -> Self {
        tracing::info!("Config::from_source parsing {} bytes", src.len()); // add this
        let result = parse(src);
        tracing::info!("Config parse done, {} errors", result.errors.len()); // add this
        for e in &result.errors {
            tracing::warn!("Config parse error in {:?}: {e}", path);
        }
        Self::from_file(result.file)
    }

    pub fn from_file(f: ConfigFile) -> Self {
        let mut cfg = Self::default();

        if let Some(v) = f.get("font") {
            if let Some(s) = v.value.as_str() {
                cfg.font_path = PathBuf::from(s);
            }
        }
        if let Some(v) = f.get("font_size") {
            if let Some(n) = v.value.as_f64() {
                cfg.font_size = n as f32;
            }
        }
        if let Some(v) = f.get("gap") {
            if let Some(px) = v.value.as_px() {
                cfg.gap = px;
            }
        }
        if let Some(v) = f.get("border_width") {
            if let Some(px) = v.value.as_px() {
                cfg.border_width = px;
            }
        }
        if let Some(v) = f.get("seat") {
            if let Some(s) = v.value.as_str() {
                cfg.seat_name = s.to_string();
            }
        }
        if let Some(v) = f.get("workspaces") {
            if let Some(n) = v.value.as_i64() {
                cfg.workspaces = n.clamp(1, 32) as u8;
            }
        }

        if let Some(b) = f.block("colors") {
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

        if let Some(b) = f.block("keyboard") {
            if let Some(v) = b.get("layout") {
                cfg.keyboard.layout = v.value.as_str().map(String::from);
            }
            if let Some(v) = b.get("variant") {
                cfg.keyboard.variant = v.value.as_str().map(String::from);
            }
            if let Some(v) = b.get("options") {
                cfg.keyboard.options = v.value.as_str().map(String::from);
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

        if let Some(b) = f.block("bar") {
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
            if let Some(v) = b.get("bg") {
                if let Some(c) = color_from_value(&v.value) {
                    cfg.bar.bg = c;
                }
            }
            if let Some(v) = b.get("fg") {
                if let Some(c) = color_from_value(&v.value) {
                    cfg.bar.fg = c;
                }
            }
        }

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

        cfg.keybinds.clear();
        cfg.keybinds.extend(default_keybinds());
        for sv in f.get_all("keybind") {
            if let Value::Array(parts) = &sv.value {
                let strs: Vec<&str> = parts.iter().filter_map(|p| p.value.as_str()).collect();
                if strs.len() >= 2 {
                    if let Some(combo) = KeyCombo::parse(strs[0]) {
                        if let Some(action) = KeyAction::parse(strs[1], &strs[2..]) {
                            cfg.keybinds.push((combo, action));
                        }
                    }
                }
            }
        }

        for sv in f.get_all("window_rule") {
            if let Value::Array(parts) = &sv.value {
                let strs: Vec<&str> = parts.iter().filter_map(|p| p.value.as_str()).collect();
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
        }

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
        let new = Self::load();
        self.font_size = new.font_size;
        self.gap = new.gap;
        self.border_width = new.border_width;
        self.colors = new.colors;
        self.bar = new.bar;
        self.bar_modules = new.bar_modules;
        self.keybinds = new.keybinds;
        self.window_rules = new.window_rules;
        self.keyboard = new.keyboard;
        self.exec = new.exec;
        self.exec_once = new.exec_once;
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
