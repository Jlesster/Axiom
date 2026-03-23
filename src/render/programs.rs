// src/render/programs.rs — GL shader program wrapper, VAO, and GL texture.

use anyhow::{bail, Result};
use std::ffi::CString;

use crate::state::RawBuffer;

// ── syscall + EGL proc-address shims ─────────────────────────────────────────

mod sys {
    use std::ffi::c_void;
    extern "C" {
        pub fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        pub fn munmap(addr: *mut c_void, len: usize) -> i32;
        pub fn eglGetCurrentDisplay() -> *mut c_void;
        pub fn eglGetProcAddress(name: *const u8) -> *mut c_void;
    }
    pub const PROT_READ: i32 = 0x1;
    pub const MAP_SHARED: i32 = 0x01;
    pub const MAP_FAILED: *mut c_void = !0usize as *mut _;

    pub unsafe fn egl_get_current_display() -> *mut c_void {
        eglGetCurrentDisplay()
    }
}

/// Look up an EGL/GL extension function by NUL-terminated name.
unsafe fn egl_proc(name: &[u8]) -> Option<*mut std::ffi::c_void> {
    let p = unsafe { sys::eglGetProcAddress(name.as_ptr()) };
    if p.is_null() {
        None
    } else {
        Some(p)
    }
}

pub struct GlProgram {
    pub id: u32,
}

impl GlProgram {
    pub fn compile(vert_src: &str, frag_src: &str) -> Result<Self> {
        unsafe {
            let vert = compile_shader(gl::VERTEX_SHADER, vert_src)?;
            let frag = compile_shader(gl::FRAGMENT_SHADER, frag_src)?;

            let prog = gl::CreateProgram();
            gl::AttachShader(prog, vert);
            gl::AttachShader(prog, frag);
            gl::LinkProgram(prog);

            let mut status = 0i32;
            gl::GetProgramiv(prog, gl::LINK_STATUS, &mut status);
            if status == 0 {
                let mut len = 0i32;
                gl::GetProgramiv(prog, gl::INFO_LOG_LENGTH, &mut len);
                let mut buf = vec![0u8; len as usize];
                gl::GetProgramInfoLog(prog, len, std::ptr::null_mut(), buf.as_mut_ptr() as _);
                bail!("program link: {}", String::from_utf8_lossy(&buf));
            }

            gl::DeleteShader(vert);
            gl::DeleteShader(frag);

            Ok(Self { id: prog })
        }
    }

    pub fn bind(&self) {
        unsafe {
            gl::UseProgram(self.id);
        }
    }

    pub fn loc(&self, name: &str) -> i32 {
        let c = CString::new(name).unwrap();
        unsafe { gl::GetUniformLocation(self.id, c.as_ptr()) }
    }
}

impl Drop for GlProgram {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteProgram(self.id);
        }
    }
}

unsafe fn compile_shader(kind: u32, src: &str) -> Result<u32> {
    let shader = gl::CreateShader(kind);
    let c = CString::new(src).unwrap();
    let ptr = c.as_ptr();
    gl::ShaderSource(shader, 1, &ptr, std::ptr::null());
    gl::CompileShader(shader);

    let mut status = 0i32;
    gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut status);
    if status == 0 {
        let mut len = 0i32;
        gl::GetShaderiv(shader, gl::INFO_LOG_LENGTH, &mut len);
        let mut buf = vec![0u8; len as usize];
        gl::GetShaderInfoLog(shader, len, std::ptr::null_mut(), buf.as_mut_ptr() as _);
        bail!("shader compile: {}", String::from_utf8_lossy(&buf));
    }
    Ok(shader)
}

// ─────────────────────────────────────────────────────────────────────────────
// src/render/gl_util.rs — VAO/VBO for a unit quad.
// ─────────────────────────────────────────────────────────────────────────────

/// A VAO+VBO that holds a unit [0,1]² quad.
/// Vertex layout: vec2 position (a_pos), vec2 uv (a_uv).
pub struct QuadVao {
    vao: u32,
    vbo: u32,
}

