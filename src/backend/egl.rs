// src/backend/egl.rs

use anyhow::{bail, Context, Result};
use gbm::Surface as GbmSurface;
use khronos_egl as egl;
use std::ffi::c_void;
use std::os::unix::io::BorrowedFd;

type EglGetPlatformDisplayEXT = unsafe extern "C" fn(
    platform: egl::Enum,
    native_display: *mut c_void,
    attrib_list: *const egl::Int,
) -> *mut c_void;

// Raw fn pointer type for eglGetError — not exposed by the dynamic instance.
type EglGetError = unsafe extern "C" fn() -> egl::Int;

const EGL_PLATFORM_GBM_KHR: egl::Enum = 0x31D7;

// ── EGL DMA-BUF extension constants ──────────────────────────────────────────
// From EGL_EXT_image_dma_buf_import and EGL_EXT_image_dma_buf_import_modifiers.
// These are not exposed by khronos-egl, so we define them manually.

const EGL_LINUX_DMA_BUF_EXT: egl::Enum = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: egl::Int = 0x3271;

const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Int = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Int = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Int = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Int = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Int = 0x3444;

const EGL_DMA_BUF_PLANE1_FD_EXT: egl::Int = 0x3275;
const EGL_DMA_BUF_PLANE1_OFFSET_EXT: egl::Int = 0x3276;
const EGL_DMA_BUF_PLANE1_PITCH_EXT: egl::Int = 0x3277;
const EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT: egl::Int = 0x3445;
const EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT: egl::Int = 0x3446;

const EGL_DMA_BUF_PLANE2_FD_EXT: egl::Int = 0x3278;
const EGL_DMA_BUF_PLANE2_OFFSET_EXT: egl::Int = 0x3279;
const EGL_DMA_BUF_PLANE2_PITCH_EXT: egl::Int = 0x327A;
const EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT: egl::Int = 0x3447;
const EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT: egl::Int = 0x3448;

const EGL_DMA_BUF_PLANE3_FD_EXT: egl::Int = 0x3440;
const EGL_DMA_BUF_PLANE3_OFFSET_EXT: egl::Int = 0x3441;
const EGL_DMA_BUF_PLANE3_PITCH_EXT: egl::Int = 0x3442;
const EGL_DMA_BUF_PLANE3_MODIFIER_LO_EXT: egl::Int = 0x3449;
const EGL_DMA_BUF_PLANE3_MODIFIER_HI_EXT: egl::Int = 0x344A;

// Sentinel for "no modifier supplied" — both halves == 0xFFFFFFFF.
const DRM_FORMAT_MOD_INVALID_HI: u32 = 0xFFFF_FFFF;
const DRM_FORMAT_MOD_INVALID_LO: u32 = 0xFFFF_FFFF;

// ── EglImage ──────────────────────────────────────────────────────────────────

type EglCreateImageKHR =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type EglDestroyImageKHR = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;

/// Owned wrapper around an `EGLImageKHR`.
/// Calls `eglDestroyImageKHR` on drop — keep alive as long as the GL texture
/// that was created from it exists.
pub struct EglImage {
    display: *mut c_void, // raw EGLDisplay (same lifetime as EglContext)
    image: *mut c_void,   // EGLImageKHR
    destroy_fn: EglDestroyImageKHR,
}

// SAFETY: EGLImageKHR is an opaque pointer; we never share it across threads.
unsafe impl Send for EglImage {}
unsafe impl Sync for EglImage {}

impl EglImage {
    /// Raw `EGLImageKHR` pointer — pass to `glEGLImageTargetTexture2DOES`.
    pub fn raw(&self) -> *mut c_void {
        self.image
    }
}

impl Drop for EglImage {
    fn drop(&mut self) {
        if !self.image.is_null() {
            unsafe { (self.destroy_fn)(self.display, self.image) };
        }
    }
}

// ── Plane descriptor ─────────────────────────────────────────────────────────

/// One plane of a multi-planar DMA-BUF (caller fills this from protocol data).
pub struct DmaBufPlane {
    /// Borrowed fd — must stay valid for the duration of `import_dmabuf`.
    pub fd: i32,
    pub offset: u32,
    pub stride: u32,
    /// Upper 32 bits of the DRM format modifier.
    pub modifier_hi: u32,
    /// Lower 32 bits of the DRM format modifier.
    pub modifier_lo: u32,
}

// ── EglSurface / EglContext ───────────────────────────────────────────────────

pub struct EglSurface(pub egl::Surface);

pub struct EglContext {
    pub display: egl::Display,
    pub context: egl::Context,
    pub config: egl::Config,
    egl_lib: egl::DynamicInstance<egl::EGL1_5>,
    // Cached proc addresses for DMA-BUF import (resolved once in new()).
    create_image: Option<EglCreateImageKHR>,
    destroy_image: Option<EglDestroyImageKHR>,
    // Cached eglGetError for accurate error reporting in import_dmabuf.
    get_error: Option<EglGetError>,
}

