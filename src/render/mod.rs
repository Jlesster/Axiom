use crate::backend::Backend;
use crate::wm::{Layout, Rect, WindowId, WmState};
use anyhow::Result;
use std::collections::HashMap;

// ── Per-window GL texture ─────────────────────────────────────────────────────

struct WindowTexture {
    tex_id: gl::types::GLuint,
    width: i32,
    height: i32,
}

impl Drop for WindowTexture {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.tex_id);
        }
    }
}

// ── Pending buffer release queue ──────────────────────────────────────────────
//
// wl_shm buffers must NOT be released until the GPU has finished reading them.
// We queue them here and flush after the page flip completes (i.e. on the
// next render cycle, which is safe because we wait for vsync via DRM).

pub struct PendingRelease {
    pub buffer: wayland_server::protocol::wl_buffer::WlBuffer,
}

// ── RenderState ───────────────────────────────────────────────────────────────

pub struct RenderState {
    gl_ready: bool,
    textures: HashMap<WindowId, WindowTexture>,
    /// Buffers uploaded last frame, waiting to be released after page flip.
    pending_release: Vec<wayland_server::protocol::wl_buffer::WlBuffer>,
}

impl RenderState {
    pub fn new(backend: &Backend) -> Result<Self> {
        Ok(Self {
            gl_ready: !backend.outputs.is_empty(),
            textures: HashMap::new(),
            pending_release: Vec::new(),
        })
    }

    /// Release buffers from the previous frame. Call this at the start of
    /// each new frame, after the page flip has completed (i.e. after present()).
    /// This is safe because DRM page flip is synchronous in our render loop.
    pub fn release_old_buffers(&mut self) {
        for buf in self.pending_release.drain(..) {
            buf.release();
        }
    }

