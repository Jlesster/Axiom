// backend.rs — GPU initialisation and output setup.

use std::{
    os::unix::io::{FromRawFd, IntoRawFd},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDevice,
            DrmDeviceFd, DrmEvent, DrmNode,
        },
        egl::{EGLContext, EGLDisplay},
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        session::Session,
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        drm::control::{connector, crtc, Device as DrmControlDevice, ModeTypeFlags},
        wayland_server::DisplayHandle,
    },
    utils::{DeviceFd, Size, Transform},
};

use crate::{
    chrome::ChromeApp,
    config::BarPosition,
    state::{BackendData, SurfaceData, Trixie},
};

// ── add_gpu ───────────────────────────────────────────────────────────────────

pub fn add_gpu(
    state: &mut Trixie,
    dh: &DisplayHandle,
    node: DrmNode,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let owned_fd = state.session.open(
        path,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOCTTY,
    )?;
    let drm_fd = DrmDeviceFd::new(unsafe { DeviceFd::from_raw_fd(owned_fd.into_raw_fd()) });

    let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), true)?;
    let gbm = GbmDevice::new(drm_fd.clone())?;
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let ctx = EGLContext::new(&egl)?;
    let renderer = unsafe { GlesRenderer::new(ctx)? };

    gl::load_with(|s| {
        let sym = std::ffi::CString::new(s).unwrap();
        unsafe { smithay::backend::egl::ffi::egl::GetProcAddress(sym.as_ptr()) as *const _ }
    });

    if state.ui.is_none() {
        if let Err(e) = unsafe { renderer.egl_context().make_current() } {
            tracing::warn!("make_current for trixui init: {e}");
        } else {
            init_chrome(state);
        }
    }

    // VBlank events are the sole render driver. Each vblank calls frame_finish,
    // which clears pending_frame and schedules the next render via insert_idle.
    state
        .handle
        .insert_source(drm_notifier, move |event, _, state| {
            if let DrmEvent::VBlank(crtc) = event {
                state.frame_finish(node, crtc);
            }
        })
        .unwrap();

    let mut backend = BackendData {
        surfaces: Default::default(),
        renderer,
        gbm,
        drm,
        drm_node: node,
    };

    let res = backend.drm.resource_handles()?;
    let connectors: Vec<_> = res
        .connectors()
        .iter()
        .filter_map(|&h| backend.drm.get_connector(h, false).ok())
        .filter(|c| c.state() == connector::State::Connected)
        .collect();

    for conn in connectors {
        let conn_name = format!("{}-{}", conn.interface().as_str(), conn.interface_id());

        // Look up this connector in the config.
        let mon_cfg = state
            .config
            .monitors
            .iter()
            .find(|m| m.name == conn_name)
            .cloned();

        let mode = if let Some(ref mc) = mon_cfg {
            // 1. Exact match: resolution + refresh.
            conn.modes()
                .iter()
                .find(|m| {
                    let (mw, mh) = m.size();
                    mw as u32 == mc.width
                        && mh as u32 == mc.height
                        && m.vrefresh() as u32 == mc.refresh
                })
                // 2. Resolution match, any refresh — pick highest refresh.
                .or_else(|| {
                    conn.modes()
                        .iter()
                        .filter(|m| {
                            let (mw, mh) = m.size();
                            mw as u32 == mc.width && mh as u32 == mc.height
                        })
                        .max_by_key(|m| m.vrefresh())
                })
                // 3. Kernel preferred.
                .or_else(|| {
                    conn.modes()
                        .iter()
                        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                })
                // 4. Whatever is first.
                .or_else(|| conn.modes().first())
                .copied()
        } else {
            tracing::warn!(
                "No monitor config found for connector '{conn_name}' — using kernel preferred mode. \
                 Add a `monitor {conn_name} {{ }}` block to monitors.conf to configure it."
            );
            conn.modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .or_else(|| conn.modes().first())
                .copied()
        };

        let Some(mode) = mode else {
            tracing::warn!("Connector '{conn_name}' has no usable modes, skipping");
            continue;
        };

        tracing::info!(
            "Connector '{conn_name}': selected mode {}x{}@{}Hz{}",
            mode.size().0,
            mode.size().1,
            mode.vrefresh(),
            if mon_cfg.is_some() {
                " (from config)"
            } else {
                " (kernel preferred)"
            },
        );

        let crtc = res
            .crtcs()
            .iter()
            .copied()
            .find(|&c| !backend.surfaces.contains_key(&c));
        let Some(crtc) = crtc else {
            tracing::warn!("No free CRTC available for connector '{conn_name}'");
            continue;
        };

        if let Err(e) = add_output(state, dh, &mut backend, node, conn.handle(), crtc, mode) {
            tracing::warn!("Output setup failed for '{conn_name}': {e}");
        }
    }

    state.backends.insert(node, backend);
    Ok(())
}

