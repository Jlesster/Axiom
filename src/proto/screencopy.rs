// src/proto/screencopy.rs — zwlr-screencopy-v1

use std::sync::{Arc, Mutex};

use wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};
use wayland_server::{
    protocol::{wl_buffer::WlBuffer, wl_shm},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::Axiom;

// ── Frame request data ────────────────────────────────────────────────────────

pub struct ScreencopyFrame {
    pub output_id: u32,
    pub overlay_cursor: bool,
    pub pending_buffer: Option<WlBuffer>,
    pub done: bool,
}

// ── Global ────────────────────────────────────────────────────────────────────

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => {
                let out_id = output.id().protocol_id();
                let frame_obj = init.init(
                    frame,
                    Arc::new(Mutex::new(ScreencopyFrame {
                        output_id: out_id,
                        overlay_cursor: overlay_cursor != 0,
                        pending_buffer: None,
                        done: false,
                    })),
                );
                let (w, h) = state
                    .outputs
                    .iter()
                    .find(|o| o.wl_id == out_id)
                    .map(|o| (o.width, o.height))
                    .unwrap_or((1920, 1080));
                let stride = w * 4;
                frame_obj.buffer(wl_shm::Format::Xrgb8888, w, h, stride);
                if frame_obj.version() >= 3 {
                    frame_obj.buffer_done();
                }
            }

            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x: _,
                y: _,
                width,
                height,
            } => {
                let out_id = output.id().protocol_id();
                let frame_obj = init.init(
                    frame,
                    Arc::new(Mutex::new(ScreencopyFrame {
                        output_id: out_id,
                        overlay_cursor: overlay_cursor != 0,
                        pending_buffer: None,
                        done: false,
                    })),
                );
                let stride = width as u32 * 4;
                frame_obj.buffer(
                    wl_shm::Format::Xrgb8888,
                    width as u32,
                    height as u32,
                    stride,
                );
                if frame_obj.version() >= 3 {
                    frame_obj.buffer_done();
                }
            }

            zwlr_screencopy_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── Per-frame dispatch ────────────────────────────────────────────────────────

impl Dispatch<ZwlrScreencopyFrameV1, Arc<Mutex<ScreencopyFrame>>> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame_obj: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &Arc<Mutex<ScreencopyFrame>>,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => {
                let mut frame = data.lock().unwrap();
                if frame.done {
                    frame_obj.failed();
                    return;
                }
                frame.done = true;
                drop(frame);

                if let Err(e) = do_copy(state, frame_obj, &buffer, data) {
                    tracing::warn!("screencopy: {e}");
                    frame_obj.failed();
                }
            }

            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => {
                let mut frame = data.lock().unwrap();
                if frame.done {
                    frame_obj.failed();
                    return;
                }
                frame.done = true;
                drop(frame);

                if let Err(e) = do_copy(state, frame_obj, &buffer, data) {
                    tracing::warn!("screencopy copy_with_damage: {e}");
                    frame_obj.failed();
                } else {
                    let frame = data.lock().unwrap();
                    let (w, h) = state
                        .outputs
                        .iter()
                        .find(|o| o.wl_id == frame.output_id)
                        .map(|o| (o.width, o.height))
                        .unwrap_or((1920, 1080));
                    frame_obj.damage(0, 0, w, h);
                }
            }

            zwlr_screencopy_frame_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ── GL readback ───────────────────────────────────────────────────────────────

fn do_copy(
    state: &mut Axiom,
    frame_obj: &ZwlrScreencopyFrameV1,
    buffer: &WlBuffer,
    data: &Arc<Mutex<ScreencopyFrame>>,
) -> anyhow::Result<()> {
    use crate::proto::shm::ShmBuffer;

    let shm = buffer
        .data::<ShmBuffer>()
        .ok_or_else(|| anyhow::anyhow!("screencopy: buffer is not SHM (DMA-BUF capture NYI)"))?;

    let (w, h) = (shm.width, shm.height);
    let out_id = data.lock().unwrap().output_id;

    // Make the relevant output context current.
    let surf_ptr = {
        let out = state
            .outputs
            .iter()
            .find(|o| o.wl_id == out_id)
            .ok_or_else(|| anyhow::anyhow!("output not found"))?;
        &out.render_surf as *const crate::backend::OutputSurface
    };
    let surf = unsafe { &*surf_ptr };
    surf.make_current(&state.backend.egl)?;

    // Re-render so the capture is always up to date.
    state.render.render_output(
        &state.wm,
        &state.anim,
        &state.input,
        &state.outputs,
        &state.layer_surfaces,
        surf,
        state
            .outputs
            .iter()
            .position(|o| o.wl_id == out_id)
            .unwrap_or(0),
    );

    // Map the SHM pool and write pixels at the correct offset within it.
    //
    // FIX: previously `offset` was passed directly as the mmap file offset,
    // which is wrong when the buffer does not start at byte 0 of the pool.
    // The correct approach is to map the entire pool from offset 0 and then
    // advance the pointer by the buffer's offset within the pool.
    // mmap(2) requires the offset to be page-aligned; mapping from 0 always
    // satisfies that constraint.
    let pool = shm.pool.lock().unwrap();
    let pool_size = pool.size;
    let buf_offset = shm.offset as usize;
    let byte_len = (shm.stride * shm.height) as usize;

    if buf_offset + byte_len > pool_size {
        anyhow::bail!(
            "screencopy: buffer offset+size ({}) exceeds pool size ({})",
            buf_offset + byte_len,
            pool_size
        );
    }

    // Map the whole pool from offset 0.
    let map_ptr = unsafe {
        crate::sys::mmap(
            std::ptr::null_mut(),
            pool_size,
            crate::sys::PROT_READ | crate::sys::PROT_WRITE,
            crate::sys::MAP_SHARED,
            pool.fd_raw(),
            0, // always map from the start of the fd
        )
    };
    if map_ptr == crate::sys::MAP_FAILED {
        anyhow::bail!("screencopy mmap failed");
    }

    unsafe {
        // Advance by buf_offset to reach the start of this buffer within the pool.
        let pixel_ptr = (map_ptr as *mut u8).add(buf_offset) as *mut std::ffi::c_void;

        gl::ReadPixels(0, 0, w, h, gl::BGRA, gl::UNSIGNED_BYTE, pixel_ptr);

        // GL reads bottom-up; flip vertically in-place.
        let row_bytes = shm.stride as usize;
        let pixels = std::slice::from_raw_parts_mut(pixel_ptr as *mut u8, byte_len);
        let mut tmp = vec![0u8; row_bytes];
        for row in 0..(h as usize / 2) {
            let top = row * row_bytes;
            let bot = (h as usize - 1 - row) * row_bytes;
            tmp.copy_from_slice(&pixels[top..top + row_bytes]);
            pixels.copy_within(bot..bot + row_bytes, top);
            pixels[bot..bot + row_bytes].copy_from_slice(&tmp);
        }

        crate::sys::munmap(map_ptr, pool_size);
    }

    let now = state.now_ms();
    let tv_sec = now / 1000;
    let tv_nsec = (now % 1000) * 1_000_000;
    frame_obj.ready(tv_sec, tv_nsec, now);

    Ok(())
}
