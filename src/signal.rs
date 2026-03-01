// signal.rs — SIGTERM / SIGINT → clean compositor shutdown.
//
// Registers calloop signal sources so that Ctrl+C in the launching terminal
// (or `kill <pid>`) triggers an orderly exit rather than a hard crash that
// leaves the TTY in a broken state.
//
// Shutdown order:
//   1. Set `state.running = false`  (the main loop exits on the next iteration)
//   2. Wayland clients receive compositor-gone and self-terminate
//   3. DRM surfaces are dropped, releasing the DRM master
//   4. libseat session is closed, restoring the previous VT

use calloop::{
    signals::{Signal, Signals},
    LoopHandle,
};

use crate::state::Trixie;

/// Register SIGTERM and SIGINT handlers.
///
/// Call once during compositor startup, before `event_loop.run()`.
pub fn init_signals(
    handle: &LoopHandle<'static, Trixie>,
) -> Result<(), Box<dyn std::error::Error>> {
    let signals = Signals::new(&[Signal::SIGTERM, Signal::SIGINT])?;

    handle.insert_source(
        signals,
        |signal: calloop::signals::Event, _, state: &mut Trixie| {
            match signal.signal() {
                Signal::SIGTERM => tracing::info!("SIGTERM received — shutting down"),
                Signal::SIGINT => tracing::info!("SIGINT (Ctrl+C) received — shutting down"),
                _ => return,
            }
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        },
    )?;

    tracing::debug!("Signal handlers installed (SIGTERM, SIGINT)");
    Ok(())
}