impl EglContext {
    pub fn new(gbm: &gbm::Device<impl std::os::unix::io::AsFd>) -> Result<Self> {
        let lib = unsafe { libloading::Library::new("libEGL.so.1").context("load libEGL.so.1")? };
        let egl_lib = unsafe {
            egl::DynamicInstance::<egl::EGL1_5>::load_required_from(lib)
                .context("load EGL 1.5 symbols")?
        };

        let get_platform_display: EglGetPlatformDisplayEXT = unsafe {
            let raw = egl_lib
                .get_proc_address("eglGetPlatformDisplayEXT")
                .context("eglGetPlatformDisplayEXT not available")?;
            std::mem::transmute(raw)
        };

        let display: egl::Display = unsafe {
            use gbm::AsRaw;
            let raw_dpy = get_platform_display(
                EGL_PLATFORM_GBM_KHR,
                gbm.as_raw() as *mut c_void,
                std::ptr::null(),
            );
            if raw_dpy.is_null() {
                bail!("eglGetPlatformDisplayEXT returned null");
            }
            std::mem::transmute(raw_dpy)
        };

        egl_lib.initialize(display).context("eglInitialize")?;

        // ── check for required DMA-BUF extension ─────────────────────────────
        let exts = egl_lib
            .query_string(Some(display), egl::EXTENSIONS)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let has_dma_buf = exts.contains("EGL_EXT_image_dma_buf_import");
        if !has_dma_buf {
            tracing::warn!("EGL_EXT_image_dma_buf_import not available — DMA-BUF clients will fall back to SHM");
        }

        // Resolve proc addresses now; store None if unavailable.
        let create_image: Option<EglCreateImageKHR> = unsafe {
            egl_lib
                .get_proc_address("eglCreateImageKHR")
                .map(|p| std::mem::transmute(p))
        };
        let destroy_image: Option<EglDestroyImageKHR> = unsafe {
            egl_lib
                .get_proc_address("eglDestroyImageKHR")
                .map(|p| std::mem::transmute(p))
        };
        // Resolve eglGetError for accurate error codes in import_dmabuf.
        let get_error: Option<EglGetError> = unsafe {
            egl_lib
                .get_proc_address("eglGetError")
                .map(|p| std::mem::transmute(p))
        };

        let attribs = [
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::DEPTH_SIZE,
            0,
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_BIT,
            egl::NONE,
        ];

        egl_lib.bind_api(egl::OPENGL_API).context("eglBindAPI")?;

        let config = egl_lib
            .choose_first_config(display, &attribs)
            .context("eglChooseConfig")?
            .context("no matching EGL config")?;

        let ctx_attribs = [
            egl::CONTEXT_MAJOR_VERSION,
            3,
            egl::CONTEXT_MINOR_VERSION,
            3,
            egl::CONTEXT_OPENGL_PROFILE_MASK,
            egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
            egl::NONE,
        ];

        let context = egl_lib
            .create_context(display, config, None, &ctx_attribs)
            .context("eglCreateContext")?;

        gl::load_with(|sym| {
            egl_lib
                .get_proc_address(sym)
                .map(|p| p as *const _)
                .unwrap_or(std::ptr::null())
        });

        tracing::info!("EGL context created (OpenGL 3.3 core)");
        Ok(Self {
            display,
            context,
            config,
            egl_lib,
            create_image,
            destroy_image,
            get_error,
        })
    }

    // ── DMA-BUF import ────────────────────────────────────────────────────────

