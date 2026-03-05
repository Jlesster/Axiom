// cursor.rs — Hardware cursor plane via DRM + XCursor theme loading.
//
// Strategy:
//   1. On first pointer motion, load the cursor theme (XCursor or fallback bitmap).
//   2. Upload the cursor bitmap to a GBM-backed DRM cursor buffer (64×64 ARGB8888).
//   3. Call DrmDevice::set_cursor / move_cursor each frame — this goes through the
//      dedicated hardware cursor plane and costs zero GPU fill-rate.
//   4. Fall back to a software rendered arrow (SolidColorRenderElement) if the
//      DRM cursor plane is not available or the upload fails.
//
// The cursor state lives in `CursorManager`, stored in `Trixie`.
// `render.rs` calls `cursor_manager.move_to(x, y)` after pointer motion.
// `backend.rs` calls `cursor_manager.upload_to_output(drm, crtc)` once per output.

use std::path::PathBuf;

use smithay::{
    backend::{
        allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        drm::{DrmDevice, DrmDeviceFd},
    },
    input::pointer::CursorImageStatus,
    utils::Point,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// DRM cursor buffers must be a power-of-two size. 64×64 is universally supported.
pub const CURSOR_SIZE: u32 = 64;

// ── CursorTheme ───────────────────────────────────────────────────────────────

/// A loaded, ready-to-upload ARGB8888 bitmap for one cursor image.
#[derive(Clone)]
pub struct CursorBitmap {
    /// Raw ARGB8888 pixel data, row-major, exactly CURSOR_SIZE×CURSOR_SIZE × 4 bytes.
    pub data: Vec<u8>,
    /// Hotspot within the 64×64 bitmap.
    pub hotspot: (u32, u32),
}

impl CursorBitmap {
    /// Build the fallback arrow cursor programmatically — clean, minimal, themes
    /// with the system accent colour passed in as ARGB.
    pub fn builtin_arrow(fg: u32, outline: u32) -> Self {
        let mut data = vec![0u8; (CURSOR_SIZE * CURSOR_SIZE * 4) as usize];

        // Simple left-pointing arrow: 20px tall, 12px wide at base.
        // Drawn as a filled triangle with a 1-px outline.
        let arrow: &[(u32, u32, u32, u32)] = &[
            // (row_start, row_end, col_start, col_end)  — filled body
            (0, 1, 0, 1),
        ];
        let _ = arrow; // suppress unused warning — we draw manually below

        let size = CURSOR_SIZE;
        for row in 0..size {
            for col in 0..size {
                let r = row as f32;
                let c = col as f32;
                // The arrow occupies rows 0..22, cols 0..14 (upper-left quadrant).
                // Left edge: col == 0
                // Hypotenuse: col <= row * 0.55
                // Bottom: row == 22 - col
                let max_row = 22.0f32;
                let is_inside = r < max_row && c < r * 0.62 + 1.0 && c < (max_row - r) + 1.0;
                let is_outline = {
                    let nr = r + 1.0;
                    let nc = c + 1.0;
                    let inner_row = nr < max_row - 1.0;
                    let inner_hyp = nc < (r - 1.0) * 0.62 + 0.5;
                    let inner_bot = nr < (max_row - 1.0 - nc) + 1.0;
                    is_inside && !(inner_row && inner_hyp && inner_bot && c > 1.0)
                };

                if is_inside {
                    let color = if is_outline { outline } else { fg };
                    let idx = ((row * size + col) * 4) as usize;
                    // ARGB8888 in little-endian (B G R A in memory order for most DRM drivers)
                    data[idx] = (color & 0xff) as u8; // B
                    data[idx + 1] = ((color >> 8) & 0xff) as u8; // G
                    data[idx + 2] = ((color >> 16) & 0xff) as u8; // R
                    data[idx + 3] = ((color >> 24) & 0xff) as u8; // A
                }
            }
        }

        Self {
            data,
            hotspot: (0, 0),
        }
    }

    /// Try to load a cursor from an XCursor theme directory.
    /// Returns None if xcursor-rs / the theme is unavailable.
    pub fn from_xcursor_theme(theme: &str, name: &str, size: u32) -> Option<Self> {
        // Search standard XCursor paths.
        let search_dirs: Vec<PathBuf> = {
            let mut dirs = Vec::new();
            if let Ok(home) = std::env::var("HOME") {
                dirs.push(PathBuf::from(&home).join(".local/share/icons").join(theme));
                dirs.push(PathBuf::from(&home).join(".icons").join(theme));
            }
            dirs.push(PathBuf::from("/usr/share/icons").join(theme));
            dirs.push(PathBuf::from("/usr/share/pixmaps"));
            dirs
        };

        for dir in &search_dirs {
            let cursor_file = dir.join("cursors").join(name);
            if cursor_file.exists() {
                if let Some(bm) = load_xcursor_file(&cursor_file, size) {
                    return Some(bm);
                }
            }
        }
        None
    }
}

/// Parse a minimal XCursor file and extract the first frame at closest size.
/// XCursor format: 4-byte magic + chunks. We only need ARGB image chunks (type 0xFFFD0002).
fn load_xcursor_file(path: &std::path::Path, target_size: u32) -> Option<CursorBitmap> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 16 {
        return None;
    }

    // Magic: "Xcur"
    if &data[0..4] != b"Xcur" {
        return None;
    }

    let header_size = u32::from_le_bytes(data[4..8].try_into().ok()?) as usize;
    let _version = u32::from_le_bytes(data[8..12].try_into().ok()?);
    let ntoc = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;

    if data.len() < header_size + ntoc * 12 {
        return None;
    }

    const XCURSOR_IMAGE_TYPE: u32 = 0xFFFD_0002;

    // Table of contents: 12 bytes each (type, subtype, position).
    let mut best_offset: Option<usize> = None;
    let mut best_size_diff = u32::MAX;

    for i in 0..ntoc {
        let toc_base = header_size + i * 12;
        if toc_base + 12 > data.len() {
            break;
        }
        let chunk_type = u32::from_le_bytes(data[toc_base..toc_base + 4].try_into().ok()?);
        let subtype = u32::from_le_bytes(data[toc_base + 4..toc_base + 8].try_into().ok()?);
        let position =
            u32::from_le_bytes(data[toc_base + 8..toc_base + 12].try_into().ok()?) as usize;

        if chunk_type != XCURSOR_IMAGE_TYPE {
            continue;
        }

        // subtype is the nominal size of this image.
        let diff = (subtype as i64 - target_size as i64).unsigned_abs() as u32;
        if diff < best_size_diff {
            best_size_diff = diff;
            best_offset = Some(position);
        }
    }

    let offset = best_offset?;
    // Image chunk header: chunk_header_size(4) + type(4) + subtype(4) + version(4)
    //                     + width(4) + height(4) + xhot(4) + yhot(4) + delay(4)
    // Then width*height ARGB u32 pixels.
    if offset + 36 > data.len() {
        return None;
    }

    let _chunk_header_size = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
    // skip type, subtype, version
    let width = u32::from_le_bytes(data[offset + 16..offset + 20].try_into().ok()?);
    let height = u32::from_le_bytes(data[offset + 20..offset + 24].try_into().ok()?);
    let xhot = u32::from_le_bytes(data[offset + 24..offset + 28].try_into().ok()?);
    let yhot = u32::from_le_bytes(data[offset + 28..offset + 32].try_into().ok()?);
    // skip delay
    let pixels_offset = offset + 36;
    let pixel_count = (width * height) as usize;

    if pixels_offset + pixel_count * 4 > data.len() {
        return None;
    }

    // Blit into a 64×64 ARGB8888 buffer (DRM cursor plane requirement).
    let mut out = vec![0u8; (CURSOR_SIZE * CURSOR_SIZE * 4) as usize];
    let blit_w = width.min(CURSOR_SIZE);
    let blit_h = height.min(CURSOR_SIZE);

    for row in 0..blit_h {
        for col in 0..blit_w {
            let src_idx = (row * width + col) as usize;
            let argb = u32::from_le_bytes(
                data[pixels_offset + src_idx * 4..pixels_offset + src_idx * 4 + 4]
                    .try_into()
                    .ok()?,
            );
            let dst_idx = ((row * CURSOR_SIZE + col) * 4) as usize;
            // XCursor stores ARGB; DRM expects ARGB8888 (same layout on LE).
            out[dst_idx] = (argb & 0xff) as u8; // B
            out[dst_idx + 1] = ((argb >> 8) & 0xff) as u8; // G
            out[dst_idx + 2] = ((argb >> 16) & 0xff) as u8; // R
            out[dst_idx + 3] = ((argb >> 24) & 0xff) as u8; // A
        }
    }

    Some(CursorBitmap {
        data: out,
        hotspot: (xhot.min(CURSOR_SIZE - 1), yhot.min(CURSOR_SIZE - 1)),
    })
}

