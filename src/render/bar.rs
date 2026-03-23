// src/render/bar.rs — AwesomeWM-parity compositor status bar.

use crate::{render::font::FontAtlas, wm::WmState};
use std::time::{Duration, Instant};

// ── Catppuccin Mocha ──────────────────────────────────────────────────────────

pub mod col {
    pub const BASE: [f32; 4] = [0.114, 0.122, 0.176, 1.0];
    pub const MANTLE: [f32; 4] = [0.094, 0.102, 0.149, 1.0];
    pub const SURFACE0: [f32; 4] = [0.180, 0.188, 0.251, 1.0];
    pub const SURFACE1: [f32; 4] = [0.239, 0.247, 0.322, 1.0];
    pub const OVERLAY0: [f32; 4] = [0.365, 0.373, 0.467, 1.0];
    pub const TEXT: [f32; 4] = [0.804, 0.839, 0.957, 1.0];
    pub const SUBTEXT1: [f32; 4] = [0.675, 0.710, 0.847, 1.0];
    pub const LAVENDER: [f32; 4] = [0.706, 0.745, 0.996, 1.0];
    pub const MAUVE: [f32; 4] = [0.804, 0.651, 0.969, 1.0];
    pub const BLUE: [f32; 4] = [0.537, 0.706, 0.980, 1.0];
    pub const GREEN: [f32; 4] = [0.651, 0.890, 0.631, 1.0];
    pub const YELLOW: [f32; 4] = [0.976, 0.886, 0.686, 1.0];
    pub const PEACH: [f32; 4] = [0.980, 0.702, 0.529, 1.0];
    pub const RED: [f32; 4] = [0.953, 0.545, 0.659, 1.0];

    pub fn alpha(c: [f32; 4], a: f32) -> [f32; 4] {
        [c[0], c[1], c[2], a]
    }
    pub fn lerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
        let t = t.clamp(0.0, 1.0);
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ]
    }
}

// ── BarConfig ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct BarConfig {
    pub height: u32,
    pub font_size: u32,
    pub pad_x: f32,
    pub tag_min_w: f32,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            height: 28,
            font_size: 13,
            pad_x: 10.0,
            tag_min_w: 24.0,
        }
    }
}

// ── SysStats ──────────────────────────────────────────────────────────────────

pub struct SysStats {
    last: Instant,
    prev_idle: u64,
    prev_total: u64,
    pub cpu: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
}

impl SysStats {
    pub fn new() -> Self {
        let (i, t) = cpu_ticks().unwrap_or((0, 1));
        Self {
            last: Instant::now() - Duration::from_secs(2),
            prev_idle: i,
            prev_total: t,
            cpu: 0.0,
            mem_used_mb: 0,
            mem_total_mb: 0,
        }
    }
    pub fn poll(&mut self) {
        if self.last.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last = Instant::now();
        if let Some((idle, total)) = cpu_ticks() {
            let di = idle.saturating_sub(self.prev_idle);
            let dt = total.saturating_sub(self.prev_total).max(1);
            self.cpu = 100.0 * (1.0 - di as f32 / dt as f32);
            self.prev_idle = idle;
            self.prev_total = total;
        }
        if let Some((used, total)) = mem_kb() {
            self.mem_used_mb = used / 1024;
            self.mem_total_mb = total / 1024;
        }
    }
}

fn cpu_ticks() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let nums: Vec<u64> = s
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if nums.len() < 4 {
        return None;
    }
    Some((nums[3], nums.iter().sum()))
}
fn mem_kb() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = 0u64;
    let mut avail = 0u64;
    for l in s.lines() {
        if l.starts_with("MemTotal:") {
            total = parse_kb(l)?;
        }
        if l.starts_with("MemAvailable:") {
            avail = parse_kb(l)?;
        }
    }
    Some((total.saturating_sub(avail), total))
}
fn parse_kb(l: &str) -> Option<u64> {
    l.split_whitespace().nth(1)?.parse().ok()
}

pub fn wall_time() -> String {
    let mut ts = Ts { sec: 0, nsec: 0 };
    unsafe {
        clock_gettime(0, &mut ts);
    }
    let s = ts.sec as u64 % 86400;
    format!("{:02}:{:02}", s / 3600, (s % 3600) / 60)
}
#[repr(C)]
struct Ts {
    sec: i64,
    nsec: i64,
}
extern "C" {
    fn clock_gettime(clk: i32, tp: *mut Ts) -> i32;
}

