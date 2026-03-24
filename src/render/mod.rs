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
out vec2 v_pos;
uniform mat3 u_proj;
uniform vec4 u_rect;
void main() {
    v_uv  = a_uv;
    v_pos = a_pos * u_rect.zw;
    vec2 world  = a_pos * u_rect.zw + u_rect.xy;
    vec3 ndc    = u_proj * vec3(world, 1.0);
    gl_Position = vec4(ndc.xy, 0.0, 1.0);
}
"#;

const TEX_FRAG: &str = r#"
#version 330 core
in  vec2 v_uv;
in  vec2 v_pos;
out vec4 frag;
uniform sampler2D u_tex;
uniform float     u_alpha;
uniform vec2      u_size;
uniform float     u_radius;

float rounded_alpha(vec2 pos, vec2 sz, float r) {
    vec2 q = abs(pos - sz * 0.5) - sz * 0.5 + r;
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
    return clamp(-d + 0.5, 0.0, 1.0);
}

void main() {
    float mask = rounded_alpha(v_pos, u_size, u_radius);
    vec4 c = texture(u_tex, v_uv);
    frag = vec4(c.rgb, c.a * u_alpha * mask);
}
"#;

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
    return clamp(-d + 0.5, 0.0, 1.0);
}

void main() {
    float mask = rounded_alpha(v_pos, u_size, u_radius);
    frag = u_color * vec4(1.0, 1.0, 1.0, mask);
}
"#;

const BORDER_FRAG: &str = r#"
#version 330 core
in  vec2 v_pos;
out vec4 frag;

uniform vec2  u_size;
uniform float u_radius;
uniform float u_thickness;
uniform vec4  u_col_a;
uniform vec4  u_col_b;
uniform float u_focused;

float rounded_sdf(vec2 pos, vec2 sz, float r) {
    vec2 q = abs(pos - sz * 0.5) - sz * 0.5 + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    float outer = rounded_sdf(v_pos, u_size, u_radius);
    float inner = rounded_sdf(v_pos, u_size, max(u_radius - u_thickness, 0.0));

    float outer_mask = clamp(-outer + 0.5, 0.0, 1.0);
    float inner_mask = clamp( inner + 0.5, 0.0, 1.0);
    float ring = outer_mask * inner_mask;

    if (ring < 0.001) discard;

    vec2 c = v_pos - u_size * 0.5;
    float t = (atan(c.y, c.x) / 3.14159265 + 1.0) * 0.5;

    vec4 col = mix(u_col_a, u_col_b, t);
    if (u_focused < 0.5) {
        col = mix(col, vec4(0.0), 0.45);
    }
    frag = vec4(col.rgb * col.a, col.a) * ring;
}
"#;

const SHADOW_FRAG: &str = r#"
#version 330 core
in  vec2 v_pos;
out vec4 frag;
uniform vec4  u_color;
uniform vec2  u_size;
uniform float u_radius;
uniform float u_blur;

