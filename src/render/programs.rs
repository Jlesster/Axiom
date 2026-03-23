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
    }
    #[link(name = "EGL")]
    extern "C" {
        pub fn eglGetProcAddress(name: *const u8) -> *mut c_void;
    }
    pub const PROT_READ: i32 = 0x1;
    pub const MAP_SHARED: i32 = 0x01;
    pub const MAP_FAILED: *mut c_void = !0usize as *mut _;
}

unsafe fn egl_proc(name: &[u8]) -> Option<*mut std::ffi::c_void> {
    let p = unsafe { sys::eglGetProcAddress(name.as_ptr()) };
    if p.is_null() {
        None
    } else {
        Some(p)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GlProgram
// ─────────────────────────────────────────────────────────────────────────────

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
// QuadVao
// ─────────────────────────────────────────────────────────────────────────────

pub struct QuadVao {
    vao: u32,
    vbo: u32,
}

impl QuadVao {
    pub fn new() -> Self {
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
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, 0 as _);
            gl::EnableVertexAttribArray(0);
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
// GlTexture
// ─────────────────────────────────────────────────────────────────────────────

/// Owns a single GL texture name.
///
/// For DMA-BUF surfaces we also keep the `EglImage` alive alongside the
/// texture — `eglDestroyImageKHR` must NOT be called until the texture is
/// deleted.
pub struct GlTexture {
    pub id: u32,
    /// Kept alive so the driver's EGLImage → texture binding remains valid.
    _egl_image: Option<crate::backend::egl::EglImage>,
}

impl GlTexture {
    pub fn new_empty() -> Self {
        let mut id = 0u32;
        unsafe {
            gl::GenTextures(1, &mut id);
        }
        Self {
            id,
            _egl_image: None,
        }
    }

    /// Called by the SHM upload path in render/mod.rs after a TexImage2D call
    /// to invalidate any stale EGLImage from a previous DMA-BUF commit on
    /// the same texture slot.
    pub fn clear_egl_image(&mut self) {
        self._egl_image = None;
    }

    /// Upload from a SHM or DMA-BUF `RawBuffer`.
    pub fn upload_buffer(&mut self, buf: &RawBuffer, egl: &crate::backend::egl::EglContext) {
        match buf {
            RawBuffer::Shm {
                pool_fd,
                offset,
                width,
                height,
                stride,
                format: _,
            } => {
                self.upload_shm(*pool_fd, *width, *height, *stride, *offset);
            }
            RawBuffer::Dmabuf {
                fds,
                offsets,
                strides,
                modifier,
                width,
                height,
                format,
                ..
            } => {
                let mod_hi = (*modifier >> 32) as u32;
                let mod_lo = (*modifier & 0xFFFF_FFFF) as u32;
                self.upload_dmabuf(
                    egl,
                    fds,
                    offsets,
                    strides,
                    mod_hi,
                    mod_lo,
                    *width as u32,
                    *height as u32,
                    *format,
                );
            }
        }
    }

    // ── SHM upload ────────────────────────────────────────────────────────────

    fn upload_shm(&mut self, pool_fd: i32, width: i32, height: i32, stride: i32, offset: i32) {
        let size = (stride * height) as usize;
        let ptr = unsafe {
            sys::mmap(
                std::ptr::null_mut(),
                size,
                sys::PROT_READ,
                sys::MAP_SHARED,
                pool_fd,
                offset as i64,
            )
        };
        if ptr == sys::MAP_FAILED {
            tracing::warn!("mmap SHM pool failed");
            return;
        }
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.id);
            Self::set_tex_params();
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width,
                height,
                0,
                gl::BGRA,
                gl::UNSIGNED_BYTE,
                ptr,
            );
            gl::BindTexture(gl::TEXTURE_2D, 0);
            sys::munmap(ptr, size);
        }
        self._egl_image = None;
    }

    // ── DMA-BUF upload ───────────────────────────────────────────────────────

    fn upload_dmabuf(
        &mut self,
        egl: &crate::backend::egl::EglContext,
        fds: &[i32],
        offsets: &[u32],
        strides: &[u32],
        modifier_hi: u32,
        modifier_lo: u32,
        width: u32,
        height: u32,
        format: u32,
    ) {
        let n_planes = fds.len().min(offsets.len()).min(strides.len());
        let planes: Vec<crate::backend::egl::DmaBufPlane> = (0..n_planes)
            .map(|i| crate::backend::egl::DmaBufPlane {
                fd: fds[i],
                offset: offsets[i],
                stride: strides[i],
                modifier_hi,
                modifier_lo,
            })
            .collect();

        let egl_image = match egl.import_dmabuf(width, height, format, &planes) {
            Some(img) => img,
            None => {
                tracing::error!(
                    "DMA-BUF import failed: format={:#010x} {}x{} {} planes",
                    format,
                    width,
                    height,
                    n_planes
                );
                return;
            }
        };

        type TargetFn = unsafe extern "C" fn(u32, *mut std::ffi::c_void);
        let target_fn: TargetFn = unsafe {
            match egl_proc(b"glEGLImageTargetTexture2DOES\0") {
                Some(p) => std::mem::transmute(p),
                None => {
                    tracing::error!("glEGLImageTargetTexture2DOES not available");
                    return;
                }
            }
        };

        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.id);
            target_fn(gl::TEXTURE_2D, egl_image.raw());
            Self::set_tex_params();
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }

        let gl_err = unsafe { gl::GetError() };
        if gl_err != gl::NO_ERROR {
            tracing::error!("GL error 0x{:x} after glEGLImageTargetTexture2DOES", gl_err);
            return;
        }

        self._egl_image = Some(egl_image);
    }

    unsafe fn set_tex_params() {
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
    }
}

impl Drop for GlTexture {
    fn drop(&mut self) {
        // Delete the GL texture first so the driver can release internal refs.
        unsafe {
            gl::DeleteTextures(1, &self.id);
        }
        // _egl_image drops here → eglDestroyImageKHR.
    }
}
