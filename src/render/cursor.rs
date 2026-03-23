// src/render/cursor.rs — Hardware cursor via DRM dumb buffer.
//
// We load the default left_ptr cursor from the system xcursor theme and
// upload it to a DRM dumb buffer, then set it on each CRTC.  This replaces
// the white rectangle software cursor.

use anyhow::{Context, Result};
use drm::control::{crtc, dumbbuffer::DumbBuffer, Device as ControlDevice};
use std::os::unix::io::AsFd as _;

// 64×64 is supported by virtually all drivers.
const CURSOR_W: u32 = 64;
const CURSOR_H: u32 = 64;

pub struct HwCursor {
    pub buf: DumbBuffer,
    pub hot_x: u32,
    pub hot_y: u32,
}

#[allow(deprecated)]
impl HwCursor {
    /// Load the system xcursor theme and create a DRM dumb buffer for it.
    /// Falls back to a generated arrow if xcursor is unavailable.
    pub fn load(drm: &impl ControlDevice) -> Result<Self> {
        let (pixels, hot_x, hot_y) =
            load_xcursor_pixels().unwrap_or_else(|| (make_arrow_pixels(), 0, 0));

        let mut buf = drm
            .create_dumb_buffer((CURSOR_W, CURSOR_H), drm_fourcc::DrmFourcc::Argb8888, 32)
            .context("create dumb cursor buffer")?;

        {
            let mut map = drm
                .map_dumb_buffer(&mut buf)
                .context("map dumb cursor buffer")?;
            let dst: &mut [u8] = map.as_mut();
            let src: &[u8] = bytemuck::cast_slice(&pixels);
            let copy_len = src.len().min(dst.len());
            dst[..copy_len].copy_from_slice(&src[..copy_len]);
        }

        Ok(Self { buf, hot_x, hot_y })
    }

    /// Set this cursor on a CRTC.
    pub fn set_on_crtc(&self, drm: &impl ControlDevice, crtc_h: crtc::Handle, x: i32, y: i32) {
        let _ = drm.set_cursor2(
            crtc_h,
            Some(&self.buf),
            (self.hot_x as i32, self.hot_y as i32),
        );
        let _ = drm.move_cursor(crtc_h, (x, y));
    }

    /// Move without re-uploading the bitmap.
    pub fn move_on_crtc(&self, drm: &impl ControlDevice, crtc_h: crtc::Handle, x: i32, y: i32) {
        let _ = drm.move_cursor(crtc_h, (x, y));
    }

    pub fn hide_on_crtc(&self, drm: &impl ControlDevice, crtc_h: crtc::Handle) {
        let _ = drm.set_cursor(crtc_h, Option::<&DumbBuffer>::None);
    }
}

// ── Pixel sources ─────────────────────────────────────────────────────────────

/// Try to load the left_ptr image from the running xcursor theme.
/// Returns ARGB8888 pixels at CURSOR_W×CURSOR_H and the hotspot.
fn load_xcursor_pixels() -> Option<(Vec<u32>, u32, u32)> {
    // xcursor-rs or a similar crate would go here.
    // For now return None so we always use the generated arrow.
    None
}

/// Generate a simple white arrow cursor with a black outline, ARGB8888.
fn make_arrow_pixels() -> Vec<u32> {
    let w = CURSOR_W as usize;
    let h = CURSOR_H as usize;
    let mut buf = vec![0u32; w * h]; // fully transparent

    // Draw a simple 12×20 arrow in the top-left corner.
    // Outer outline (black), inner fill (white).
    for row in 0..20usize {
        let width_at_row = if row < 12 { row + 1 } else { 20 - row };
        for col in 0..width_at_row {
            // skip the very last pixel row to make a point
            if row == 19 && col > 0 {
                break;
            }
            let is_edge = row == 0
                || col == 0
                || col + 1 == width_at_row
                || (row >= 12 && (col == 0 || col + 1 == width_at_row));
            let argb = if is_edge {
                0xFF_00_00_00
            } else {
                0xFF_FF_FF_FF
            };
            if row < h && col < w {
                buf[row * w + col] = argb;
            }
        }
    }
    buf
}
