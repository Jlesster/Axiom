// src/render/glyph_vao.rs — Streaming VAO for glyph quads.
//
// Each draw call uploads 6 vertices with exact per-glyph UV coordinates.
// This avoids the [0,1]² UV assumption of the shared QuadVao.

pub struct GlyphVao {
    pub vao: u32,
    pub vbo: u32,
}

impl GlyphVao {
    pub fn new() -> Self {
        let (mut vao, mut vbo) = (0u32, 0u32);
        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);

            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            // Pre-allocate for 6 vertices × (2 pos + 2 uv) floats.
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (6 * 4 * std::mem::size_of::<f32>()) as isize,
                std::ptr::null(),
                gl::STREAM_DRAW,
            );

            let stride = (4 * std::mem::size_of::<f32>()) as i32;
            // location 0: vec2 position (unit quad [0,1]²)
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, 0 as _);
            gl::EnableVertexAttribArray(0);
            // location 1: vec2 UV (exact atlas sub-rect)
            gl::VertexAttribPointer(
                1,
                2,
                gl::FLOAT,
                gl::FALSE,
                stride,
                (2 * std::mem::size_of::<f32>()) as _,
            );
            gl::EnableVertexAttribArray(1);

            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        }
        Self { vao, vbo }
    }

    /// Upload vertices for this glyph and draw.
    /// `uv` = [u0, v0, u1, v1] — the sub-rect in the atlas texture.
    pub fn draw(&self, uv: [f32; 4]) {
        let (u0, v0, u1, v1) = (uv[0], uv[1], uv[2], uv[3]);
        #[rustfmt::skip]
        let verts: [f32; 24] = [
            // x     y     u   v
            0.0,  0.0,   u0, v0,
            1.0,  0.0,   u1, v0,
            1.0,  1.0,   u1, v1,
            0.0,  0.0,   u0, v0,
            1.0,  1.0,   u1, v1,
            0.0,  1.0,   u0, v1,
        ];
        unsafe {
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::BufferSubData(
                gl::ARRAY_BUFFER,
                0,
                (verts.len() * std::mem::size_of::<f32>()) as isize,
                verts.as_ptr() as _,
            );
            gl::DrawArrays(gl::TRIANGLES, 0, 6);
            gl::BindVertexArray(0);
        }
    }
}

impl Drop for GlyphVao {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteBuffers(1, &self.vbo);
        }
    }
}
