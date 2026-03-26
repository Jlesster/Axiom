use anyhow::Result;
use calloop::{LoopHandle, LoopSignal};
use std::sync::{atomic::AtomicBool, Arc};
use wayland_server::{Display, DisplayHandle};

use crate::backend::Backend;
use crate::input::InputState;
use crate::ipc::IpcServer;
use crate::render::RenderState;
use crate::scripting::ScriptEngine;
use crate::wm::{WindowId, WmState};
use crate::xwayland::XWayland;

pub struct Axiom {
    pub display: Display<Axiom>,
    pub dh: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Axiom>,
    pub loop_signal: LoopSignal,
    pub backend: Backend,
    pub render: RenderState,
    pub input: InputState,
    pub wm: WmState,
    pub script: ScriptEngine,
    pub ipc: IpcServer,
    pub needs_redraw: bool,
    pub running: Arc<AtomicBool>,
    /// XWayland instance — None if Xwayland binary not found.
    pub xwayland: Option<XWayland>,
}

impl Axiom {
    pub fn new(
        display: Display<Axiom>,
        loop_handle: LoopHandle<'static, Axiom>,
        loop_signal: LoopSignal,
        dh: &DisplayHandle,
    ) -> Result<Self> {
        let backend = Backend::init()?;
        let render = RenderState::new(&backend)?;
        let input = InputState::new()?;
        let wm = WmState::new();
        let script = ScriptEngine::new()?;
        let ipc = IpcServer::new(dh)?;

        crate::proto::register_globals(dh);

        let mut s = Self {
            display,
            dh: dh.clone(),
            loop_handle,
            loop_signal,
            backend,
            render,
            input,
            wm,
            script,
            ipc,
            needs_redraw: true,
            running: Arc::new(AtomicBool::new(true)),
            xwayland: None, // set in main.rs after socket is bound
        };

        let (w, h) = s.backend.output_size();
        s.wm.add_monitor(0, 0, w as i32, h as i32);
        Ok(s)
    }

    /// Returns raw fd for calloop registration.
    pub fn display_raw_fd(&mut self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.display.backend().poll_fd().as_raw_fd()
    }

    pub fn dispatch_clients(&mut self) -> Result<()> {
        // SAFETY: display and the rest of Axiom are disjoint fields.
        let state_ptr = self as *mut Axiom;
        unsafe {
            (*state_ptr).display.dispatch_clients(&mut *state_ptr)?;
        }
        Ok(())
    }

    pub fn flush_clients(&mut self) {
        self.display.flush_clients();
    }

    pub fn drain_actions(&mut self) {
        self.update_lua_state();
        let actions: Vec<_> = self.script.actions.lock().unwrap().drain(..).collect();
        for action in actions {
            if let crate::scripting::LuaAction::Spawn(ref s) = action {
                if let Some(json) = s.strip_prefix("__cfg__") {
                    if let Ok(cfg) = serde_json::from_str::<crate::wm::WmConfig>(json) {
                        self.wm.apply_config(cfg);
                        self.needs_redraw = true;
                    }
                    continue;
                }
            }
            crate::scripting::apply_action(action, self);
        }
    }

    fn update_lua_state(&self) {
        let lua = &self.script.lua;
        let _ = lua
            .globals()
            .set("_axiom_active_ws", self.wm.active_ws() + 1);

        let _ = (|| -> mlua::Result<()> {
            let clients = lua.create_table()?;
            for (i, (_, win)) in self.wm.windows.iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", win.id)?;
                t.set("app_id", win.app_id.clone())?;
                t.set("title", win.title.clone())?;
                t.set("floating", win.floating)?;
                t.set("fullscreen", win.fullscreen)?;
                t.set("x", win.rect.x)?;
                t.set("y", win.rect.y)?;
                t.set("width", win.rect.w)?;
                t.set("height", win.rect.h)?;
                t.set("workspace", win.workspace + 1)?;
                clients.raw_set(i + 1, t)?;
            }
            lua.globals().set("_axiom_clients", clients)?;

            if let Some(id) = self.wm.focused_window() {
                if let Some(win) = self.wm.windows.get(&id) {
                    let t = lua.create_table()?;
                    t.set("id", win.id)?;
                    t.set("app_id", win.app_id.clone())?;
                    t.set("title", win.title.clone())?;
                    t.set("floating", win.floating)?;
                    t.set("fullscreen", win.fullscreen)?;
                    t.set("x", win.rect.x)?;
                    t.set("y", win.rect.y)?;
                    t.set("width", win.rect.w)?;
                    t.set("height", win.rect.h)?;
                    lua.globals().set("_axiom_focused", t)?;
                }
            } else {
                lua.globals().set("_axiom_focused", mlua::Value::Nil)?;
            }
            Ok(())
        })();
    }

    pub fn sync_keyboard_focus(&mut self) {
        if let Some(id) = self.wm.focused_window() {
            self.input.set_keyboard_focus(id, &self.dh, &self.wm);
            self.script.emit_client_focus(&self.wm, id);
        } else {
            // No focused window — clear keyboard focus
            self.input.clear_keyboard_focus(&self.dh);
        }
    }

    pub fn close_window(&mut self, id: WindowId) {
        self.script.emit_client_close(&self.wm, id);
        crate::proto::close_toplevel(&self.dh, id);
        self.wm.remove_window(id);
        self.render.remove_window(id);
        self.sync_keyboard_focus();
        self.needs_redraw = true;
    }

    pub fn send_configure_focused(&mut self) {
        if let Some(id) = self.wm.focused_window() {
            crate::proto::configure_toplevel(self, id);
        }
    }

    pub fn reload_config(&mut self) -> Result<()> {
        tracing::info!("Reloading config");
        self.script = ScriptEngine::new()?;
        self.script.load_config(&mut self.wm)?;
        // Re-send configures to all windows so they resize to match new gaps/borders
        crate::proto::configure_all(self);
        self.needs_redraw = true;
        Ok(())
    }

    pub fn on_surface_commit(&mut self, surface: &wayland_server::protocol::wl_surface::WlSurface) {
        crate::proto::handle_surface_commit(self, surface);
    }
}
