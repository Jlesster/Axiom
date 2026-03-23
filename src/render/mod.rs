// src/render/mod.rs — OpenGL render pipeline.

pub mod bar;
pub mod cursor;
pub mod font;
pub mod glyph_vao;
pub mod programs;

use anyhow::Result;
use std::collections::HashMap;

pub use cursor::HwCursor;
pub use programs::{GlProgram, GlTexture, QuadVao};

use crate::{
    backend::{egl::EglContext, OutputSurface},
    input::InputState,
    proto::layer_shell::{Layer, LayerSurfaceRef},
    state::OutputState,
    wm::{anim::AnimSet, Rect, WindowId, WmState},
};

// ── Shaders ───────────────────────────────────────────────────────────────────

const QUAD_VERT: &str = r#"
#version 330 core
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
out vec2 v_uv;
out vec2 v_pos;      // pixel position within the rect
uniform mat3 u_proj;
uniform vec4 u_rect; // x, y, w, h  (pixels)
void main() {
    v_uv  = a_uv;
    v_pos = a_pos * u_rect.zw;          // 0..w, 0..h
    vec2 world  = a_pos * u_rect.zw + u_rect.xy;
    vec3 ndc    = u_proj * vec3(world, 1.0);
    gl_Position = vec4(ndc.xy, 0.0, 1.0);
}
"#;

// Textured quad with rounded corners and per-pixel alpha.
const TEX_FRAG: &str = r#"
#version 330 core
in  vec2 v_uv;
in  vec2 v_pos;
out vec4 frag;
uniform sampler2D u_tex;
uniform float     u_alpha;
uniform vec2      u_size;   // rect pixel size
uniform float     u_radius; // corner radius in pixels

float rounded_alpha(vec2 pos, vec2 sz, float r) {
    vec2 q = abs(pos - sz * 0.5) - sz * 0.5 + r;
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
    return clamp(-d, 0.0, 1.0);           // anti-aliased edge
}

void main() {
    float mask = rounded_alpha(v_pos, u_size, u_radius);
    frag = texture(u_tex, v_uv) * vec4(1.0, 1.0, 1.0, u_alpha * mask);
}
"#;

// Solid colour quad with rounded corners.
const SOLID_FRAG: &str = r#"
#version 330 core
in  vec2 v_pos;
out vec4 frag;
uniform vec4  u_color;
uniform vec2  u_size;
uniform float u_radius;

float rounded_alpha(vec2 pos, vec2 sz, float r) {
    vec2 q = abs(pos - sz * 0.5) - sz * 0.5 + r;
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
    return clamp(-d, 0.0, 1.0);
}

void main() {
    float mask = rounded_alpha(v_pos, u_size, u_radius);
    frag = u_color * vec4(1.0, 1.0, 1.0, mask);
}
"#;

// Drop-shadow — a blurred dark rectangle offset behind the window.
// We approximate a gaussian shadow with a smooth falloff.
const SHADOW_FRAG: &str = r#"
#version 330 core
in  vec2 v_pos;
out vec4 frag;
uniform vec4  u_color;   // shadow colour (pre-multiplied alpha target)
uniform vec2  u_size;    // shadow rect size
uniform float u_radius;  // corner radius
uniform float u_blur;    // blur spread in pixels

float rounded_sdf(vec2 pos, vec2 sz, float r) {
    vec2 q = abs(pos - sz * 0.5) - sz * 0.5 + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    float d    = rounded_sdf(v_pos, u_size, u_radius);
    float blur = max(u_blur, 0.001);
    float a    = smoothstep(blur, 0.0, d) * u_color.a;
    frag = vec4(u_color.rgb * a, a);   // pre-multiplied
}
"#;

const GLYPH_GRAY_FRAG: &str = r#"
#version 330 core
in  vec2 v_uv;
in  vec2 v_pos;
out vec4 frag;
uniform sampler2D u_tex;
uniform vec4 u_color;
void main() {
    float cov = texture(u_tex, v_uv).r;
    frag = vec4(u_color.rgb, u_color.a * cov);
}
"#;

const GLYPH_LCD_FRAG: &str = r#"
#version 330 core
in  vec2 v_uv;
in  vec2 v_pos;
out vec4 frag;
uniform sampler2D u_tex;
uniform vec4 u_color;
uniform vec4 u_bg;
void main() {
    vec3 cov  = texture(u_tex, v_uv).rgb;
    vec3 rgb  = u_color.rgb * cov + u_bg.rgb * (1.0 - cov);
    float a   = max(cov.r, max(cov.g, cov.b)) * u_color.a;
    frag = vec4(rgb * a, a);
}
"#;

