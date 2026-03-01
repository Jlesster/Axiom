// session.rs — TTY acquisition, VT switching, and session pause/resume.
//
// Smithay's libseat session gives us:
//   - seat device open/close (DRM, evdev)
//   - VT switch notifications (pause/resume)
//
// Flow:
//   1. `Session::new()` opens the seat via libseat (no root needed if user is in `seat` group)
//   2. calloop source drives the session — it fires `SessionEvent::PauseDevice` on VT-away
//      and `SessionEvent::ActivateDevice` on VT-return
//   3. When paused: disable all DRM surfaces, drop GPU device lease
//   4. When resumed: re-open devices, re-enable surfaces
//   5. `Ctrl+Alt+Fn` keys are intercepted in input.rs and routed here via `TwmAction::SwitchVt`

use smithay::backend::{
    drm::DrmNode,
    session::{
        libseat::{LibSeatSession, LibSeatSessionNotifier},
        Event as SessionEvent, Session,
    },
};
use smithay::reexports::calloop::LoopHandle;

use crate::state::Trixie;

// ── Session init ──────────────────────────────────────────────────────────────

/// Open a libseat session and register its notifier with calloop.
///
/// Returns the `(session, notifier)` pair. The session is stored in
/// `Trixie::session`; the notifier is inserted into the event loop here.
pub fn init_session(
    handle: &LoopHandle<'static, Trixie>,
) -> Result<LibSeatSession, Box<dyn std::error::Error>> {
    let (session, notifier) = LibSeatSession::new()?;

    handle.insert_source(notifier, |event, _, state| {
        handle_session_event(event, state);
    })?;

    tracing::info!("Session opened: seat={}", session.seat());

    Ok(session)
}

// ── VT switch request ─────────────────────────────────────────────────────────

/// Called from input.rs when Ctrl+Alt+Fn is pressed.
/// Asks the session layer to switch to VT `n`.
pub fn switch_vt(state: &mut Trixie, vt: i32) {
    tracing::info!("Switching to VT {vt}");
    if let Err(e) = state.session.change_vt(vt) {
        tracing::warn!("VT switch to {vt} failed: {e}");
    }
}

// ── Session event handler ─────────────────────────────────────────────────────

fn handle_session_event(event: SessionEvent, state: &mut Trixie) {
    match event {
        SessionEvent::PauseSession => {
            tracing::info!("Session paused — suspending all DRM surfaces");
            pause_all_outputs(state);
        }
        SessionEvent::ActivateSession => {
            tracing::info!("Session resumed — re-activating DRM surfaces");
            resume_all_outputs(state);
        }
    }
}

// ── Pause: called when we leave our VT ───────────────────────────────────────

fn pause_all_outputs(state: &mut Trixie) {
    // Mark every surface as not pending a frame so the timer doesn't
    // try to render into a paused device.
    for backend in state.backends.values_mut() {
        backend.drm.pause();
        for surface in backend.surfaces.values_mut() {
            surface.pending_frame = false;
        }
    }

    tracing::debug!("All outputs paused");
}

// ── Resume: called when we return to our VT ──────────────────────────────────

fn resume_all_outputs(state: &mut Trixie) {
    // Collect nodes first to avoid borrow issues.
    let nodes: Vec<DrmNode> = state.backends.keys().copied().collect();

    for node in nodes {
        let backend = state.backends.get_mut(&node).unwrap();

        if let Err(e) = backend.drm.activate(false) {
            tracing::warn!("DRM activate failed for {node}: {e}");
            continue;
        }

        // Re-enable every surface (the render timer will pick them up).
        let crtcs: Vec<_> = backend.surfaces.keys().copied().collect();
        for crtc in crtcs {
            if let Some(surface) = backend.surfaces.get_mut(&crtc) {
                // Reset the compositor surface state so it does a full redraw.
                surface.pending_frame = false;
                // next_frame_time is already in the past so the timer fires immediately.
                surface.next_frame_time =
                    std::time::Instant::now() + std::time::Duration::from_millis(16);
            }
        }
    }

    // Kick a render on all surfaces so we repaint straight away.
    let nodes: Vec<DrmNode> = state.backends.keys().copied().collect();
    for node in nodes {
        let crtcs: Vec<_> = state
            .backends
            .get(&node)
            .map(|b| b.surfaces.keys().copied().collect())
            .unwrap_or_default();
        for crtc in crtcs {
            state.render_surface(node, crtc);
        }
    }

    tracing::debug!("All outputs resumed");
}