// ── init_chrome ───────────────────────────────────────────────────────────────

fn init_chrome(state: &mut Trixie) {
    tracing::info!("init_chrome: loading font {:?}", state.config.font_path);

    let font_bytes: &'static [u8] = match std::fs::read(&state.config.font_path) {
        Ok(b) => Box::leak(b.into_boxed_slice()),
        Err(e) => {
            tracing::warn!("Could not load font: {e} — using embedded fallback font");
            // Fall through with no font_bytes — SmithayApp uses its own
            // embedded Iosevka when constructed via the builder with no
            // explicit .font() call.
            &[]
        }
    };

    let chrome_app = ChromeApp::new(std::sync::Arc::clone(&state.config));

    // SmithayApp::new(app, vp_w, vp_h) — viewport is 0×0 here;
    // add_output calls ui.resize(pw, ph) with the real DRM dimensions
    // immediately after, before any rendering occurs.
    let result = if font_bytes.is_empty() {
        // No font on disk — use the embedded fallback.
        trixui::smithay::SmithayApp::new(chrome_app, 0, 0)
    } else {
        trixui::smithay::SmithayApp::builder(chrome_app)
            .viewport(0, 0)
            .font(font_bytes, state.config.font_size)
            .build()
    };

    match result {
        Ok(ui) => {
            state.ui = Some(ui);
            tracing::info!("Chrome init OK");
        }
        Err(e) => {
            tracing::warn!("SmithayApp init failed: {e}");
        }
    }
}

// ── add_output ────────────────────────────────────────────────────────────────

pub fn add_output(
    state: &mut Trixie,
    dh: &DisplayHandle,
    backend: &mut BackendData,
    node: DrmNode,
    connector: connector::Handle,
    crtc: crtc::Handle,
    drm_mode: smithay::reexports::drm::control::Mode,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = drm_mode.size();
    let (pw, ph) = (w as u32, h as u32);
    let hz = drm_mode.vrefresh() as u64;

    let wl_mode = Mode {
        size: (w as i32, h as i32).into(),
        refresh: hz as i32 * 1000,
    };

    let output = Output::new(
        format!("{node}-{crtc:?}"),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Trixie".into(),
            model: "DRM".into(),
        },
    );
    output.create_global::<Trixie>(dh);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(wl_mode);
    state.space.map_output(&output, (0, 0));

    let config_bar_h = state.config.bar.height;
    let at_bottom = state.config.bar.position == BarPosition::Bottom;

    let actual_bar_h = if let Some(ui) = &mut state.ui {
        ui.resize(pw, ph);
        ui.set_bar_height_px(config_bar_h);

        // Mirror trixui's own rounding: ceil(bar_h_px / cell_h) * cell_h
        let cell_h = ui.line_h();
        let rounded = if cell_h == 0 {
            config_bar_h
        } else {
            ((config_bar_h + cell_h - 1) / cell_h) * cell_h
        };
        tracing::info!(
            "trixui resized to {pw}×{ph}, config bar={config_bar_h}px, \
             cell_h={cell_h}px, actual bar={rounded}px"
        );
        rounded
    } else {
        config_bar_h
    };

    // TWM gets the rounded value so its content_rect matches trixui exactly.
    state.twm.resize(pw, ph);
    state.twm.set_bar_height(actual_bar_h, at_bottom);

    tracing::info!(
        "Output {node}/{crtc:?}: {pw}×{ph}@{hz}Hz, bar={actual_bar_h}px (config={config_bar_h}px)"
    );

    let frame_duration = Duration::from_nanos(1_000_000_000 / hz.max(1));

    let compositor = DrmCompositor::new(
        &output,
        backend.drm.create_surface(crtc, drm_mode, &[connector])?,
        None,
        GbmAllocator::new(
            backend.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(backend.gbm.clone(), Some(node)),
        [Fourcc::Argb8888, Fourcc::Xrgb8888].iter().copied(),
        backend
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .clone(),
        Size::<u32, smithay::utils::Buffer>::from((64, 64)),
        None::<GbmDevice<DrmDeviceFd>>,
    )?;

    backend.surfaces.insert(
        crtc,
        SurfaceData {
            output: output.clone(),
            compositor,
            damage_tracker: OutputDamageTracker::from_output(&output),
            // next_frame_time is no longer used as a gate — pending_frame is
            // the sole guard. Set to now so the first render fires immediately.
            next_frame_time: Instant::now(),
            pending_frame: false,
            frame_duration,
        },
    );

    // Kick the first render. After that, vblank → frame_finish → insert_idle
    // forms the self-sustaining render loop with no repeating timer needed.
    state
        .handle
        .insert_idle(move |s| s.render_surface(node, crtc));

    Ok(())
}