// ── Chrome config (tweak here to taste) ──────────────────────────────────────

pub struct ChromeConfig {
    /// Window corner radius in pixels.
    pub corner_radius: f32,
    /// Drop-shadow spread (blur radius) in pixels.
    pub shadow_spread: f32,
    /// Shadow offset from the window edge (extra padding each side).
    pub shadow_offset: f32,
    /// Focused border width (the ring drawn outside the rounded window).
    pub border_width: f32,
    /// Focused shadow colour (RGBA).
    pub shadow_focused: [f32; 4],
    /// Unfocused shadow colour (RGBA).
    pub shadow_unfocused: [f32; 4],
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
        }
    }
}

// ── RenderState ───────────────────────────────────────────────────────────────

pub struct RenderState {
    pub prog_tex: GlProgram,
    pub prog_solid: GlProgram,
    pub prog_shadow: GlProgram,
    pub prog_glyph_gray: GlProgram,
    pub prog_glyph_lcd: GlProgram,
    pub quad_vao: QuadVao,
    pub glyph_vao: glyph_vao::GlyphVao,
    pub textures: HashMap<WindowId, GlTexture>,
    pub layer_textures: HashMap<u32, GlTexture>,
    pub hw_cursor: Option<HwCursor>,
    pub font: Option<font::FontAtlas>,
    pub bar: bar::BarState,
    pub chrome: ChromeConfig,
}

impl RenderState {
    pub fn new() -> Result<Self> {
        let prog_tex = GlProgram::compile(QUAD_VERT, TEX_FRAG)?;
        let prog_solid = GlProgram::compile(QUAD_VERT, SOLID_FRAG)?;
        let prog_shadow = GlProgram::compile(QUAD_VERT, SHADOW_FRAG)?;
        let prog_glyph_gray = GlProgram::compile(QUAD_VERT, GLYPH_GRAY_FRAG)?;
        let prog_glyph_lcd = GlProgram::compile(QUAD_VERT, GLYPH_LCD_FRAG)?;
        let quad_vao = QuadVao::new();
        let glyph_vao = glyph_vao::GlyphVao::new();

        unsafe {
            gl::Enable(gl::BLEND);
            // Standard straight-alpha blending — used by textures and glyphs.
            // The shadow draw call temporarily switches to pre-multiplied and
            // restores this afterwards.
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            gl::Disable(gl::DEPTH_TEST);
            gl::Disable(gl::STENCIL_TEST);
        }

        let font = find_font().and_then(|p| {
            font::FontAtlas::new(&p)
                .map_err(|e| {
                    tracing::warn!("font {p}: {e}");
                    e
                })
                .ok()
        });
        if font.is_none() {
            tracing::warn!("no font — bar text disabled");
        }

        Ok(Self {
            prog_tex,
            prog_solid,
            prog_shadow,
            prog_glyph_gray,
            prog_glyph_lcd,
            quad_vao,
            glyph_vao,
            textures: HashMap::new(),
            layer_textures: HashMap::new(),
            hw_cursor: None,
            font,
            bar: bar::BarState::new(bar::BarConfig::default()),
            chrome: ChromeConfig::default(),
        })
    }

    // ── Per-frame render ──────────────────────────────────────────────────────