impl QuadVao {
    pub fn new() -> Self {
        // Interleaved: pos(2) + uv(2)
        #[rustfmt::skip]
        let verts: [f32; 24] = [
            // pos       uv
            0.0, 0.0,  0.0, 0.0,
            1.0, 0.0,  1.0, 0.0,
            1.0, 1.0,  1.0, 1.0,

            0.0, 0.0,  0.0, 0.0,
            1.0, 1.0,  1.0, 1.0,
            0.0, 1.0,  0.0, 1.0,
        ];

        let (mut vao, mut vbo) = (0u32, 0u32);
        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);
            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (verts.len() * 4) as isize,
                verts.as_ptr() as _,
                gl::STATIC_DRAW,
            );
            let stride = (4 * std::mem::size_of::<f32>()) as i32;
            // a_pos
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, 0 as _);
            gl::EnableVertexAttribArray(0);
            // a_uv
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
        }
        Self { vao, vbo }
    }

    pub fn draw(&self) {
        unsafe {
            gl::BindVertexArray(self.vao);
            gl::DrawArrays(gl::TRIANGLES, 0, 6);
            gl::BindVertexArray(0);
        }
    }
}

impl Drop for QuadVao {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteBuffers(1, &self.vbo);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// src/render/texture.rs — GL texture from wl_buffer.
// ─────────────────────────────────────────────────────────────────────────────

pub struct GlTexture {
    pub id: u32,
}

impl GlTexture {
    pub fn new_empty() -> Self {
        let mut id = 0u32;
        unsafe {
            gl::GenTextures(1, &mut id);
        }
        Self { id }
    }

    pub fn upload_buffer(&mut self, buf: &RawBuffer, w: i32, h: i32) {
        match buf {
            RawBuffer::Shm {
                pool_fd,
                offset,
                width,
                height,
                stride,
                format: _,
            } => {
                let size = (*stride * *height) as usize;
                let ptr = unsafe {
                    sys::mmap(
                        std::ptr::null_mut(),
                        size,
                        sys::PROT_READ,
                        sys::MAP_SHARED,
                        *pool_fd,
                        *offset as i64,
                    )
                };
                if ptr == sys::MAP_FAILED {
                    tracing::warn!("mmap SHM pool failed");
                    return;
                }
                unsafe {
                    gl::BindTexture(gl::TEXTURE_2D, self.id);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
                    gl::TexImage2D(
                        gl::TEXTURE_2D,
                        0,
                        gl::RGBA as i32,
                        *width,
                        *height,
                        0,
                        gl::BGRA,
                        gl::UNSIGNED_BYTE,
                        ptr,
                    );
                    gl::BindTexture(gl::TEXTURE_2D, 0);
                    sys::munmap(ptr, size);
                }
            }
            RawBuffer::Dmabuf {
                fds,
                strides,
                width,
                height,
                format,
                ..
            } => {
                self.import_dmabuf(fds[0], *width, *height, *format, strides[0]);
            }
        }
    }

    fn import_dmabuf(&mut self, fd: i32, w: i32, h: i32, fmt: u32, stride: u32) {
        type V = std::ffi::c_void;
        type CreateFn = unsafe extern "C" fn(*mut V, *mut V, u32, *mut V, *const i32) -> *mut V;
        type DestroyFn = unsafe extern "C" fn(*mut V, *mut V);
        type TargetFn = unsafe extern "C" fn(u32, *mut V);

        const DMA_BUF: u32 = 0x3270;
        let attribs: [i32; 13] = [
            0x3057,
            w,
            0x3056,
            h,
            0x3271,
            fmt as i32,
            0x3272,
            fd,
            0x3273,
            0,
            0x3274,
            stride as i32,
            0x3038,
        ];

        unsafe {
            let Some(create) = egl_proc(b"eglCreateImageKHR\0") else {
                return;
            };
            let Some(destroy) = egl_proc(b"eglDestroyImageKHR\0") else {
                return;
            };
            let Some(target) = egl_proc(b"glEGLImageTargetTexture2DOES\0") else {
                return;
            };
            let create: CreateFn = std::mem::transmute(create);
            let destroy: DestroyFn = std::mem::transmute(destroy);
            let target: TargetFn = std::mem::transmute(target);
            let dpy = sys::egl_get_current_display();
            let img = create(
                dpy,
                std::ptr::null_mut(),
                DMA_BUF,
                std::ptr::null_mut(),
                attribs.as_ptr(),
            );
            if img.is_null() {
                tracing::warn!("eglCreateImageKHR failed");
                return;
            }
            gl::BindTexture(gl::TEXTURE_2D, self.id);
            target(gl::TEXTURE_2D, img);
            gl::BindTexture(gl::TEXTURE_2D, 0);
            destroy(dpy, img);
        }
    }
}

impl Drop for GlTexture {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.id);
        }
    }
}
