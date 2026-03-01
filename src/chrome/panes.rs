// chrome/panes.rs — pixel-perfect pane borders + titles.

use trixui::{
    renderer::{BorderSide, Color as PixColor, TextStyle},
    PixelCanvas,
};

use crate::config::Colors;
use crate::twm::{PaneSnap, Rect};

// ── Colour helpers ────────────────────────────────────────────────────────────

#[inline]
fn c(col: crate::config::Color) -> PixColor {
    PixColor::rgba(col.r, col.g, col.b, col.a)
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn draw_workspace_panes(
    canvas: &mut PixelCanvas,
    panes: &[PaneSnap],
    focused: Option<crate::twm::PaneId>,
    colors: &Colors,
    border_w: u32,
    _title_h: u32,
    cell_w: u32,
    cell_h: u32,
) {
    let bw = border_w.max(1).min(2);

    for pane in panes {
        let focused = Some(pane.id) == focused;
        draw_pane(canvas, pane, focused, colors, bw, cell_w, cell_h);
    }
}

// ── Single pane ───────────────────────────────────────────────────────────────

fn draw_pane(
    canvas: &mut PixelCanvas,
    pane: &PaneSnap,
    focused: bool,
    colors: &Colors,
    bw: u32,
    cell_w: u32,
    cell_h: u32,
) {
    let r = pane.rect;
    if r.w < bw * 2 || r.h < bw * 2 {
        return;
    }

    let border_col = if focused {
        c(colors.active_border)
    } else {
        c(colors.inactive_border)
    };

    // Border lines only — NO interior fill.
    // The DRM compositor clear_color handles the background.
    canvas.border(r.x, r.y, r.w, r.h, BorderSide::ALL, border_col, bw);

    // Title on top border.
    if !pane.fullscreen && cell_w > 0 && cell_h > 0 {
        draw_title_on_border(canvas, pane, focused, colors, r, bw, cell_w, cell_h);
    }
}

// ── Title rendering ───────────────────────────────────────────────────────────

fn draw_title_on_border(
    canvas: &mut PixelCanvas,
    pane: &PaneSnap,
    focused: bool,
    colors: &Colors,
    r: Rect,
    bw: u32,
    cell_w: u32,
    cell_h: u32,
) {
    let indicator = if pane.is_embedded { "󰖟" } else { "◆" };
    let raw_title = format!(" {} [{}] {} ", pane.title, pane.id, indicator);

    let max_chars = (r.w as usize).saturating_sub(cell_w as usize * 4) / cell_w as usize;
    let title = truncate(&raw_title, max_chars);
    if title.is_empty() {
        return;
    }

    let title_px_w = title.chars().count() as u32 * cell_w;
    let tx = r.x + cell_w * 2;

    if tx + title_px_w > r.x + r.w {
        return;
    }

    // Erase the top border line behind the title text only —
    // this is a small notch, not a full interior fill.
    let cut_h = cell_h.min(r.h / 2);
    let ty = r.y + (cut_h.saturating_sub(cell_h)) / 2;

    let bg_col = c(colors.pane_bg);
    // Only fill the exact title notch — nothing else.
    canvas.fill(tx, r.y, title_px_w, cut_h, bg_col);

    let (fg, bg) = if focused {
        (c(colors.active_border), bg_col)
    } else {
        (c(colors.inactive_border), bg_col)
    };

    let style = TextStyle {
        fg,
        bg: PixColor::TRANSPARENT,
        bold: focused,
        italic: false,
    };

    canvas.text_maxw(tx, ty, &title, style, title_px_w + cell_w);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if max == 0 {
        return String::new();
    }
    if n <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}
