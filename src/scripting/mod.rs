mod lua_api;

use anyhow::Result;
use mlua::prelude::*;
use std::sync::{Arc, Mutex};

use crate::wm::{Layout, WindowId, WmState};

// ── Action queue ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LuaAction {
    Spawn(String),
    FocusId(WindowId),
    CloseId(WindowId),
    MoveToWorkspace(WindowId, usize),
    SwitchWorkspace(usize),
    SetLayout(usize, Layout),
    SetFloat(WindowId, bool),
    SetFullscreen(WindowId, bool),
    FocusDirection(u8),
    CycleFocus(i32),
    IncMaster,
    DecMaster,
    Reload,
    Quit,
}

pub type ActionQueue = Arc<Mutex<Vec<LuaAction>>>;

// ── ScriptEngine ─────────────────────────────────────────────────────────────

pub struct ScriptEngine {
    pub lua: Lua,
    pub actions: ActionQueue,
}

impl ScriptEngine {
    pub fn new() -> Result<Self> {
        let lua = Lua::new();
        let actions: ActionQueue = Arc::new(Mutex::new(Vec::new()));
        Ok(Self { lua, actions })
    }

    pub fn load_config(&mut self, wm: &mut WmState) -> Result<()> {
        lua_api::install(&self.lua, self.actions.clone(), wm)?;
        let path = config_path();
        if !path.exists() {
            tracing::info!("No config at {path:?}, using defaults");
            return Ok(());
        }
        let code = std::fs::read_to_string(&path)?;
        self.lua
            .load(&code)
            .set_name(path.to_string_lossy())
            .exec()?;
        tracing::info!("Loaded config from {path:?}");
        Ok(())
    }

    pub fn emit_client_open(&self, wm: &WmState, id: WindowId) {
        self.emit_client("client.open", wm, id);
    }
    pub fn emit_client_close(&self, wm: &WmState, id: WindowId) {
        self.emit_client("client.close", wm, id);
    }
    pub fn emit_client_focus(&self, wm: &WmState, id: WindowId) {
        self.emit_client("client.focus", wm, id);
    }

    pub fn emit_bare(&self, event: &str) {
        let _ = (|| -> LuaResult<()> {
            let signals: LuaTable = self.lua.globals().get("_axiom_signals")?;
            if let Ok(f) = signals.get::<_, LuaFunction>(event) {
                // ← get::<_, V>
                f.call::<_, ()>(())?;
            }
            Ok(())
        })();
    }

    pub fn fire_keybind(&self, combo: &str) -> bool {
        (|| -> LuaResult<bool> {
            let keybinds: LuaTable = self.lua.globals().get("_axiom_keybinds")?;
            if let Ok(f) = keybinds.get::<_, LuaFunction>(combo) {
                // ← get::<_, V>
                f.call::<_, ()>(())?;
                Ok(true)
            } else {
                Ok(false)
            }
        })()
        .unwrap_or(false)
    }

    fn emit_client(&self, event: &str, wm: &WmState, id: WindowId) {
        let Some(win) = wm.windows.get(&id) else {
            return;
        };
        let _ = (|| -> LuaResult<()> {
            let tbl = self.lua.create_table()?;
            tbl.set("id", win.id)?;
            tbl.set("app_id", win.app_id.clone())?;
            tbl.set("title", win.title.clone())?;
            tbl.set("floating", win.floating)?;
            tbl.set("fullscreen", win.fullscreen)?;
            tbl.set("x", win.rect.x)?;
            tbl.set("y", win.rect.y)?;
            tbl.set("width", win.rect.w)?;
            tbl.set("height", win.rect.h)?;
            let signals: LuaTable = self.lua.globals().get("_axiom_signals")?;
            if let Ok(f) = signals.get::<_, LuaFunction>(event) {
                // ← get::<_, V>
                f.call::<_, ()>(tbl)?;
            }
            Ok(())
        })();
    }
}

fn config_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .unwrap_or_else(|_| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
    std::path::PathBuf::from(base).join("axiom/axiom.rc.lua")
}

pub fn apply_action(action: LuaAction, state: &mut crate::state::Axiom) {
    match action {
        LuaAction::Spawn(cmd) => {
            spawn(&cmd);
        }
        LuaAction::SwitchWorkspace(ws) => {
            state.wm.switch_workspace(ws);
            state.needs_redraw = true;
        }
        LuaAction::MoveToWorkspace(id, ws) => {
            state.wm.move_to_workspace(id, ws);
            state.needs_redraw = true;
        }
        LuaAction::SetLayout(ws_idx, layout) => {
            if let Some(ws) = state.wm.workspaces.get_mut(ws_idx) {
                ws.layout = layout;
            }
            state.wm.reflow();
            state.needs_redraw = true;
        }
        LuaAction::SetFloat(id, on) => {
            if let Some(w) = state.wm.windows.get_mut(&id) {
                w.floating = on;
            }
            state.wm.reflow();
            state.needs_redraw = true;
        }
        LuaAction::SetFullscreen(id, on) => {
            state.wm.fullscreen_window(id, on);
            state.needs_redraw = true;
        }
        LuaAction::FocusDirection(dir) => {
            state.wm.focus_direction(dir);
            state.sync_keyboard_focus();
            state.needs_redraw = true;
        }
        LuaAction::CycleFocus(delta) => {
            state.wm.cycle_focus(delta);
            state.sync_keyboard_focus();
            state.needs_redraw = true;
        }
        LuaAction::FocusId(id) => {
            state.wm.focus_window(id);
            state.sync_keyboard_focus();
            state.needs_redraw = true;
        }
        LuaAction::CloseId(id) => {
            state.close_window(id);
        }
        LuaAction::IncMaster => {
            state.wm.inc_master();
            state.needs_redraw = true;
        }
        LuaAction::DecMaster => {
            state.wm.dec_master();
            state.needs_redraw = true;
        }
        LuaAction::Reload => {
            let _ = state.reload_config();
        }
        LuaAction::Quit => {
            state.loop_signal.stop();
        }
    }
}

pub fn spawn(cmd: &str) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if let Some((prog, args)) = parts.split_first() {
        let _ = std::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}