float rounded_sdf(vec2 pos, vec2 sz, float r) {
    vec2 q = abs(pos - sz * 0.5) - sz * 0.5 + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    float d    = rounded_sdf(v_pos, u_size, u_radius);
    float blur = max(u_blur, 0.001);
    float a    = smoothstep(blur, 0.0, d) * u_color.a;
    frag = vec4(u_color.rgb * a, a);
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

// ── Chrome config ─────────────────────────────────────────────────────────────

pub struct ChromeConfig {
    pub corner_radius: f32,
    pub shadow_spread: f32,
    pub shadow_offset: f32,
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

// ── RenderState ───────────────────────────────────────────────────────────────

pub struct RenderState {
    pub prog_tex: GlProgram,
    pub prog_solid: GlProgram,
    pub prog_border: GlProgram,
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
        let prog_border = GlProgram::compile(QUAD_VERT, BORDER_FRAG)?;
        let prog_shadow = GlProgram::compile(QUAD_VERT, SHADOW_FRAG)?;
        let prog_glyph_gray = GlProgram::compile(QUAD_VERT, GLYPH_GRAY_FRAG)?;
        let prog_glyph_lcd = GlProgram::compile(QUAD_VERT, GLYPH_LCD_FRAG)?;
        let quad_vao = QuadVao::new();
        let glyph_vao = glyph_vao::GlyphVao::new();

        unsafe {
            gl::Enable(gl::BLEND);
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
            prog_border,
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

        // Build draw order: tiled first (back), floating on top, focused last
        // (so it always renders above everything else on its workspace).
        let draw_order = build_draw_order(wm, aws, focused_id);

        // Pass 1: shadows (back to front, skip windows with no texture yet)
        for &id in &draw_order {
            let Some(win) = wm.windows.get(&id) else {
                continue;
            };
            if !self.textures.contains_key(&id) {
                continue;
            }
            let rect = anim.get_rect(id, win.rect);
            let opacity = anim.get_opacity(id);
            if opacity < 0.01 {
                continue;
            }
            self.draw_shadow(&proj, rect, focused_id == Some(id), opacity);
        }

        // Pass 2: window content + border ring (back to front)
        for &id in &draw_order {
            let Some(win) = wm.windows.get(&id) else {
                continue;
            };
            if !self.textures.contains_key(&id) {
                continue;
            }
            let rect = anim.get_rect(id, win.rect);
            let opacity = anim.get_opacity(id);
            if opacity < 0.01 {
                continue;
            }
            self.draw_window(wm, &proj, id, rect, focused_id == Some(id), opacity);
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

        // Software cursor fallback.
        if self.hw_cursor.is_none() && !input.hw_cursor_active {
            let (cx, cy) = (input.pointer_x as f32, input.pointer_y as f32);
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

    fn draw_shadow(&self, proj: &[f32; 9], rect: Rect, focused: bool, opacity: f32) {
        let so = self.chrome.shadow_offset;
        let sr = self.chrome.shadow_spread;
        let cr = self.chrome.corner_radius;
        let bw = self.chrome.border_width;

        let sx = rect.x as f32 - so - bw;
        let sy = rect.y as f32 - so * 0.5 - bw;
        let sw = rect.w as f32 + (so + bw) * 2.0;
        let sh = rect.h as f32 + (so + bw) * 2.5;

        let base = if focused {
            self.chrome.shadow_focused
        } else {
            self.chrome.shadow_unfocused
        };
        let col = [base[0], base[1], base[2], base[3] * opacity];

        unsafe {
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
            gl::Uniform1f(self.prog_shadow.loc("u_radius"), cr + bw + so * 0.5);
            gl::Uniform1f(self.prog_shadow.loc("u_blur"), sr);
            self.quad_vao.draw();
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        }
    }

    // ── Window + border ring ──────────────────────────────────────────────────

    fn draw_window(
        &self,
        wm: &WmState,
        proj: &[f32; 9],
        id: WindowId,
        rect: Rect,
        focused: bool,
        opacity: f32,
    ) {
        let cr = self.chrome.corner_radius;
        let bw = self.chrome.border_width;

        if bw > 0.0 {
            let bx = rect.x as f32 - bw;
            let by = rect.y as f32 - bw;
            let bw2 = rect.w as f32 + bw * 2.0;
            let bh2 = rect.h as f32 + bw * 2.0;
            let br = cr + bw;

            let (ca, cb) = if focused {
                (self.chrome.border_active_a, self.chrome.border_active_b)
            } else {
                (self.chrome.border_inactive, self.chrome.border_inactive)
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

        let Some(tex) = self.textures.get(&id) else {
            return;
        };
        self.blit_rounded(
            proj,
            rect.x as f32,
            rect.y as f32,
            rect.w as f32,
            rect.h as f32,
            cr,
            tex,
            opacity,
        );
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
            gl::Uniform1f(p.loc("u_radius"), 0.0);
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

// ── Draw order helper ─────────────────────────────────────────────────────────
//
// Correct order: tiled windows back-to-front, then floating windows, then the
// focused window last (always on top regardless of tiled/floating status).

fn build_draw_order(wm: &WmState, aws: usize, focused_id: Option<WindowId>) -> Vec<WindowId> {
    let ws = &wm.workspaces[aws];

    let (mut tiled, mut floating): (Vec<WindowId>, Vec<WindowId>) = ws
        .windows
        .iter()
        .copied()
        .filter(|&id| focused_id != Some(id)) // focused goes last
        .partition(|&id| wm.windows.get(&id).map(|w| !w.floating).unwrap_or(true));

    let mut order = Vec::with_capacity(ws.windows.len());
    order.append(&mut tiled);
    order.append(&mut floating);
    if let Some(fid) = focused_id {
        if ws.windows.contains(&fid) {
            order.push(fid);
        }
    }
    order
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

    // ── DMA-BUF path ──────────────────────────────────────────────────────────
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
        let modifier = ((first.modifier_hi as u64) << 32) | (first.modifier_lo as u64);

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

    // ── SHM path ──────────────────────────────────────────────────────────────
    if let Some(shm) = buf.data::<ShmBuffer>() {
        let tex = map.entry(key).or_insert_with(GlTexture::new_empty);
        shm.with_data(|bytes| {
            unsafe {
                gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4);
                gl::BindTexture(gl::TEXTURE_2D, tex.id);
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA8 as i32,
                    shm.width,
                    shm.height,
                    0,
                    gl::BGRA,
                    gl::UNSIGNED_INT_8_8_8_8_REV,
                    bytes.as_ptr() as *const _,
                );
                set_tex_params();
                gl::BindTexture(gl::TEXTURE_2D, 0);
            }
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
        "/usr/share/fonts/TTF/IosevkaJlessBrainsNerdFont-Regular.ttf",
        "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",
        "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    ]
    .iter()
    .find(|p| std::path::Path::new(p).exists())
    .map(|s| s.to_string())
}