// ── DrawCtx ───────────────────────────────────────────────────────────────────

/// Thin drawing interface passed into BarState::draw.
/// All callbacks take proj by value ([f32;9] is Copy) to avoid lifetime issues.
pub struct DrawCtx<'a> {
    pub proj: &'a [f32; 9],
    pub font: &'a mut FontAtlas,
    /// Fill a rect: (proj, x, y, w, h, colour)
    pub fill: &'a dyn Fn([f32; 9], f32, f32, f32, f32, [f32; 4]),
    /// Draw a glyph: (proj, x, y, w, h, tex_id, colour, uv[4], is_lcd)
    pub glyph: &'a dyn Fn([f32; 9], f32, f32, f32, f32, u32, [f32; 4], [f32; 4], bool),
}

impl<'a> DrawCtx<'a> {
    pub fn rect(&self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        (self.fill)(*self.proj, x, y, w, h, c);
    }

    /// Draw text starting at pixel-baseline `y`, return new x.
    pub fn text(&mut self, mut x: f32, y: f32, s: &str, sz: u32, c: [f32; 4]) -> f32 {
        for ch in s.chars() {
            if ch == ' ' {
                // Use the advance of the space glyph if available, else estimate.
                let sp = self
                    .font
                    .glyph(' ', sz)
                    .map(|g| g.advance as f32)
                    .unwrap_or(sz as f32 * 0.3);
                x += sp;
                continue;
            }
            let Some(g) = self.font.glyph(ch, sz) else {
                x += (sz / 2) as f32;
                continue;
            };
            if g.px_w > 0 && g.px_h > 0 {
                let gx = (x + g.bearing_x as f32).floor();
                // y is baseline; bearing_y is pixels above baseline to glyph top.
                let gy = (y - g.bearing_y as f32).floor();
                (self.glyph)(
                    *self.proj,
                    gx,
                    gy,
                    g.px_w as f32,
                    g.px_h as f32,
                    g.tex_id,
                    c,
                    g.uv,
                    g.lcd,
                );
            }
            x += g.advance as f32;
        }
        x
    }

    /// Measure text pixel width.
    pub fn measure(&mut self, s: &str, sz: u32) -> f32 {
        s.chars()
            .map(|ch| {
                self.font
                    .glyph(ch, sz)
                    .map(|g| g.advance as f32)
                    .unwrap_or(sz as f32 * 0.5)
            })
            .sum()
    }

    /// Draw text with baseline placed so glyphs are vertically centred in `bar_h`.
    pub fn text_mid(&mut self, x: f32, bar_h: f32, s: &str, sz: u32, c: [f32; 4]) -> f32 {
        // cap-height ≈ 0.7 × em; centre cap-height within bar.
        let cap = sz as f32 * 0.70;
        let base = ((bar_h + cap) * 0.5).floor();
        self.text(x, base, s, sz, c)
    }

    pub fn sep(&self, x: f32, bar_h: f32) {
        self.rect(x, 3.0, 1.0, bar_h - 6.0, col::SURFACE1);
    }
}

// ── BarState ──────────────────────────────────────────────────────────────────

pub struct BarState {
    pub cfg: BarConfig,
    pub stats: SysStats,
}

impl BarState {
    pub fn new(cfg: BarConfig) -> Self {
        Self {
            cfg,
            stats: SysStats::new(),
        }
    }

    pub fn tick(&mut self) {
        self.stats.poll();
    }