// ── CursorManager ─────────────────────────────────────────────────────────────

/// Tracks the current cursor image and position, and manages the DRM cursor plane.
pub struct CursorManager {
    pub bitmap: CursorBitmap,
    pub pos: Point<f64, smithay::utils::Logical>,
    /// Whether the hardware cursor plane was successfully set up.
    pub hw_cursor_ok: bool,
    /// Theme name to load from (falls back to "default").
    pub theme_name: String,
    /// The GBM buffer object backing the hardware cursor plane.
    /// Must be kept alive for as long as the cursor is displayed — the DRM
    /// driver holds a reference to the underlying buffer, so dropping the BO
    /// here would leave the cursor plane pointing at freed memory.
    _hw_cursor_bo: Option<smithay::reexports::gbm::BufferObject<()>>,
}

impl Default for CursorManager {
    fn default() -> Self {
        Self {
            bitmap: CursorBitmap::builtin_arrow(0xFFCDD2E6, 0xFF1E1E2E),
            pos: Point::from((0.0, 0.0)),
            hw_cursor_ok: false,
            theme_name: "default".into(),
            _hw_cursor_bo: None,
        }
    }
}

impl CursorManager {
    /// Load the cursor theme. Called once after the GL context is ready.
    /// Reads $XCURSOR_THEME and $XCURSOR_SIZE from the environment first,
    /// then falls back to the `theme_name` field and CURSOR_SIZE.
    pub fn load_theme(&mut self) {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| self.theme_name.clone());
        let size: u32 = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(CURSOR_SIZE);

