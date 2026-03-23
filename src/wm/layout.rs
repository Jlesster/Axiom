// src/wm/layout.rs — Layout engine.
//
// Calling convention matches wm/mod.rs::reflow():
//   layout::compute(layout: Layout, area: Rect, n: usize, ratio: f32, gap: i32) -> Vec<Rect>
//
// Returns a Vec<Rect> in the same order as the `tiled` window slice.
// The caller (WmState::reflow) zips by index: tiled[i] → rects[i].

use super::Rect;

// ── Layout enum ───────────────────────────────────────────────────────────────
// Named `Layout` to match `pub use layout::Layout` in mod.rs.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    MasterStack,
    Bsp,
    Monocle,
    Float,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Compute geometry for `n` tiled windows within `area`.
/// Returns exactly `n` rects in window-order (index matches caller's tiled[]).
pub fn compute(layout: Layout, area: Rect, n: usize, ratio: f32, gap: i32) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    match layout {
        Layout::MasterStack => master_stack(area, n, ratio, gap),
        Layout::Bsp => bsp(area, n, gap),
        Layout::Monocle => vec![area; n],
        Layout::Float => vec![Rect::default(); n],
    }
}

// ── Master-Stack ──────────────────────────────────────────────────────────────

fn master_stack(area: Rect, n: usize, ratio: f32, gap: i32) -> Vec<Rect> {
    if n == 1 {
        return vec![area];
    }
    let master_w = ((area.w as f32 * ratio) as i32).max(1);
    let slave_w = (area.w - master_w - gap).max(1);
    let slave_n = n - 1;
    let mut rects = Vec::with_capacity(n);

    // Master — full height.
    rects.push(Rect::new(area.x, area.y, master_w, area.h));

    // Slaves — right column, stacked.
    let total_gap = gap * (slave_n as i32 - 1);
    let each_h = ((area.h - total_gap) / slave_n as i32).max(1);
    let slave_x = area.x + master_w + gap;
    for i in 0..slave_n {
        let y = area.y + (each_h + gap) * i as i32;
        let h = if i == slave_n - 1 {
            area.y + area.h - y
        } else {
            each_h
        };
        rects.push(Rect::new(slave_x, y, slave_w, h));
    }
    rects
}

// ── BSP ───────────────────────────────────────────────────────────────────────

fn bsp(area: Rect, n: usize, gap: i32) -> Vec<Rect> {
    let mut rects = vec![Rect::default(); n];
    bsp_recurse(area, 0, n, gap, true, &mut rects);
    rects
}

fn bsp_recurse(area: Rect, start: usize, n: usize, gap: i32, split_h: bool, out: &mut Vec<Rect>) {
    match n {
        0 => {}
        1 => {
            out[start] = area;
        }
        _ => {
            let mid = n / 2;
            let (area_a, area_b) = if split_h {
                let hw = (area.w - gap) / 2;
                (
                    Rect::new(area.x, area.y, hw, area.h),
                    Rect::new(area.x + hw + gap, area.y, area.w - hw - gap, area.h),
                )
            } else {
                let hh = (area.h - gap) / 2;
                (
                    Rect::new(area.x, area.y, area.w, hh),
                    Rect::new(area.x, area.y + hh + gap, area.w, area.h - hh - gap),
                )
            };
            bsp_recurse(area_a, start, mid, gap, !split_h, out);
            bsp_recurse(area_b, start + mid, n - mid, gap, !split_h, out);
        }
    }
}
