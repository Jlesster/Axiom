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
    backend::OutputSurface,
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
uniform mat3 u_proj;
uniform vec4 u_rect;   // x, y, w, h in pixels
void main() {
    vec2 world  = a_pos * u_rect.zw + u_rect.xy;
    vec3 ndc    = u_proj * vec3(world, 1.0);
    gl_Position = vec4(ndc.xy, 0.0, 1.0);
    v_uv = a_uv;
}
"#;

const TEX_FRAG: &str = r#"
#version 330 core
in  vec2 v_uv;
out vec4 frag;
uniform sampler2D u_tex;
uniform float u_alpha;
void main() { frag = texture(u_tex, v_uv) * u_alpha; }
"#;

const SOLID_FRAG: &str = r#"
#version 330 core
out vec4 frag;
uniform vec4 u_color;
void main() { frag = u_color; }
"#;

// Grayscale glyph: RED atlas, single-channel coverage.
const GLYPH_GRAY_FRAG: &str = r#"
#version 330 core
in  vec2 v_uv;
out vec4 frag;
uniform sampler2D u_tex;
uniform vec4 u_color;
void main() {
    float cov = texture(u_tex, v_uv).r;
    frag = vec4(u_color.rgb, u_color.a * cov);
}
"#;

// LCD subpixel glyph: RGB atlas, per-channel coverage.
// u_bg must be the opaque background colour so per-channel blend is correct.
const GLYPH_LCD_FRAG: &str = r#"
#version 330 core
in  vec2 v_uv;
out vec4 frag;
uniform sampler2D u_tex;
uniform vec4 u_color;
uniform vec4 u_bg;
void main() {
    vec3 cov  = texture(u_tex, v_uv).rgb;
    // Per-channel blend against the known background.
    vec3 rgb  = u_color.rgb * cov + u_bg.rgb * (1.0 - cov);
    float a   = max(cov.r, max(cov.g, cov.b)) * u_color.a;
    frag = vec4(rgb * a, a);
}
"#;

// ── RenderState ───────────────────────────────────────────────────────────────

pub struct RenderState {
    pub prog_tex: GlProgram,
    pub prog_solid: GlProgram,
    pub prog_glyph_gray: GlProgram,
    pub prog_glyph_lcd: GlProgram,
    pub quad_vao: QuadVao,
    pub glyph_vao: glyph_vao::GlyphVao,
    pub textures: HashMap<WindowId, GlTexture>,
    pub layer_textures: HashMap<u32, GlTexture>,
    pub hw_cursor: Option<HwCursor>,
    pub font: Option<font::FontAtlas>,
    pub bar: bar::BarState,
}

impl RenderState {
    pub fn new() -> Result<Self> {
        let prog_tex = GlProgram::compile(QUAD_VERT, TEX_FRAG)?;
        let prog_solid = GlProgram::compile(QUAD_VERT, SOLID_FRAG)?;
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
            prog_glyph_gray,
            prog_glyph_lcd,
            quad_vao,
            glyph_vao,
            textures: HashMap::new(),
            layer_textures: HashMap::new(),
            hw_cursor: None,
            font,
            bar: bar::BarState::new(bar::BarConfig::default()),
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

        let (tiled, floating): (Vec<WindowId>, Vec<WindowId>) = wm.workspaces[aws]
            .windows
            .iter()
            .copied()
            .partition(|&id| wm.windows.get(&id).map(|w| !w.floating).unwrap_or(true));

        for &id in tiled.iter().chain(floating.iter()) {
            let Some(win) = wm.windows.get(&id) else {
                continue;
            };
            let rect = anim.get_rect(id, win.rect);
            let focused = wm.focused_window() == Some(id);
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
            self.fill(&proj, cx, cy, 8.0, 12.0, [1.0, 1.0, 1.0, 0.9]);
        }
    }

    // ── Bar ───────────────────────────────────────────────────────────────────

