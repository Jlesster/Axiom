// src/backend/egl.rs
// Changes from previous version:
//   1. eglGetPlatformDisplayEXT returns *mut c_void; must cast to egl::Display
//      via egl_lib.get_display() or a direct transmute — khronos-egl 6 exposes
//      Display as a newtype over *mut c_void, so transmute is safe here.
//   2. create_window_surface is unsafe in khronos-egl 6; wrap calls in unsafe{}.

use anyhow::{bail, Context, Result};
use gbm::Surface as GbmSurface;
use khronos_egl as egl;
use std::ffi::c_void;

type EglGetPlatformDisplayEXT = unsafe extern "C" fn(
    platform: egl::Enum,
    native_display: *mut c_void,
    attrib_list: *const egl::Int,
) -> *mut c_void; // ← returns raw pointer, not egl::Display

const EGL_PLATFORM_GBM_KHR: egl::Enum = 0x31D7;

pub struct EglSurface(pub egl::Surface);

pub struct EglContext {
    pub display: egl::Display,
    pub context: egl::Context,
    pub config: egl::Config,
    egl_lib: egl::DynamicInstance<egl::EGL1_5>,
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

        // The extension returns *mut c_void; khronos-egl's Display is
        // repr(transparent) over *mut c_void so transmute is sound.
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
        })
    }

    pub fn create_window_surface<U>(&self, gbm_surf: &GbmSurface<U>) -> Result<EglSurface> {
        use gbm::AsRaw;
        let raw = unsafe { gbm_surf.as_raw() } as egl::NativeWindowType;
        // create_window_surface is unsafe in khronos-egl 6.
        let surf = unsafe {
            self.egl_lib
                .create_window_surface(self.display, self.config, raw, None)
                .context("eglCreateWindowSurface")?
        };
        Ok(EglSurface(surf))
    }

    /// Raw-pointer variant used by backend/mod.rs OutputSurface init.
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