    /// Upload a wl_shm buffer for a window. The old buffer is queued for
    /// deferred release rather than released immediately.
    pub fn upload_shm_buffer(
        &mut self,
        id: WindowId,
        data: &[u8],
        width: i32,
        height: i32,
        stride: i32,
        _format: wayland_server::protocol::wl_shm::Format,
    ) {
        if !self.gl_ready {
            return;
        }

        // Convert ARGB8888/XRGB8888 → RGBA for GL.
        // wl_shm ARGB8888 is stored as [B, G, R, A] in memory (little-endian).
        let pixel_count = (width * height) as usize;
        let mut rgba = vec![0u8; pixel_count * 4];
        for i in 0..pixel_count {
            let row = i / width as usize;
            let col = i % width as usize;
            let src = row * stride as usize + col * 4;
            if src + 3 >= data.len() {
                break;
            }
            rgba[i * 4] = data[src + 2]; // R ← B channel
            rgba[i * 4 + 1] = data[src + 1]; // G
            rgba[i * 4 + 2] = data[src]; // B ← R channel
            rgba[i * 4 + 3] = data[src + 3]; // A
        }

        unsafe {
            let entry = self.textures.entry(id).or_insert_with(|| {
                let mut tex = 0;
                gl::GenTextures(1, &mut tex);
                WindowTexture {
                    tex_id: tex,
                    width: 0,
                    height: 0,
                }
            });

            gl::BindTexture(gl::TEXTURE_2D, entry.tex_id);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);

            if entry.width != width || entry.height != height {
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA as i32,
                    width,
                    height,
                    0,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    rgba.as_ptr() as *const _,
                );
                entry.width = width;
                entry.height = height;
            } else {
                gl::TexSubImage2D(
                    gl::TEXTURE_2D,
                    0,
                    0,
                    0,
                    width,
                    height,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    rgba.as_ptr() as *const _,
                );
            }
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }
    }

    /// Queue a buffer for release after the current frame's page flip.
    pub fn queue_buffer_release(&mut self, buf: wayland_server::protocol::wl_buffer::WlBuffer) {
        self.pending_release.push(buf);
    }

    /// Free GL texture when a window closes.
    pub fn remove_window(&mut self, id: WindowId) {
        self.textures.remove(&id);
    }

    pub fn render_frame(
        &mut self,
        backend: &mut Backend,
        wm: &WmState,
        cursor_x: f64,
        cursor_y: f64,
    ) -> Result<()> {
        if !self.gl_ready {
            return Ok(());
        }

        // Release buffers from the previous frame — safe now that the
        // page flip has completed (DRM waits for vsync before returning).
        self.release_old_buffers();

        let output_count = backend.outputs.len();
        for i in 0..output_count {
            let (w, h) = (backend.outputs[i].width, backend.outputs[i].height);
            backend.outputs[i].make_current(&backend.egl)?;
            unsafe {
                gl::Viewport(0, 0, w as i32, h as i32);
            }

            let mon = wm.monitors.get(i);
            let ws_idx = mon.map(|m| m.active_ws).unwrap_or(0);
            let bar_h = wm.config.bar_height as i32;
            let bar_bot = wm.config.bar_at_bottom;

            self.render_workspace(wm, ws_idx, w as i32, h as i32, bar_h, bar_bot);

            // Draw software cursor on top of everything
            draw_cursor(cursor_x as i32, cursor_y as i32, w as i32, h as i32);

            backend.outputs[i].present(&backend.drm, &backend.egl)?;
        }
        Ok(())
    }

    fn render_workspace(
        &self,
        wm: &WmState,
        ws_idx: usize,
        vp_w: i32,
        vp_h: i32,
        bar_h: i32,
        bar_bottom: bool,
    ) {
        let cfg = &wm.config;
        let ws = match wm.workspaces.get(ws_idx) {
            Some(w) => w,
            None => return,
        };
        let focused_id = ws.focused;

        unsafe {
            gl::ClearColor(0.07, 0.07, 0.10, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        for &id in &ws.windows {
            let Some(win) = wm.windows.get(&id) else {
                continue;
            };
            if ws.layout == Layout::Monocle && focused_id != Some(id) {
                continue;
            }

            let is_focused = focused_id == Some(id);
            let border_color = if is_focused {
                cfg.active_border_f32()
            } else {
                cfg.inactive_border_f32()
            };

            if cfg.border_w > 0 && !win.fullscreen {
                fill_rect(win.rect, border_color, vp_w, vp_h);
            }

            let inner = win.rect.inset(cfg.border_w as i32);

            if let Some(tex) = self.textures.get(&id) {
                draw_texture(tex.tex_id, inner, vp_w, vp_h);
            } else {
                fill_rect(inner, [0.15, 0.15, 0.22, 1.0], vp_w, vp_h);
            }
        }

        // Status bar
        let bar_rect = if bar_bottom {
            Rect::new(0, vp_h - bar_h, vp_w, bar_h)
        } else {
            Rect::new(0, 0, vp_w, bar_h)
        };
        fill_rect(bar_rect, cfg.bar_bg_f32(), vp_w, vp_h);
        draw_workspace_indicators(wm, ws_idx, bar_rect, vp_w, vp_h);
    }
}

// ── Software cursor ───────────────────────────────────────────────────────────
//
// A simple 12×20 arrow cursor drawn with GL primitives. Replaced by xcursor
// theme loading in a future iteration.

fn draw_cursor(cx: i32, cy: i32, vp_w: i32, vp_h: i32) {
    // Arrow shape: filled white quad + black outline approximation
    // We draw a small filled rectangle as a placeholder cursor.
    // Real xcursor support is a future TODO.
    let cursor_w = 12i32;
    let cursor_h = 20i32;

    // White fill
    fill_rect(
        Rect::new(cx, cy, cursor_w, cursor_h),
        [1.0, 1.0, 1.0, 0.9],
        vp_w,
        vp_h,
    );
    // Black border (1px inset)
    fill_rect(
        Rect::new(cx + 1, cy + 1, cursor_w - 2, cursor_h - 2),
        [0.85, 0.85, 0.85, 1.0],
        vp_w,
        vp_h,
    );
    // Top-left corner pixel in black for directional hint
    fill_rect(Rect::new(cx, cy, 2, 2), [0.0, 0.0, 0.0, 1.0], vp_w, vp_h);
}

// ── Texture drawing ───────────────────────────────────────────────────────────

static TEXTURE_SHADER: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

fn get_texture_shader() -> u32 {
    *TEXTURE_SHADER.get_or_init(|| unsafe {
        let vs_src = b"attribute vec2 pos; attribute vec2 uv; varying vec2 v_uv;
            void main() { gl_Position = vec4(pos, 0.0, 1.0); v_uv = uv; }\0";
        let fs_src = b"precision mediump float; uniform sampler2D tex; varying vec2 v_uv;
            void main() { gl_FragColor = texture2D(tex, v_uv); }\0";

        let vs = gl::CreateShader(gl::VERTEX_SHADER);
        gl::ShaderSource(vs, 1, &(vs_src.as_ptr() as *const _), std::ptr::null());
        gl::CompileShader(vs);

        let fs = gl::CreateShader(gl::FRAGMENT_SHADER);
        gl::ShaderSource(fs, 1, &(fs_src.as_ptr() as *const _), std::ptr::null());
        gl::CompileShader(fs);

        let prog = gl::CreateProgram();
        gl::AttachShader(prog, vs);
        gl::AttachShader(prog, fs);
        gl::LinkProgram(prog);
        gl::DeleteShader(vs);
        gl::DeleteShader(fs);
        prog
    })
}

fn draw_texture(tex_id: u32, rect: Rect, vp_w: i32, vp_h: i32) {
    if rect.w <= 0 || rect.h <= 0 {
        return;
    }

    let x0 = 2.0 * rect.x as f32 / vp_w as f32 - 1.0;
    let x1 = 2.0 * (rect.x + rect.w) as f32 / vp_w as f32 - 1.0;
    let y1 = -2.0 * rect.y as f32 / vp_h as f32 + 1.0;
    let y0 = -2.0 * (rect.y + rect.h) as f32 / vp_h as f32 + 1.0;

    #[rustfmt::skip]
    let verts: [f32; 24] = [
        x0, y0, 0.0, 1.0,
        x1, y0, 1.0, 1.0,
        x0, y1, 0.0, 0.0,
        x1, y0, 1.0, 1.0,
        x1, y1, 1.0, 0.0,
        x0, y1, 0.0, 0.0,
    ];

    unsafe {
        let prog = get_texture_shader();
        gl::UseProgram(prog);
        gl::BindTexture(gl::TEXTURE_2D, tex_id);
        gl::Uniform1i(
            gl::GetUniformLocation(prog, b"tex\0".as_ptr() as *const _),
            0,
        );

        let pos_loc = gl::GetAttribLocation(prog, b"pos\0".as_ptr() as *const _) as u32;
        let uv_loc = gl::GetAttribLocation(prog, b"uv\0".as_ptr() as *const _) as u32;

        gl::EnableVertexAttribArray(pos_loc);
        gl::EnableVertexAttribArray(uv_loc);

        let stride = (4 * std::mem::size_of::<f32>()) as i32;
        let base = verts.as_ptr();
        gl::VertexAttribPointer(pos_loc, 2, gl::FLOAT, gl::FALSE, stride, base as *const _);
        gl::VertexAttribPointer(
            uv_loc,
            2,
            gl::FLOAT,
            gl::FALSE,
            stride,
            base.add(2) as *const _,
        );

        gl::DrawArrays(gl::TRIANGLES, 0, 6);

        gl::DisableVertexAttribArray(pos_loc);
        gl::DisableVertexAttribArray(uv_loc);
        gl::BindTexture(gl::TEXTURE_2D, 0);
        gl::UseProgram(0);
    }
}

// ── Solid rect ────────────────────────────────────────────────────────────────

pub fn fill_rect(rect: Rect, color: [f32; 4], _vp_w: i32, vp_h: i32) {
    if rect.w <= 0 || rect.h <= 0 {
        return;
    }
    let scissor_y = vp_h - rect.y - rect.h;
    unsafe {
        gl::Enable(gl::SCISSOR_TEST);
        gl::Scissor(rect.x, scissor_y, rect.w, rect.h);
        gl::ClearColor(color[0], color[1], color[2], color[3]);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::Disable(gl::SCISSOR_TEST);
    }
}

// ── Workspace indicators ──────────────────────────────────────────────────────

fn draw_workspace_indicators(wm: &WmState, active_ws: usize, bar: Rect, vp_w: i32, vp_h: i32) {
    let cell_w = 20i32;
    let pad = 4i32;
    for i in 0..wm.workspaces.len() {
        let has_windows = !wm.workspaces[i].windows.is_empty();
        let is_active = i == active_ws;
        let color = if is_active {
            [0.7, 0.85, 1.0, 1.0f32]
        } else if has_windows {
            [0.5, 0.5, 0.7, 1.0]
        } else {
            [0.3, 0.3, 0.4, 1.0]
        };
        let r = Rect::new(
            bar.x + i as i32 * (cell_w + pad) + pad,
            bar.y + pad,
            cell_w,
            bar.h - 2 * pad,
        );
        fill_rect(r, color, vp_w, vp_h);
    }
}
