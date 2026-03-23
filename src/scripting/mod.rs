// src/scripting/mod.rs

pub mod lua_api;
pub mod signals;

use anyhow::Result;
use mlua::prelude::*;
use std::path::{Path, PathBuf};

use crate::wm::WmState;
use lua_api::ActionQueue;

// ── ScriptEngine ──────────────────────────────────────────────────────────────

pub struct ScriptEngine {
    pub lua: Lua,
    pub queue: ActionQueue,
    rc_path: PathBuf,
    config_dir: PathBuf,
}

impl ScriptEngine {
    pub fn new(config_dir: &Path, wm: &WmState) -> Result<Self> {
        let lua = Lua::new();

        // Set up package.path so require() works from the config dir.
        // Drop the LuaTable borrow before we move `lua` into Self.
        {
            let package: LuaTable = lua.globals().get("package")?;
            let existing: String = package.get("path").unwrap_or_default();
            let config_str = config_dir.to_string_lossy();
            package.set(
                "path",
                format!("{dir}/?.lua;{dir}/?/init.lua;{existing}", dir = config_str,),
            )?;
            package.set("cpath", "")?;
            // `package` (and its borrow of `lua`) dropped here.
        }

        signals::install_globals(&lua)?;
        let queue =
            lua_api::install(&lua, wm).map_err(|e| anyhow::anyhow!("Lua API install: {e}"))?;

        let rc_path = config_dir.join("axiom.rc.lua");
        Ok(Self {
            lua,
            queue,
            rc_path,
            config_dir: config_dir.to_path_buf(),
        })
    }

    /// Execute the RC file.  All require() calls resolve relative to the
    /// config directory automatically via package.path set in new().
    pub fn run_rc(&self, wm: &mut WmState) -> Result<()> {
        if !self.rc_path.exists() {
            tracing::info!("No axiom.rc.lua at {:?} — using defaults", self.rc_path);
            return Ok(());
        }
        let src = std::fs::read_to_string(&self.rc_path)?;
        self.lua
            .load(&src)
            .set_name("axiom.rc.lua")
            .exec()
            .map_err(|e| {
                // Give the user a nice error with line numbers.
                anyhow::anyhow!("RC error: {e}")
            })?;
        self.apply_rules(wm);
        Ok(())
    }

    pub fn reload(&self, wm: &mut WmState) -> Result<()> {
        // Clear existing rules before re-running so they don't accumulate.
        if let Ok(tbl) = self.lua.named_registry_value::<LuaTable>("axiom_rules") {
            // Set all entries to nil.
            for i in 1..=tbl.raw_len() {
                let _ = tbl.set(i, LuaValue::Nil);
            }
        }
        if let Ok(tbl) = self.lua.named_registry_value::<LuaTable>("axiom_keybinds") {
            // Clear keybinds so reload doesn't double-register.
            for pair in tbl.clone().pairs::<LuaValue, LuaValue>() {
                if let Ok((k, _)) = pair {
                    let _ = tbl.set(k, LuaValue::Nil);
                }
            }
        }
        self.run_rc(wm)
    }

    pub fn fire_keybind(&self, combo: &str) -> Result<()> {
        lua_api::fire_keybind(&self.lua, combo)
            .map_err(|e| anyhow::anyhow!("keybind '{combo}': {e}"))
    }

    pub fn drain(&mut self, state: &mut crate::state::Axiom) {
        lua_api::drain(&self.queue, state);
    }

    /// Emit a client signal (client.open, client.focus, etc.)
    pub fn emit_client(&self, event: &str, win: &crate::wm::Window) {
        signals::emit_client_signal(&self.lua, event, win);
    }

    /// Emit a signal with no arguments (compositor.ready, etc.)
    pub fn emit_bare(&self, event: &str) {
        lua_api::emit_bare(&self.lua, event);
    }

    /// Called each main-loop tick.
    pub fn tick(&self, wm: &WmState) {
        signals::update_client_list(&self.lua, wm);
        signals::update_screen_count(&self.lua, wm.monitors.len());
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn apply_rules(&self, wm: &mut WmState) {
        use crate::wm::rules::{Effect, Matcher, WindowRule};
        let Ok(tbl) = self.lua.named_registry_value::<LuaTable>("axiom_rules") else {
            return;
        };
        wm.config.rules.clear();
        for pair in tbl.clone().pairs::<LuaValue, LuaTable>() {
            let Ok((_, rule)) = pair else { continue };
            let Ok(m) = rule.get::<_, LuaTable>("match") else {
                continue;
            };
            let Ok(act) = rule.get::<_, LuaTable>("action") else {
                continue;
            };

            let matcher = if let (Ok(a), Ok(t)) =
                (m.get::<_, String>("app_id"), m.get::<_, String>("title"))
            {
                Matcher::Both {
                    app_id: a,
                    title: t,
                }
            } else if let Ok(a) = m.get::<_, String>("app_id") {
                Matcher::AppId(a)
            } else if let Ok(t) = m.get::<_, String>("title") {
                Matcher::Title(t)
            } else {
                Matcher::Always
            };

            let mut effects = Vec::new();
            if act.get::<_, bool>("float").unwrap_or(false) {
                effects.push(Effect::Float);
            }
            if let Ok(ws) = act.get::<_, usize>("workspace") {
                effects.push(Effect::Workspace(ws.saturating_sub(1)));
            }
            if !effects.is_empty() {
                wm.config.rules.push(WindowRule { matcher, effects });
            }
        }
    }
}
