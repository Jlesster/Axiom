// chrome/mod.rs — ChromeApp: the trixui App that renders all compositor chrome.

pub mod bar;
pub mod panes;

use trixui::{
    app::{App, Cmd, Event, Frame},
    layout::Rect as PixRect,
    PixColor,
};

use crate::config::Config;
use crate::twm::TwmSnapshot;
use bar::BarModuleSet;

// ── ChromeMsg ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum ChromeMsg {
    Snapshot(TwmSnapshot),
    ConfigReloaded(std::sync::Arc<Config>),
}

// ── ChromeApp ─────────────────────────────────────────────────────────────────

pub struct ChromeApp {
    snap: TwmSnapshot,
    config: std::sync::Arc<Config>,
    modules: BarModuleSet,
    title_h: u32,
}

impl ChromeApp {
    pub fn new(config: std::sync::Arc<Config>) -> Self {
        let modules = BarModuleSet::from_config(&config);
        Self {
            snap: TwmSnapshot::default(),
            title_h: 0,
            modules,
            config,
        }
    }

    fn rebuild_modules(&mut self) {
        self.modules = BarModuleSet::from_config(&self.config);
    }
}

impl App for ChromeApp {
    type Message = ChromeMsg;

    fn update(&mut self, event: Event<ChromeMsg>) -> Cmd<ChromeMsg> {
        match event {
            Event::Message(ChromeMsg::Snapshot(snap)) => {
                self.snap = snap;
            }
            Event::Message(ChromeMsg::ConfigReloaded(cfg)) => {
                self.config = cfg;
                self.rebuild_modules();
            }
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        // Grab all immutable frame values before taking the mutable canvas borrow.
        let cell_w = frame.cell_w();
        let cell_h = frame.cell_h();
        let frame_bar_area = frame.bar_area();
        let canvas = frame.canvas();

        let snap = &self.snap;
        let colors = &self.config.colors;

        // ── Bar background only ───────────────────────────────────────────────
        // Do NOT fill the content area where Wayland windows live — a full-screen
        // fill causes a visible flash on every frame as it briefly paints over
        // window surfaces before they are composited on top.
        if frame_bar_area.w > 0 && frame_bar_area.h > 0 {
            canvas.fill(
                frame_bar_area.x,
                frame_bar_area.y,
                frame_bar_area.w,
                frame_bar_area.h,
                PixColor::rgba(
                    colors.pane_bg.r,
                    colors.pane_bg.g,
                    colors.pane_bg.b,
                    colors.pane_bg.a,
                ),
            );
        }

        // ── Active workspace panes ────────────────────────────────────────────
        if let Some(ws) = snap.workspaces.get(snap.active_ws) {
            panes::draw_workspace_panes(
                canvas,
                &ws.panes,
                snap.focused_id,
                colors,
                self.config.border_width,
                self.title_h,
                cell_w,
                cell_h,
            );
        }

        // ── Bar ───────────────────────────────────────────────────────────────
        // frame_bar_area is now exact pixels (no cell-rounding), so its height
        // matches the configured bar height and text centering is correct.
        if frame_bar_area.w > 0 && frame_bar_area.h > 0 {
            let pix_rect = PixRect::new(
                frame_bar_area.x,
                frame_bar_area.y,
                frame_bar_area.w,
                frame_bar_area.h,
            );
            self.modules.draw(
                canvas,
                pix_rect,
                snap,
                colors,
                &self.config.bar,
                cell_w,
                cell_h,
            );
        }
    }
}