    /// Import a client DMA-BUF into an `EGLImageKHR`.
    ///
    /// The returned `EglImage` must be kept alive until the GL texture that
    /// was populated via `glEGLImageTargetTexture2DOES` is deleted.
    ///
    /// Returns `None` when:
    ///  - `eglCreateImageKHR` / `eglDestroyImageKHR` were not available at init, or
    ///  - the driver rejects the import (bad format, unsupported modifier, etc.)
    pub fn import_dmabuf(
        &self,
        width: u32,
        height: u32,
        format: u32, // DRM fourcc
        planes: &[DmaBufPlane],
    ) -> Option<EglImage> {
        let create = self.create_image?;
        let destroy = self.destroy_image?;

        if planes.is_empty() || planes.len() > 4 {
            tracing::warn!("import_dmabuf: invalid plane count {}", planes.len());
            return None;
        }

        // Per-plane attribute table — (fd, offset, pitch, mod_lo, mod_hi).
        const PLANE_ATTRS: [[egl::Int; 5]; 4] = [
            [
                EGL_DMA_BUF_PLANE0_FD_EXT,
                EGL_DMA_BUF_PLANE0_OFFSET_EXT,
                EGL_DMA_BUF_PLANE0_PITCH_EXT,
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            ],
            [
                EGL_DMA_BUF_PLANE1_FD_EXT,
                EGL_DMA_BUF_PLANE1_OFFSET_EXT,
                EGL_DMA_BUF_PLANE1_PITCH_EXT,
                EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT,
            ],
            [
                EGL_DMA_BUF_PLANE2_FD_EXT,
                EGL_DMA_BUF_PLANE2_OFFSET_EXT,
                EGL_DMA_BUF_PLANE2_PITCH_EXT,
                EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT,
            ],
            [
                EGL_DMA_BUF_PLANE3_FD_EXT,
                EGL_DMA_BUF_PLANE3_OFFSET_EXT,
                EGL_DMA_BUF_PLANE3_PITCH_EXT,
                EGL_DMA_BUF_PLANE3_MODIFIER_LO_EXT,
                EGL_DMA_BUF_PLANE3_MODIFIER_HI_EXT,
            ],
        ];

        // Build the attrib list.  Max size:
        //   3 (w/h/fourcc) + 4 planes × 7 attribs each + 1 terminator = 32 ints
        let mut attrs: Vec<egl::Int> = Vec::with_capacity(32);

        attrs.extend_from_slice(&[
            egl::WIDTH as egl::Int,
            width as egl::Int,
            egl::HEIGHT as egl::Int,
            height as egl::Int,
            EGL_LINUX_DRM_FOURCC_EXT,
            format as egl::Int,
        ]);

        for (i, plane) in planes.iter().enumerate() {
            let keys = &PLANE_ATTRS[i];
            attrs.extend_from_slice(&[
                keys[0],
                plane.fd,
                keys[1],
                plane.offset as egl::Int,
                keys[2],
                plane.stride as egl::Int,
            ]);

            // Only include modifier attribs if the client supplied a real one.
            let invalid = plane.modifier_hi == DRM_FORMAT_MOD_INVALID_HI
                && plane.modifier_lo == DRM_FORMAT_MOD_INVALID_LO;
            if !invalid {
                attrs.extend_from_slice(&[
                    keys[3],
                    plane.modifier_lo as egl::Int,
                    keys[4],
                    plane.modifier_hi as egl::Int,
                ]);
            }
        }

        attrs.push(egl::NONE as egl::Int); // terminator

        // Raw EGLDisplay pointer for the C call.
        let raw_dpy: *mut c_void = unsafe { std::mem::transmute(self.display) };

        let image = unsafe {
            create(
                raw_dpy,
                std::ptr::null_mut(), // EGL_NO_CONTEXT
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(), // buffer — must be NULL for dma-buf target
                attrs.as_ptr(),
            )
        };

        if image.is_null() {
            // Query the real EGL error code instead of the unrelated GL error.
            let egl_err = self.get_error.map(|f| unsafe { f() }).unwrap_or(0);
            tracing::error!(
                "eglCreateImageKHR returned NULL for format={:#010x} {}x{} ({} planes) EGL error={:#06x}",
                format,
                width,
                height,
                planes.len(),
                egl_err,
            );
            return None;
        }

        tracing::debug!(
            "eglCreateImageKHR OK: format={:#010x} {}x{} {} planes",
            format,
            width,
            height,
            planes.len()
        );

        Some(EglImage {
            display: raw_dpy,
            image,
            destroy_fn: destroy,
        })
    }

    // ── Window surface helpers ────────────────────────────────────────────────

    pub fn create_window_surface<U>(&self, gbm_surf: &GbmSurface<U>) -> Result<EglSurface> {
        use gbm::AsRaw;
        let raw = unsafe { gbm_surf.as_raw() } as egl::NativeWindowType;
        let surf = unsafe {
            self.egl_lib
                .create_window_surface(self.display, self.config, raw, None)
                .context("eglCreateWindowSurface")?
        };
        Ok(EglSurface(surf))
    }

    pub fn create_window_surface_raw(&self, raw: *mut c_void) -> Result<egl::Surface> {
        unsafe {
            self.egl_lib
                .create_window_surface(
                    self.display,
                    self.config,
                    raw as egl::NativeWindowType,
                    None,
                )
                .context("eglCreateWindowSurface (raw)")
        }
    }

    pub fn make_current(&self, surf: &EglSurface) -> Result<()> {
        self.egl_lib
            .make_current(self.display, Some(surf.0), Some(surf.0), Some(self.context))
            .context("eglMakeCurrent")
    }

    pub fn make_current_surfaceless(&self) -> Result<()> {
        self.egl_lib
            .make_current(self.display, None, None, Some(self.context))
            .context("eglMakeCurrent (surfaceless)")
    }

    pub fn swap_buffers(&self, surf: &EglSurface) -> Result<()> {
        self.egl_lib
            .swap_buffers(self.display, surf.0)
            .context("eglSwapBuffers")
    }
}

impl Drop for EglContext {
    fn drop(&mut self) {
        let _ = self.egl_lib.make_current(self.display, None, None, None);
        let _ = self.egl_lib.destroy_context(self.display, self.context);
        let _ = self.egl_lib.terminate(self.display);
    }
}
