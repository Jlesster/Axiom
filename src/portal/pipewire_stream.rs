// src/portal/pipewire_stream.rs — PipeWire screencast stub.
//
// libspa 0.8.0 has a binding bug against some system PipeWire headers
// (spa_pod_builder field layout mismatch).  We stub this out so the rest
// of the compositor compiles and runs.  Screen sharing via the D-Bus portal
// will advertise the stream but return node_id=0 until this is re-enabled.
//
// To re-enable: once `libspa` ≥ 0.8.1 is released (or pin pipewire = "0.7"),
// remove this file and restore the real implementation.

use anyhow::Result;

pub struct PipeWireStream;

impl PipeWireStream {
    pub fn new() -> Result<Self> {
        anyhow::bail!("PipeWire screencast disabled (libspa 0.8.0 header mismatch)")
    }

    pub fn node_id(&self) -> u32 {
        0
    }

    pub fn push_frame(&self, _pixels: &[u8], _w: u32, _h: u32) {}
}
