use anyhow::{Context, Result};
use gbm::AsRaw; // ← brings as_raw() into scope for GbmDevice and Surface
use khronos_egl as egl;

pub struct EglContext {
    lib: egl::DynamicInstance<egl::EGL1_4>,
    pub display: egl::Display,
    pub context: egl::Context,
    pub config: egl::Config,
}

impl EglContext {
    pub fn new(gbm: &super::gbm::GbmDevice) -> Result<Self> {
        let lib = unsafe { egl::DynamicInstance::<egl::EGL1_4>::load_required() }
            .context("load libEGL")?;

        let native_display = gbm.inner.as_raw() as egl::NativeDisplayType;
        let display = unsafe { lib.get_display(native_display) }.context("eglGetDisplay")?;

        lib.initialize(display).context("eglInitialize")?;
        lib.bind_api(egl::OPENGL_ES_API).context("eglBindAPI")?;

        let attribs = [
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            0,
            egl::DEPTH_SIZE,
            0,
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES2_BIT,
            egl::NONE,
        ];
        let config = lib
            .choose_first_config(display, &attribs)
            .context("eglChooseConfig")?
            .context("no EGL config")?;

        let ctx_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
        let context = lib
            .create_context(display, config, None, &ctx_attribs)
            .context("eglCreateContext")?;

        Ok(Self {
            lib,
            display,
            context,
            config,
        })
    }

    pub fn create_window_surface(&self, gbm_surface: &gbm::Surface<()>) -> Result<egl::Surface> {
        let surface = unsafe {
            self.lib.create_window_surface(
                self.display,
                self.config,
                gbm_surface.as_raw() as egl::NativeWindowType,
                None,
            )
        }
        .context("eglCreateWindowSurface")?;
        Ok(surface)
    }

    pub fn make_current(&self, surface: &egl::Surface) -> Result<()> {
        self.lib
            .make_current(
                self.display,
                Some(*surface),
                Some(*surface),
                Some(self.context),
            )
            .context("eglMakeCurrent")
    }

    pub fn swap_buffers(&self, surface: &egl::Surface) -> Result<()> {
        self.lib
            .swap_buffers(self.display, *surface)
            .context("eglSwapBuffers")
    }

    pub fn load_gl(&self) {
        gl::load_with(|s| {
            self.lib
                .get_proc_address(s)
                .map(|f| f as *const _)
                .unwrap_or(std::ptr::null())
        });
    }
}
