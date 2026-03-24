// src/render/chrome.rs — Window chrome: shadows, borders, and content blitting.
//
// Layout contract (critical):
//
//   win.rect  == the CONTENT rect the client rendered into.
//               reflow() assigns this; it is what xdg_toplevel.configure sends.
//
//   border ring == drawn OUTSIDE the content rect, expanding by border_width.
//                  The quad sent to the border shader starts at
//                  (content.x - bw, content.y - bw) and is
//                  (content.w + 2*bw) × (content.h + 2*bw).
//
//   shadow quad  == drawn OUTSIDE the border ring. It expands the border quad
//                  further by shadow_offset on all sides.
//                  Anchored to the *content* rect and expanded once here —
//                  do NOT pre-expand before calling draw_shadow.
//
// For tiled windows:
//   - corner_radius = 0  (fills tile pixel-perfectly, no gap between tiles)
//   - shadow suppressed (caller skips draw_shadow for non-floating windows)

use super::programs::{GlProgram, GlTexture, QuadVao};
use crate::wm::Rect;

// ── Chrome config ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChromeConfig {
    pub corner_radius: f32,
    pub shadow_spread: f32,
    pub shadow_offset: f32,
    /// Border ring width in pixels. Set to 0.0 to disable.
    pub border_width: f32,
    pub shadow_focused: [f32; 4],
    pub shadow_unfocused: [f32; 4],
    pub border_active_a: [f32; 4],
    pub border_active_b: [f32; 4],
    pub border_inactive: [f32; 4],
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            corner_radius: 10.0,
            shadow_spread: 28.0,
            shadow_offset: 32.0,
            border_width: 2.0,
            shadow_focused: [0.0, 0.0, 0.0, 0.72],
            shadow_unfocused: [0.0, 0.0, 0.0, 0.38],
            border_active_a: [0.706, 0.745, 0.996, 1.0],
            border_active_b: [0.804, 0.651, 0.969, 1.0],
            border_inactive: [0.239, 0.247, 0.322, 0.7],
        }
    }
}

// ── ChromeRenderer ────────────────────────────────────────────────────────────

pub struct ChromeRenderer<'r> {
    pub cfg: &'r ChromeConfig,
    pub prog_tex: &'r GlProgram,
    pub prog_solid: &'r GlProgram,
    pub prog_border: &'r GlProgram,
    pub prog_shadow: &'r GlProgram,
    pub quad_vao: &'r QuadVao,
}

impl<'r> ChromeRenderer<'r> {
    // ── Public entry points ───────────────────────────────────────────────────