    pub fn render_output(
        &mut self,
        wm: &WmState,
        anim: &AnimSet,
        input: &InputState,
        outputs: &[OutputState],
        layer_surfaces: &[(
            LayerSurfaceRef,
            wayland_server::protocol::wl_surface::WlSurface,
        )],
        _surf: &OutputSurface,
        out_idx: usize,
    ) {
        let out = &outputs[out_idx];
        let (w, h) = (out.width, out.height);

        unsafe {
            gl::Viewport(0, 0, w as i32, h as i32);
            gl::ClearColor(bar::col::BASE[0], bar::col::BASE[1], bar::col::BASE[2], 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        let proj = ortho(w as f32, h as f32);

        self.draw_layers(&proj, layer_surfaces, w as i32, h as i32, Layer::Background);
        self.draw_layers(&proj, layer_surfaces, w as i32, h as i32, Layer::Bottom);

        let aws = wm
            .monitors
            .iter()
            .find(|m| m.output_id == out.wl_id)
            .map(|m| m.active_ws)
            .unwrap_or_else(|| wm.active_ws());

        let focused_id = wm.focused_window();

        let (tiled, floating): (Vec<WindowId>, Vec<WindowId>) = wm.workspaces[aws]
            .windows
            .iter()
            .copied()
            .partition(|&id| wm.windows.get(&id).map(|w| !w.floating).unwrap_or(true));

        // Draw in z-order: tiled first, then floating, focused window last.
        let mut draw_order: Vec<WindowId> = tiled.iter().chain(floating.iter()).copied().collect();
        // Ensure focused window is painted on top (in its group).
        if let Some(fid) = focused_id {
            if let Some(pos) = draw_order.iter().position(|&id| id == fid) {
                draw_order.remove(pos);
                draw_order.push(fid);
            }
        }

        for &id in &draw_order {
            let Some(win) = wm.windows.get(&id) else {
                continue;
            };
            let rect = anim.get_rect(id, win.rect);
            let focused = focused_id == Some(id);
            // Shadow pass.
            self.draw_shadow(&proj, rect, focused);
            // Window pass.
            self.draw_window(wm, &proj, id, rect, focused);
        }

        self.draw_layers(&proj, layer_surfaces, w as i32, h as i32, Layer::Top);
        self.draw_layers(&proj, layer_surfaces, w as i32, h as i32, Layer::Overlay);

        if self.bar.cfg.height > 0 {
            self.bar.tick();
            let mon_idx = wm
                .monitors
                .iter()
                .position(|m| m.output_id == out.wl_id)
                .unwrap_or(0);
            self.draw_bar(&proj, wm, w as f32, mon_idx);
        }

        if self.hw_cursor.is_none() && !input.hw_cursor_active {
            let (cx, cy) = (input.cursor_pos.0 as f32, input.cursor_pos.1 as f32);
            // Software cursor: small rounded dot
            self.fill_rounded(
                &proj,
                cx - 4.0,
                cy - 4.0,
                8.0,
                12.0,
                3.0,
                [1.0, 1.0, 1.0, 0.9],
            );
        }
    }

    // ── Shadow ────────────────────────────────────────────────────────────────

    fn draw_shadow(&self, proj: &[f32; 9], rect: Rect, focused: bool) {
        let so = self.chrome.shadow_offset;
        let sr = self.chrome.shadow_spread;
        let cr = self.chrome.corner_radius;
        let sx = rect.x as f32 - so;
        let sy = rect.y as f32 - so * 0.5;
        let sw = rect.w as f32 + so * 2.0;
        let sh = rect.h as f32 + so * 2.5;
        let col = if focused {
            self.chrome.shadow_focused
        } else {
            self.chrome.shadow_unfocused
        };

        unsafe {
            // Shadow shader outputs pre-multiplied alpha — switch blend mode
            // just for this draw call, then restore straight-alpha.
            gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

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
            gl::Uniform1f(self.prog_shadow.loc("u_radius"), cr + so * 0.5);
            gl::Uniform1f(self.prog_shadow.loc("u_blur"), sr);
            self.quad_vao.draw();

            // Restore standard straight-alpha for everything else.
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        }
    }

    // ── Bar ───────────────────────────────────────────────────────────────────

    fn draw_bar(&mut self, proj: &[f32; 9], wm: &WmState, out_w: f32, mon_idx: usize) {
        let Some(font) = self.font.as_mut() else {
            return;
        };

        let ps = &self.prog_solid as *const GlProgram;
        let pg = &self.prog_glyph_gray as *const GlProgram;
        let pl = &self.prog_glyph_lcd as *const GlProgram;
        let qv = &self.quad_vao as *const QuadVao;
        let gv = &self.glyph_vao as *const glyph_vao::GlyphVao;
        let bg = bar::col::MANTLE;

        let fill_fn = |proj: [f32; 9], x: f32, y: f32, w: f32, h: f32, c: [f32; 4]| unsafe {
            let p = &*ps;
            p.bind();
            gl::UniformMatrix3fv(p.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(p.loc("u_rect"), x, y, w, h);
            gl::Uniform4f(p.loc("u_color"), c[0], c[1], c[2], c[3]);
            gl::Uniform2f(p.loc("u_size"), w, h);
            gl::Uniform1f(p.loc("u_radius"), 0.0); // bar uses no rounding
            (*qv).draw();
        };

        let glyph_fn = |proj: [f32; 9],
                        x: f32,
                        y: f32,
                        w: f32,
                        h: f32,
                        tex: u32,
                        c: [f32; 4],
                        uv: [f32; 4],
                        lcd: bool| {
            unsafe {
                let p = if lcd { &*pl } else { &*pg };
                p.bind();
                gl::UniformMatrix3fv(p.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
                gl::Uniform4f(p.loc("u_rect"), x, y, w, h);
                gl::Uniform1i(p.loc("u_tex"), 0);
                gl::Uniform4f(p.loc("u_color"), c[0], c[1], c[2], c[3]);
                gl::Uniform4f(p.loc("u_bg"), bg[0], bg[1], bg[2], bg[3]);
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, tex);
                (*gv).draw(uv);
            }
        };

        let mut ctx = bar::DrawCtx {
            proj,
            font,
            fill: &fill_fn,
            glyph: &glyph_fn,
        };
        self.bar.draw(&mut ctx, wm, out_w, mon_idx);
    }

    // ── Window ────────────────────────────────────────────────────────────────

    fn draw_window(&self, wm: &WmState, proj: &[f32; 9], id: WindowId, rect: Rect, focused: bool) {
        let Some(tex) = self.textures.get(&id) else {
            return;
        };

        let cr = self.chrome.corner_radius;
        let bw = self.chrome.border_width;

        // Focused border ring drawn as a slightly larger rounded fill underneath.
        if bw > 0.0 {
            let bc = if focused {
                wm.config.active_border
            } else {
                wm.config.inactive_border
            };
            // Make inactive border almost transparent so it's subtle.
            let bc = if focused {
                bc
            } else {
                [bc[0], bc[1], bc[2], bc[3] * 0.4]
            };
            self.fill_rounded(
                proj,
                rect.x as f32 - bw,
                rect.y as f32 - bw,
                rect.w as f32 + bw * 2.0,
                rect.h as f32 + bw * 2.0,
                cr + bw,
                bc,
            );
        }

        self.blit_rounded(
            proj,
            rect.x as f32,
            rect.y as f32,
            rect.w as f32,
            rect.h as f32,
            cr,
            tex,
            1.0,
        );
    }

    // ── Layer surfaces ────────────────────────────────────────────────────────

    fn draw_layers(
        &self,
        proj: &[f32; 9],
        surfaces: &[(
            LayerSurfaceRef,
            wayland_server::protocol::wl_surface::WlSurface,
        )],
        ow: i32,
        oh: i32,
        target: Layer,
    ) {
        use wayland_server::Resource as _;
        for (ls_ref, surf) in surfaces {
            let ls = ls_ref.lock().unwrap();
            if ls.layer != target || !ls.mapped {
                continue;
            }
            let sid = surf.id().protocol_id();
            let (x, y, lw, lh) = layer_geom(&ls, ow, oh);
            drop(ls);
            if let Some(tex) = self.layer_textures.get(&sid) {
                // Layer surfaces: no rounding (they're typically bars/docks)
                self.blit(proj, x as f32, y as f32, lw as f32, lh as f32, tex, 1.0);
            }
        }
    }

    // ── GL primitives ─────────────────────────────────────────────────────────

    fn fill_rounded(&self, proj: &[f32; 9], x: f32, y: f32, w: f32, h: f32, r: f32, c: [f32; 4]) {
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

    fn fill(&self, proj: &[f32; 9], x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.fill_rounded(proj, x, y, w, h, 0.0, c);
    }

    fn blit(&self, proj: &[f32; 9], x: f32, y: f32, w: f32, h: f32, tex: &GlTexture, alpha: f32) {
        self.blit_rounded(proj, x, y, w, h, 0.0, tex, alpha);
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

    // ── Texture management ────────────────────────────────────────────────────

    pub fn upload_surface_texture(
        &mut self,
        id: WindowId,
        s: &wayland_server::protocol::wl_surface::WlSurface,
        egl: &EglContext,
    ) {
        upload_surface(&mut self.textures, id, s, egl);
    }

    pub fn upload_layer_texture(
        &mut self,
        id: u32,
        s: &wayland_server::protocol::wl_surface::WlSurface,
        egl: &EglContext,
    ) {
        upload_surface(&mut self.layer_textures, id, s, egl);
    }

    pub fn remove_window_texture(&mut self, id: WindowId) {
        self.textures.remove(&id);
    }

    pub fn remove_layer_texture(&mut self, id: u32) {
        self.layer_textures.remove(&id);
    }

    pub fn release_buffer(&mut self, _buf: &wayland_server::protocol::wl_buffer::WlBuffer) {}
}

// ── Surface texture upload ────────────────────────────────────────────────────

fn upload_surface<K: std::hash::Hash + Eq + Copy>(
    map: &mut HashMap<K, GlTexture>,
    key: K,
    surf: &wayland_server::protocol::wl_surface::WlSurface,
    egl: &EglContext,
) {
    use crate::proto::{compositor::SurfaceData, dmabuf::DmaBufBuffer, shm::ShmBuffer};
    use std::os::unix::io::AsRawFd;
    use std::sync::Arc;
    use wayland_server::Resource as _;

    let sd = match surf.data::<Arc<SurfaceData>>() {
        Some(d) => d.clone(),
        None => return,
    };
    let buf = match sd.current.lock().unwrap().buffer.clone() {
        Some(b) => b,
        None => return,
    };

    // ── DMA-BUF path ─────────────────────────────────────────────────────────
    if let Some(dmabuf) = buf.data::<DmaBufBuffer>() {
        let tex = map.entry(key).or_insert_with(GlTexture::new_empty);

        let active: Vec<&crate::proto::dmabuf::DmaBufPlane> =
            dmabuf.planes.iter().filter(|p| p.fd.is_some()).collect();

        if active.is_empty() {
            tracing::warn!("upload_surface: DmaBufBuffer has no active planes");
            return;
        }

        let fds: Vec<i32> = active
            .iter()
            .map(|p| p.fd.as_ref().unwrap().as_raw_fd())
            .collect();
        let offsets: Vec<u32> = active.iter().map(|p| p.offset).collect();
        let strides: Vec<u32> = active.iter().map(|p| p.stride).collect();

        let first = active[0];
        let modifier: u64 = ((first.modifier_hi as u64) << 32) | (first.modifier_lo as u64);

        let raw = crate::state::RawBuffer::Dmabuf {
            fds,
            offsets,
            strides,
            modifier,
            width: dmabuf.width,
            height: dmabuf.height,
            format: dmabuf.format,
        };

        tex.upload_buffer(&raw, egl);
        return;
    }

    // ── SHM path — delegate to GlTexture::upload_shm so EglImage is cleared ──
    if let Some(shm) = buf.data::<ShmBuffer>() {
        let tex = map.entry(key).or_insert_with(GlTexture::new_empty);
        // Re-use the same raw-fd + mmap path inside GlTexture so we don't
        // duplicate the upload logic and correctly drop any stale EglImage.
        shm.with_data(|bytes| {
            // Build a temporary RawBuffer::Shm from the ShmBuffer metadata.
            // This avoids duplicating the mmap/TexImage2D logic.
            // ShmBuffer guarantees the data is valid for the duration of this closure.
            unsafe {
                gl::BindTexture(gl::TEXTURE_2D, tex.id);
                set_tex_params();
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA as i32,
                    shm.width,
                    shm.height,
                    0,
                    gl::BGRA,
                    gl::UNSIGNED_BYTE,
                    bytes.as_ptr() as *const _,
                );
                gl::BindTexture(gl::TEXTURE_2D, 0);
            }
            // Invalidate any stale EglImage from a previous DMA-BUF commit.
            tex.clear_egl_image();
        });
    }
}

unsafe fn set_tex_params() {
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn ortho(w: f32, h: f32) -> [f32; 9] {
    // Column-major 3×3 orthographic: maps [0,w]×[0,h] → [-1,1]×[1,-1]
    // (Y flipped so pixel (0,0) is top-left)
    [2.0 / w, 0.0, 0.0, 0.0, -2.0 / h, 0.0, -1.0, 1.0, 1.0]
}

fn layer_geom(
    ls: &crate::proto::layer_shell::LayerSurfaceState,
    ow: i32,
    oh: i32,
) -> (i32, i32, i32, i32) {
    use wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_surface_v1::Anchor;
    let (rw, rh) = (ls.size.0 as i32, ls.size.1 as i32);
    let [mt, mr, mb, ml] = ls.margin;
    let a = ls.anchor;
    let lw = if rw > 0 {
        rw
    } else if a.contains(Anchor::Left) && a.contains(Anchor::Right) {
        ow - ml - mr
    } else {
        ow / 2
    };
    let lh = if rh > 0 {
        rh
    } else if a.contains(Anchor::Top) && a.contains(Anchor::Bottom) {
        oh - mt - mb
    } else {
        oh / 8
    };
    let x = if a.contains(Anchor::Left) {
        ml
    } else if a.contains(Anchor::Right) {
        ow - lw - mr
    } else {
        (ow - lw) / 2
    };
    let y = if a.contains(Anchor::Top) {
        mt
    } else if a.contains(Anchor::Bottom) {
        oh - lh - mb
    } else {
        (oh - lh) / 2
    };
    (x, y, lw, lh)
}

fn find_font() -> Option<String> {
    [
        "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",
        "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/Hack-Regular.ttf",
    ]
    .iter()
    .find(|p| std::path::Path::new(p).exists())
    .map(|s| s.to_string())
}
