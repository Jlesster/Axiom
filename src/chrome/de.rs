//! chrome/de.rs — TrixieDE: the compositor's DeApp implementation.
//!
//! Replaces ChromeApp / SmithayApp. The compositor holds a `DePipeline<TrixieDE>`
//! and calls `de.set_windows(...)` + `de.render_frame()` each vblank.

use trixui::app::{Cmd, Event};
use trixui::pipelines::de::{DeApp, DeFrame, WindowInfo};
use trixui::renderer::Theme;

use crate::config::Config;
use crate::twm::TwmSnapshot;

// ── Messages ──────────────────────────────────────────────────────────────────

/// Messages the compositor can send into the DE pipeline.
#[derive(Debug, Clone)]
pub enum DeMsg {
    /// Full TWM snapshot (workspaces, layout name, focused title).
    Snapshot(TwmSnapshot),
    /// Clock tick — pre-formatted time string.
    ClockTick(String),
}

// ── TrixieDE ──────────────────────────────────────────────────────────────────

pub struct TrixieDE {
    pub snapshot: TwmSnapshot,
    pub clock: String,
    pub config: Config,
}

impl TrixieDE {
    pub fn new(config: Config) -> Self {
        Self {
            snapshot: TwmSnapshot::default(),
            clock: String::new(),
            config,
        }
    }
}

impl DeApp for TrixieDE {
    type Message = DeMsg;

    fn update(&mut self, event: Event<DeMsg>) -> Cmd<DeMsg> {
        if let Event::Message(msg) = event {
            match msg {
                DeMsg::Snapshot(s) => self.snapshot = s,
                DeMsg::ClockTick(t) => self.clock = t,
            }
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut DeFrame) {
        let snap = &self.snapshot;

        // ── Status bar ────────────────────────────────────────────────────────
        frame
            .bar()
            .left(|b| {
                let mut b = b;
                for ws in &snap.workspaces {
                    b = b.workspace_state(ws.index, ws.active, ws.occupied);
                }
                b.separator().layout(&snap.layout_name)
            })
            .center(|b| {
                if snap.focused_title.is_empty() {
                    b
                } else {
                    b.text(&snap.focused_title)
                }
            })
            .right(|b| b.clock(&self.clock))
            .finish();

        // ── Pane decorations ──────────────────────────────────────────────────
        // Windows are pre-filtered by the compositor (closing panes excluded).
        for win in frame.windows() {
            use trixui::PaneOpts;
            frame.pane(
                win.rect,
                PaneOpts::new(&win.title)
                    .focused(win.focused)
                    .border_w(self.config.border_width)
                    .corner_radius(self.config.corner_radius),
            );
        }
    }

    fn theme(&self) -> Theme {
        self.config.theme.clone()
    }
}