    pub fn draw(&mut self, ctx: &mut DrawCtx, wm: &WmState, out_w: f32, mon_idx: usize) {
        let h = self.cfg.height as f32;
        let fs = self.cfg.font_size;
        let px = self.cfg.pad_x;

        let aws = wm
            .monitors
            .get(mon_idx)
            .map(|m| m.active_ws)
            .unwrap_or_else(|| wm.active_ws());

        // ── Background ────────────────────────────────────────────────────────
        ctx.rect(0.0, 0.0, out_w, h, col::MANTLE);

        // ── Left: tag pills ───────────────────────────────────────────────────
        let mut lx = px * 0.5;
        for ws in &wm.workspaces {
            let active = ws.index == aws;
            let occupied = !ws.windows.is_empty();
            let label = format!("{}", ws.index + 1);
            let lw = ctx.measure(&label, fs).max(self.cfg.tag_min_w - px * 2.0);
            let pill_w = lw + px * 2.0;

            let bg = if active {
                col::MAUVE
            } else if occupied {
                col::SURFACE0
            } else {
                col::alpha(col::SURFACE0, 0.0)
            };
            ctx.rect(lx, 2.0, pill_w, h - 4.0, bg);

            let fg = if active {
                col::BASE
            } else if occupied {
                col::TEXT
            } else {
                col::OVERLAY0
            };
            ctx.text_mid(lx + px, h, &label, fs, fg);

            if !active && occupied {
                ctx.rect(lx + pill_w * 0.5 - 2.0, h - 4.0, 4.0, 2.0, col::LAVENDER);
            }
            lx += pill_w + 2.0;
        }

        lx += 4.0;
        ctx.sep(lx, h);
        lx += 8.0;

        // ── Right widgets (right-to-left) ─────────────────────────────────────
        let mut rx = out_w - px * 0.5;

        // Clock
        let time = wall_time();
        let tw = ctx.measure(&time, fs);
        rx -= tw;
        ctx.text_mid(rx, h, &time, fs, col::LAVENDER);
        rx -= px;
        ctx.sep(rx, h);
        rx -= px;

        // Memory
        if self.stats.mem_total_mb > 0 {
            let pct = self.stats.mem_used_mb as f32 / self.stats.mem_total_mb as f32;
            let mc = col::lerp(col::GREEN, col::RED, (pct - 0.4).max(0.0) * 2.5);
            let ms = format!("{}M/{}M", self.stats.mem_used_mb, self.stats.mem_total_mb);
            let mw = ctx.measure(&ms, fs);
            rx -= mw;
            ctx.text_mid(rx, h, &ms, fs, mc);
            let lbl = "MEM ";
            let lw = ctx.measure(lbl, fs);
            rx -= lw;
            ctx.text_mid(rx, h, lbl, fs, col::SUBTEXT1);
            rx -= px;
            ctx.sep(rx, h);
            rx -= px;
        }

        // CPU
        {
            let cc = cpu_col(self.stats.cpu);
            let cs = format!("{:.0}%", self.stats.cpu.clamp(0.0, 100.0));
            let cw = ctx.measure(&cs, fs);
            rx -= cw;
            ctx.text_mid(rx, h, &cs, fs, cc);
            let lbl = "CPU ";
            let lw = ctx.measure(lbl, fs);
            rx -= lw;
            ctx.text_mid(rx, h, lbl, fs, col::SUBTEXT1);
            rx -= px;
            ctx.sep(rx, h);
            rx -= px;
        }

        // ── Centre: focused window title ──────────────────────────────────────
        let title = wm
            .focused_window()
            .and_then(|id| wm.windows.get(&id))
            .map(|w| w.title.clone())
            .unwrap_or_default();

        if !title.is_empty() {
            let avail = (rx - lx).max(0.0);
            let tw = ctx.measure(&title, fs);
            let tx = if tw <= avail {
                lx + (avail - tw) * 0.5
            } else {
                lx
            };
            // Truncate if needed.
            if tw <= avail {
                ctx.text_mid(tx, h, &title, fs, col::TEXT);
            } else {
                let mut trunc = String::new();
                let mut acc = 0.0f32;
                let ew = ctx.measure("…", fs);
                for ch in title.chars() {
                    let cw = ctx
                        .font
                        .glyph(ch, fs)
                        .map(|g| g.advance as f32)
                        .unwrap_or(fs as f32 * 0.5);
                    if acc + cw + ew > avail {
                        break;
                    }
                    trunc.push(ch);
                    acc += cw;
                }
                trunc.push('…');
                let tw2 = ctx.measure(&trunc, fs);
                ctx.text_mid(lx + (avail - tw2) * 0.5, h, &trunc, fs, col::TEXT);
            }
        }

        // ── Bottom accent line ────────────────────────────────────────────────
        ctx.rect(0.0, h - 1.0, out_w, 1.0, col::SURFACE0);
    }
}

fn cpu_col(p: f32) -> [f32; 4] {
    if p > 80.0 {
        col::RED
    } else if p > 60.0 {
        col::PEACH
    } else if p > 35.0 {
        col::YELLOW
    } else {
        col::GREEN
    }
}