    /// Draw the drop-shadow for one floating/fullscreen window.
    ///
    /// `content_rect` is win.rect (the WM content rect). This function
    /// expands it internally to produce the shadow quad — do NOT pre-expand.
    pub fn draw_shadow(&self, proj: &[f32; 9], content_rect: Rect, focused: bool, opacity: f32) {
        let so = self.cfg.shadow_offset;
        let sr = self.cfg.shadow_spread;
        let cr = self.cfg.corner_radius;
        let bw = self.cfg.border_width;

        // Shadow quad: expand content rect by (border_width + shadow_offset) on
        // all sides, with a slight extra amount on the bottom for realism.
        let sx = content_rect.x as f32 - bw - so;
        let sy = content_rect.y as f32 - bw - so * 0.5;
        let sw = content_rect.w as f32 + (bw + so) * 2.0;
        let sh = content_rect.h as f32 + (bw + so) * 2.5;

        let base = if focused {
            self.cfg.shadow_focused
        } else {
            self.cfg.shadow_unfocused
        };
        let col = [base[0], base[1], base[2], base[3] * opacity];

        // Shadow uses premultiplied-alpha blending (ONE, ONE_MINUS_SRC_ALPHA)
        // which is already set globally. The SHADOW_FRAG shader emits
        // premul alpha so we don't need to switch blend modes here.
        unsafe {
            self.prog_shadow.bind();
            gl::UniformMatrix3fv(self.prog_shadow.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(self.prog_shadow.loc("u_rect"), sx, sy, sw, sh);
            gl::Uniform4f(
                self.prog_shadow.loc("u_color"),
                col[0],
                col[1],
                col[2],
                col[3],
            );
            gl::Uniform2f(self.prog_shadow.loc("u_size"), sw, sh);
            // Shadow corner radius follows the chrome corner radius + border.
            gl::Uniform1f(self.prog_shadow.loc("u_radius"), cr + bw + so * 0.25);
            gl::Uniform1f(self.prog_shadow.loc("u_blur"), sr);
            self.quad_vao.draw();
        }
    }

    /// Draw the border ring then blit the window texture.
    ///
    /// `content_rect` is win.rect — the rect the client rendered into.
    /// `is_floating`  controls rounded corners (tiled → radius 0).
    pub fn draw_window(
        &self,
        proj: &[f32; 9],
        content_rect: Rect,
        tex: &GlTexture,
        focused: bool,
        opacity: f32,
        is_floating: bool,
    ) {
        let radius = if is_floating {
            self.cfg.corner_radius
        } else {
            0.0
        };
        let bw = self.cfg.border_width;

        if bw > 0.0 {
            self.draw_border_inner(proj, content_rect, focused, opacity, radius, bw);
        }

        // Blit content filling exactly content_rect.
        self.blit_rounded(
            proj,
            content_rect.x as f32,
            content_rect.y as f32,
            content_rect.w as f32,
            content_rect.h as f32,
            radius,
            tex,
            opacity,
        );
    }

    /// Draw only the border ring (no texture blit). Used when no texture is
    /// available yet so the tile outline is still visible.
    pub fn draw_border_only(
        &self,
        proj: &[f32; 9],
        content_rect: Rect,
        focused: bool,
        opacity: f32,
        is_floating: bool,
    ) {
        let bw = self.cfg.border_width;
        if bw <= 0.0 {
            return;
        }
        let radius = if is_floating {
            self.cfg.corner_radius
        } else {
            0.0
        };
        self.draw_border_inner(proj, content_rect, focused, opacity, radius, bw);
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn draw_border_inner(
        &self,
        proj: &[f32; 9],
        content_rect: Rect,
        focused: bool,
        opacity: f32,
        radius: f32,
        bw: f32,
    ) {
        // Border quad: expand content rect by bw on all sides.
        let bx = content_rect.x as f32 - bw;
        let by = content_rect.y as f32 - bw;
        let bw2 = content_rect.w as f32 + bw * 2.0;
        let bh2 = content_rect.h as f32 + bw * 2.0;
        let br = radius + bw; // outer corner radius of the ring

        let (ca, cb) = if focused {
            (self.cfg.border_active_a, self.cfg.border_active_b)
        } else {
            (self.cfg.border_inactive, self.cfg.border_inactive)
        };
        let mut ca = ca;
        ca[3] *= opacity;
        let mut cb = cb;
        cb[3] *= opacity;

        unsafe {
            self.prog_border.bind();
            gl::UniformMatrix3fv(self.prog_border.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(self.prog_border.loc("u_rect"), bx, by, bw2, bh2);
            gl::Uniform2f(self.prog_border.loc("u_size"), bw2, bh2);
            gl::Uniform1f(self.prog_border.loc("u_radius"), br);
            gl::Uniform1f(self.prog_border.loc("u_thickness"), bw);
            gl::Uniform4f(self.prog_border.loc("u_col_a"), ca[0], ca[1], ca[2], ca[3]);
            gl::Uniform4f(self.prog_border.loc("u_col_b"), cb[0], cb[1], cb[2], cb[3]);
            gl::Uniform1f(
                self.prog_border.loc("u_focused"),
                if focused { 1.0 } else { 0.0 },
            );
            self.quad_vao.draw();
        }
    }

    pub fn blit(
        &self,
        proj: &[f32; 9],
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        tex: &GlTexture,
        alpha: f32,
    ) {
        self.blit_rounded(proj, x, y, w, h, 0.0, tex, alpha);
    }

    pub fn fill_rounded(
        &self,
        proj: &[f32; 9],
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        c: [f32; 4],
    ) {
        unsafe {
            self.prog_solid.bind();
            gl::UniformMatrix3fv(self.prog_solid.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(self.prog_solid.loc("u_rect"), x, y, w, h);
            gl::Uniform4f(self.prog_solid.loc("u_color"), c[0], c[1], c[2], c[3]);
            gl::Uniform2f(self.prog_solid.loc("u_size"), w, h);
            gl::Uniform1f(self.prog_solid.loc("u_radius"), r);
            self.quad_vao.draw();
        }
    }

    fn blit_rounded(
        &self,
        proj: &[f32; 9],
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        tex: &GlTexture,
        alpha: f32,
    ) {
        unsafe {
            self.prog_tex.bind();
            gl::UniformMatrix3fv(self.prog_tex.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(self.prog_tex.loc("u_rect"), x, y, w, h);
            gl::Uniform1i(self.prog_tex.loc("u_tex"), 0);
            gl::Uniform1f(self.prog_tex.loc("u_alpha"), alpha);
            gl::Uniform2f(self.prog_tex.loc("u_size"), w, h);
            gl::Uniform1f(self.prog_tex.loc("u_radius"), r);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, tex.id);
            self.quad_vao.draw();
        }
    }
}
