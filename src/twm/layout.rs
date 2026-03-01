// twm/layout.rs — all layout algorithms in pure pixel space.

use super::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Bsp,
    Columns,
    Rows,
    Monocle,
    ThreeCol,
}

impl Layout {
    pub fn next(self) -> Self {
        match self {
            Self::Bsp => Self::Columns,
            Self::Columns => Self::Rows,
            Self::Rows => Self::ThreeCol,
            Self::ThreeCol => Self::Monocle,
            Self::Monocle => Self::Bsp,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Self::Bsp => Self::Monocle,
            Self::Columns => Self::Bsp,
            Self::Rows => Self::Columns,
            Self::ThreeCol => Self::Rows,
            Self::Monocle => Self::ThreeCol,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Bsp => "BSP",
            Self::Columns => "Columns",
            Self::Rows => "Rows",
            Self::ThreeCol => "ThreeCol",
            Self::Monocle => "Monocle",
        }
    }
}

/// Compute pixel rects for `n` tiled panes in `area`.
pub fn compute(layout: Layout, area: Rect, n: usize, main_ratio: f32, gap: u32) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    match layout {
        Layout::Bsp => bsp(area, n, gap),
        Layout::Columns => columns(area, n, main_ratio, gap),
        Layout::Rows => rows(area, n, main_ratio, gap),
        Layout::ThreeCol => three_col(area, n, main_ratio, gap),
        Layout::Monocle => vec![area; n],
    }
}

// ── BSP ───────────────────────────────────────────────────────────────────────

fn bsp(area: Rect, n: usize, gap: u32) -> Vec<Rect> {
    bsp_inner(area, n, gap, area.w >= area.h)
}

fn bsp_inner(area: Rect, n: usize, gap: u32, split_vert: bool) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![area];
    }
    if split_vert {
        let half_w = area.w.saturating_sub(gap) / 2;
        let rest_w = area.w.saturating_sub(half_w + gap);
        let left = Rect::new(area.x, area.y, half_w, area.h);
        let right = Rect::new(area.x + half_w + gap, area.y, rest_w, area.h);
        let mut out = vec![left];
        out.extend(bsp_inner(right, n - 1, gap, false));
        out
    } else {
        let half_h = area.h.saturating_sub(gap) / 2;
        let rest_h = area.h.saturating_sub(half_h + gap);
        let top = Rect::new(area.x, area.y, area.w, half_h);
        let bot = Rect::new(area.x, area.y + half_h + gap, area.w, rest_h);
        let mut out = vec![top];
        out.extend(bsp_inner(bot, n - 1, gap, true));
        out
    }
}

// ── Columns ───────────────────────────────────────────────────────────────────
// Main pane on the left; rest stacked vertically on the right.

fn columns(area: Rect, n: usize, ratio: f32, gap: u32) -> Vec<Rect> {
    if n == 1 {
        return vec![area];
    }
    let main_w = ((area.w as f32 * ratio) as u32).max(4);
    let side_w = area.w.saturating_sub(main_w + gap);
    let rest = n - 1;
    let gaps = gap * (rest as u32).saturating_sub(1);
    let each_h = area.h.saturating_sub(gaps) / rest as u32;

    let mut out = vec![Rect::new(area.x, area.y, main_w, area.h)];
    for i in 0..rest {
        let y = area.y + i as u32 * (each_h + gap);
        let h = if i + 1 == rest {
            area.y + area.h - y
        } else {
            each_h
        };
        out.push(Rect::new(area.x + main_w + gap, y, side_w, h));
    }
    out
}

// ── Rows ─────────────────────────────────────────────────────────────────────
// Main pane on top; rest arranged horizontally below.

fn rows(area: Rect, n: usize, ratio: f32, gap: u32) -> Vec<Rect> {
    if n == 1 {
        return vec![area];
    }
    let main_h = ((area.h as f32 * ratio) as u32).max(3);
    let side_h = area.h.saturating_sub(main_h + gap);
    let rest = n - 1;
    let gaps = gap * (rest as u32).saturating_sub(1);
    let each_w = area.w.saturating_sub(gaps) / rest as u32;

    let mut out = vec![Rect::new(area.x, area.y, area.w, main_h)];
    for i in 0..rest {
        let x = area.x + i as u32 * (each_w + gap);
        let w = if i + 1 == rest {
            area.x + area.w - x
        } else {
            each_w
        };
        out.push(Rect::new(x, area.y + main_h + gap, w, side_h));
    }
    out
}

// ── ThreeCol ─────────────────────────────────────────────────────────────────
// Centre column is main; left and right stacks flank it.

fn three_col(area: Rect, n: usize, ratio: f32, gap: u32) -> Vec<Rect> {
    if n == 1 {
        return vec![area];
    }
    if n == 2 {
        return columns(area, n, ratio, gap);
    }

    let main_w = ((area.w as f32 * ratio) as u32).max(4);
    let side_w = area.w.saturating_sub(main_w + gap * 2) / 2;
    let cx = area.x + side_w + gap;

    // centre main
    let main = Rect::new(cx, area.y, main_w, area.h);

    // left stack
    let left_count = (n - 1) / 2;
    let right_count = (n - 1) - left_count;

    let mut out = vec![main];

    let stack = |count: usize, sx: u32| -> Vec<Rect> {
        if count == 0 {
            return vec![];
        }
        let gaps = gap * (count as u32).saturating_sub(1);
        let each_h = area.h.saturating_sub(gaps) / count as u32;
        (0..count)
            .map(|i| {
                let y = area.y + i as u32 * (each_h + gap);
                let h = if i + 1 == count {
                    area.y + area.h - y
                } else {
                    each_h
                };
                Rect::new(sx, y, side_w, h)
            })
            .collect()
    };

    out.extend(stack(left_count, area.x));
    out.extend(stack(right_count, cx + main_w + gap));
    out
}