        // Try several common cursor names in order of preference.
        let names = ["left_ptr", "arrow", "default"];
        for name in &names {
            if let Some(bm) = CursorBitmap::from_xcursor_theme(&theme, name, size) {
                tracing::info!("cursor: loaded '{name}' from theme '{theme}' at {size}px");
                self.bitmap = bm;
                return;
            }
        }

        tracing::info!("cursor: no XCursor theme found, using built-in arrow");
        // Keep the default built-in bitmap.
    }

    /// Upload the current cursor bitmap to the DRM cursor plane for `crtc`.
    /// Must be called after the EGL context is current and after `load_theme`.
    ///
    /// Returns `true` on success.
    pub fn upload_to_drm(
        &mut self,
        drm: &DrmDevice,
        crtc: smithay::reexports::drm::control::crtc::Handle,
        gbm: &GbmDevice<DrmDeviceFd>,
    ) -> bool {
        use smithay::backend::allocator::gbm::GbmBufferFlags;
        use smithay::reexports::drm::control::Device;

        // Allocate a GBM BO for the cursor (CURSOR | WRITE flags).
        let bo_result = gbm.create_buffer_object::<()>(
            CURSOR_SIZE,
            CURSOR_SIZE,
            smithay::backend::allocator::Fourcc::Argb8888,
            GbmBufferFlags::CURSOR | GbmBufferFlags::WRITE,
        );

        let mut bo = match bo_result {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("cursor: GBM BO alloc failed: {e}");
                self.hw_cursor_ok = false;
                return false;
            }
        };

        // Write pixel data row-by-row.
        if let Err(e) = bo.write(&self.bitmap.data) {
            tracing::warn!("cursor: GBM BO write failed: {e}");
            self.hw_cursor_ok = false;
            return false;
        }

        // Set the cursor on this CRTC via the DRM Device trait.
        match drm.set_cursor(crtc, Some(&bo)) {
            Ok(()) => {
                // Keep the BO alive — the DRM plane references it until replaced.
                self._hw_cursor_bo = Some(bo);
                self.hw_cursor_ok = true;
                // Set initial position so the cursor appears immediately.
                let cx = (self.pos.x as i32 - self.bitmap.hotspot.0 as i32).max(0);
                let cy = (self.pos.y as i32 - self.bitmap.hotspot.1 as i32).max(0);
                if let Err(e) = drm.move_cursor(crtc, (cx, cy).into()) {
                    tracing::trace!("cursor initial move_cursor: {e}");
                }
                tracing::debug!("cursor: hardware cursor plane active on {crtc:?}");
                true
            }
            Err(e) => {
                tracing::warn!("cursor: set_cursor failed ({e}), will use software fallback");
                self._hw_cursor_bo = None;
                self.hw_cursor_ok = false;
                false
            }
        }
    }

    /// Update the hardware cursor position. Call this every time the pointer moves.
    pub fn move_cursor(
        &mut self,
        drm: &DrmDevice,
        crtc: smithay::reexports::drm::control::crtc::Handle,
        x: i32,
        y: i32,
    ) {
        use smithay::reexports::drm::control::Device;
        if !self.hw_cursor_ok {
            return;
        }
        // Adjust for hotspot.
        let cx = (x - self.bitmap.hotspot.0 as i32).max(0);
        let cy = (y - self.bitmap.hotspot.1 as i32).max(0);

        if let Err(e) = drm.move_cursor(crtc, (cx, cy).into()) {
            tracing::trace!("cursor move_cursor: {e}");
        }
    }

    /// Hide the hardware cursor (e.g. on session pause).
    pub fn hide(&self, drm: &DrmDevice, crtc: smithay::reexports::drm::control::crtc::Handle) {
        use smithay::reexports::drm::control::Device;
        // Pass None with the correct concrete type the trait expects.
        let _ = drm.set_cursor(crtc, None::<&smithay::reexports::gbm::BufferObject<()>>);
    }

    /// Build a software-fallback SolidColorRenderElement for when the hardware
    /// cursor plane is unavailable. Renders a small high-contrast square with a
    /// dark outline so it shows on both light and dark backgrounds.
    pub fn software_element(
        &self,
        scale: smithay::utils::Scale<f64>,
    ) -> smithay::backend::renderer::element::solid::SolidColorRenderElement {
        use smithay::{
            backend::renderer::element::{solid::SolidColorBuffer, Kind},
            utils::Physical,
        };

        // 10×10 white cursor square with 1px dark border — visible on any bg.
        let size = 10i32;
        let buf = SolidColorBuffer::new((size, size), [0.95, 0.95, 0.95, 1.0]);
        let loc = smithay::utils::Point::<i32, Physical>::from((
            (self.pos.x as i32 - self.bitmap.hotspot.0 as i32).max(0),
            (self.pos.y as i32 - self.bitmap.hotspot.1 as i32).max(0),
        ));
        smithay::backend::renderer::element::solid::SolidColorRenderElement::from_buffer(
            &buf,
            loc,
            scale,
            1.0,
            Kind::Cursor,
        )
    }
}
