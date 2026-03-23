// src/portal/mod.rs — xdg-desktop-portal-axiom
//
// Implements the D-Bus interfaces required by xdg-desktop-portal so that
// browsers (screen share), OBS (PipeWire source), and GNOME screenshot
// tools work out of the box.
//
// We run the portal in a separate Tokio task so D-Bus I/O never blocks
// the compositor main loop.  Communication back to the compositor is via
// a pair of channels: the portal sends capture requests, the compositor
// fulfills them by doing a GL readback and sending the pixel data back.
//
// Interfaces implemented:
//   org.freedesktop.impl.portal.Screenshot
//   org.freedesktop.impl.portal.ScreenCast   (PipeWire stream)
//
// For Screenshot we just shell out to the zwlr_screencopy_v1 path we
// already have and save a PNG via the `image` crate.
// For ScreenCast we create a PipeWire stream and push frames into it.

use anyhow::Result;
use tokio::sync::mpsc;

pub mod dbus;
pub mod pipewire_stream; // stub — real impl requires libspa fix

// ── Messages compositor → portal ─────────────────────────────────────────────

#[derive(Debug)]
pub enum PortalEvent {
    /// A new frame is ready; contains BGRA pixels, width, height.
    Frame {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// Compositor is shutting down.
    Shutdown,
}

// ── Messages portal → compositor ─────────────────────────────────────────────

#[derive(Debug)]
pub enum PortalRequest {
    /// Take a screenshot and save it to `path`.
    Screenshot { path: String },
    /// Start streaming output `output_name` via PipeWire node `node_id`.
    StartCast { output_name: String },
    /// Stop the current stream.
    StopCast,
}

// ── Handle held by compositor ─────────────────────────────────────────────────

pub struct PortalHandle {
    pub tx: mpsc::Sender<PortalEvent>,
    pub rx: mpsc::Receiver<PortalRequest>,
}

// ── Spawn the portal task ─────────────────────────────────────────────────────

pub fn spawn_portal() -> Result<PortalHandle> {
    let (event_tx, event_rx) = mpsc::channel::<PortalEvent>(8);
    let (req_tx, req_rx) = mpsc::channel::<PortalRequest>(8);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("portal tokio runtime");
        rt.block_on(async move {
            if let Err(e) = dbus::run_portal(event_rx, req_tx).await {
                tracing::warn!("xdg-desktop-portal backend: {e}");
            }
        });
    });

    Ok(PortalHandle {
        tx: event_tx,
        rx: req_rx,
    })
}
