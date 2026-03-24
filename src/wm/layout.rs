// src/wm/layout.rs — Layout engine.

use super::Rect;

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
///
/// * `inner_gap` — pixels between adjacent windows
/// * `outer_gap` — pixels between windows and the usable-area edge
pub fn compute(
    layout: Layout,
    area: Rect,
    n: usize,
    ratio: f32,
    inner_gap: i32,
    outer_gap: i32,
) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    // Shrink the available area by outer_gap on every side once, here.
    // Individual layout functions never apply outer_gap themselves —
    // this is the single source of truth for the outer margin.
    let padded = if outer_gap > 0 {
        area.inset(outer_gap)
    } else {
        area
    };
    match layout {
        Layout::MasterStack => master_stack(padded, n, ratio, inner_gap),
        Layout::Bsp => bsp(padded, n, inner_gap),
        // Monocle: all windows occupy the full padded area, stacked.
        Layout::Monocle => vec![padded; n],
        // Float: geometry is managed by the WM directly, not by layout.
        Layout::Float => vec![Rect::default(); n],
    }
}

// ── Master-Stack ──────────────────────────────────────────────────────────────

fn master_stack(area: Rect, n: usize, ratio: f32, gap: i32) -> Vec<Rect> {
    // Single window always fills the padded area — no master/slave split.
    // This path is taken for n==1 but also naturally handles the transition
    // back from 2→1 windows without any size jump because the outer_gap
    // padding is applied identically in compute() above.
    if n == 1 {
        return vec![area];
    }

    // Master column width — clamped so neither column collapses.
    let master_w = ((area.w as f32 * ratio) as i32)
        .max(80)
        .min(area.w - gap - 80);
    let slave_w = (area.w - master_w - gap).max(80);
    let slave_n = n - 1;

    let mut rects = Vec::with_capacity(n);

    // Master: full height of the padded area.
    rects.push(Rect::new(area.x, area.y, master_w, area.h));

    // Stack: divide remaining height evenly among slave_n windows.
    // Use integer arithmetic that avoids cumulative rounding error:
    // compute each window's top edge from the total available height
    // rather than by adding individual heights in a loop.
    let total_h = area.h;
    let total_gap = gap * (slave_n as i32 - 1);
    let available_h = (total_h - total_gap).max(slave_n as i32);
    let slave_x = area.x + master_w + gap;

    for i in 0..slave_n {
        // Distribute height fairly: each slot gets floor(available_h / slave_n)
        // and the remainder pixels go to the last slot.
        let y_off = (available_h * i as i32) / slave_n as i32 + gap * i as i32;
        let y_next = if i + 1 == slave_n {
            total_h
        } else {
            (available_h * (i as i32 + 1)) / slave_n as i32 + gap * (i as i32 + 1)
        };
        let h = (y_next - y_off - if i + 1 == slave_n { 0 } else { gap }).max(1);
        rects.push(Rect::new(slave_x, area.y + y_off, slave_w, h));
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
            let (a, b) = if split_h {
                let hw = ((area.w - gap) / 2).max(1);
                (
                    Rect::new(area.x, area.y, hw, area.h),
                    Rect::new(
                        area.x + hw + gap,
                        area.y,
                        (area.w - hw - gap).max(1),
                        area.h,
                    ),
                )
            } else {
                let hh = ((area.h - gap) / 2).max(1);
                (
                    Rect::new(area.x, area.y, area.w, hh),
                    Rect::new(
                        area.x,
                        area.y + hh + gap,
                        area.w,
                        (area.h - hh - gap).max(1),
                    ),
                )
            };
            bsp_recurse(a, start, mid, gap, !split_h, out);
            bsp_recurse(b, start + mid, n - mid, gap, !split_h, out);
        }
    }
}