    fn draw_bar(&mut self, proj: &[f32; 9], wm: &WmState, out_w: f32, mon_idx: usize) {
        let Some(font) = self.font.as_mut() else {
            return;
        };

        // Raw pointers let us pass the GL objects into closures without
        // conflicting with the &mut self.bar borrow.  All accesses are
        // single-threaded and read-only on the program/vao side.
        let ps = &self.prog_solid as *const GlProgram;
        let pg = &self.prog_glyph_gray as *const GlProgram;
        let pl = &self.prog_glyph_lcd as *const GlProgram;
        let qv = &self.quad_vao as *const QuadVao;
        let gv = &self.glyph_vao as *const glyph_vao::GlyphVao;

        let bg = bar::col::MANTLE; // bar background for LCD blend

        let fill_fn = |proj: [f32; 9], x: f32, y: f32, w: f32, h: f32, c: [f32; 4]| unsafe {
            let p = &*ps;
            p.bind();
            gl::UniformMatrix3fv(p.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(p.loc("u_rect"), x, y, w, h);
            gl::Uniform4f(p.loc("u_color"), c[0], c[1], c[2], c[3]);
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
                // u_bg: -1 on gray shader → no-op, fine.
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
        let bw = wm.config.border_w as f32;
        if bw > 0.0 {
            let c = if focused {
                wm.config.active_border
            } else {
                wm.config.inactive_border
            };
            self.fill(
                proj,
                rect.x as f32 - bw,
                rect.y as f32 - bw,
                rect.w as f32 + bw * 2.0,
                rect.h as f32 + bw * 2.0,
                c,
            );
        }
        if let Some(tex) = self.textures.get(&id) {
            self.blit(
                proj,
                rect.x as f32,
                rect.y as f32,
                rect.w as f32,
                rect.h as f32,
                tex,
                1.0,
            );
        }
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

    fn fill(&self, proj: &[f32; 9], x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        unsafe {
            self.prog_solid.bind();
            gl::UniformMatrix3fv(self.prog_solid.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(self.prog_solid.loc("u_rect"), x, y, w, h);
            gl::Uniform4f(self.prog_solid.loc("u_color"), c[0], c[1], c[2], c[3]);
            self.quad_vao.draw();
        }
    }

    fn blit(&self, proj: &[f32; 9], x: f32, y: f32, w: f32, h: f32, tex: &GlTexture, alpha: f32) {
        unsafe {
            self.prog_tex.bind();
            gl::UniformMatrix3fv(self.prog_tex.loc("u_proj"), 1, gl::FALSE, proj.as_ptr());
            gl::Uniform4f(self.prog_tex.loc("u_rect"), x, y, w, h);
            gl::Uniform1i(self.prog_tex.loc("u_tex"), 0);
            gl::Uniform1f(self.prog_tex.loc("u_alpha"), alpha);
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
    ) {
        upload_shm(&mut self.textures, id, s);
    }
    pub fn upload_layer_texture(
        &mut self,
        id: u32,
        s: &wayland_server::protocol::wl_surface::WlSurface,
    ) {
        upload_shm(&mut self.layer_textures, id, s);
    }
    pub fn remove_window_texture(&mut self, id: WindowId) {
        self.textures.remove(&id);
    }
    pub fn remove_layer_texture(&mut self, id: u32) {
        self.layer_textures.remove(&id);
    }
    pub fn release_buffer(&mut self, _: &wayland_server::protocol::wl_buffer::WlBuffer) {}
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

fn upload_shm<K: std::hash::Hash + Eq + Copy>(
    map: &mut HashMap<K, GlTexture>,
    key: K,
    surf: &wayland_server::protocol::wl_surface::WlSurface,
) {
    use crate::proto::{compositor::SurfaceData, shm::ShmBuffer};
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
    if let Some(shm) = buf.data::<ShmBuffer>() {
        let tex = map.entry(key).or_insert_with(GlTexture::new_empty);
        shm.with_data(|b| unsafe {
            gl::BindTexture(gl::TEXTURE_2D, tex.id);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                shm.width,
                shm.height,
                0,
                gl::BGRA,
                gl::UNSIGNED_BYTE,
                b.as_ptr() as *const _,
            );
            gl::BindTexture(gl::TEXTURE_2D, 0);
        });
    }
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
