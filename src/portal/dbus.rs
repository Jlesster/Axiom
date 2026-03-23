// src/portal/dbus.rs — D-Bus portal implementation via zbus.

use anyhow::Result;
use std::collections::HashMap;
use tokio::sync::mpsc;
use zbus::{connection, interface, zvariant::OwnedValue};

use super::{PortalEvent, PortalRequest};
// PipeWireStream is stubbed — import the type but don't construct it.
use crate::portal::pipewire_stream::PipeWireStream;

// ── Screenshot portal ─────────────────────────────────────────────────────────

struct ScreenshotPortal {
    req_tx: mpsc::Sender<PortalRequest>,
}

#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl ScreenshotPortal {
    async fn screenshot(
        &self,
        _handle: zbus::zvariant::ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        // Generate a path in $XDG_PICTURES_DIR or /tmp.
        let path = screenshot_path();
        let _ = self
            .req_tx
            .send(PortalRequest::Screenshot { path: path.clone() })
            .await;
        // Return response=0 (success) with the URI.
        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        let uri = format!("file://{path}");
        results.insert(
            "uri".to_string(),
            OwnedValue::try_from(zbus::zvariant::Value::from(uri)).unwrap(),
        );
        Ok((0, results))
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

// ── ScreenCast portal ─────────────────────────────────────────────────────────

struct ScreenCastPortal {
    req_tx: mpsc::Sender<PortalRequest>,
    pw: std::sync::Arc<std::sync::Mutex<Option<PipeWireStream>>>,
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastPortal {
    async fn create_session(
        &self,
        _handle: zbus::zvariant::ObjectPath<'_>,
        _session_handle: zbus::zvariant::ObjectPath<'_>,
        _app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        Ok((0, HashMap::new()))
    }

    async fn select_sources(
        &self,
        _handle: zbus::zvariant::ObjectPath<'_>,
        _session_handle: zbus::zvariant::ObjectPath<'_>,
        _app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        Ok((0, HashMap::new()))
    }

    async fn start(
        &self,
        _handle: zbus::zvariant::ObjectPath<'_>,
        _session_handle: zbus::zvariant::ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let _ = self
            .req_tx
            .send(PortalRequest::StartCast {
                output_name: "output-0".to_string(),
            })
            .await;

        // Get the PipeWire node id if stream is ready.
        let node_id = self
            .pw
            .lock()
            .unwrap()
            .as_ref()
            .map(|pw| pw.node_id())
            .unwrap_or(0);

        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        // streams = array of (node_id, dict)
        let streams_val = zbus::zvariant::Value::from(vec![zbus::zvariant::Value::from((
            node_id,
            HashMap::<String, OwnedValue>::new(),
        ))]);
        results.insert(
            "streams".to_string(),
            OwnedValue::try_from(streams_val).unwrap(),
        );
        Ok((0, results))
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        1
    } // MONITOR=1

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        1
    } // HIDDEN=1

    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }
}

// ── Run ───────────────────────────────────────────────────────────────────────

pub async fn run_portal(
    mut event_rx: mpsc::Receiver<PortalEvent>,
    req_tx: mpsc::Sender<PortalRequest>,
) -> Result<()> {
    // PipeWireStream::new() will fail (stub), so we hold None.
    let pw: std::sync::Arc<std::sync::Mutex<Option<PipeWireStream>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    let _conn = connection::Builder::session()?
        .name("org.freedesktop.impl.portal.desktop.axiom")?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            ScreenshotPortal {
                req_tx: req_tx.clone(),
            },
        )?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            ScreenCastPortal {
                req_tx: req_tx.clone(),
                pw: pw.clone(),
            },
        )?
        .build()
        .await?;

    tracing::info!("xdg-desktop-portal-axiom D-Bus service started");

    // Forward compositor frames to the PipeWire stream.
    while let Some(event) = event_rx.recv().await {
        match event {
            PortalEvent::Frame {
                pixels,
                width,
                height,
            } => {
                if let Some(ref mut stream) = *pw.lock().unwrap() {
                    stream.push_frame(&pixels, width, height);
                }
            }
            PortalEvent::Shutdown => break,
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn screenshot_path() -> String {
    let dir = std::env::var("XDG_PICTURES_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{home}/Pictures")
    });
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{dir}/axiom-screenshot-{ts}.png")
}
