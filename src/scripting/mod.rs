// src/scripting/mod.rs

pub mod lua_api;
pub mod signals;

use anyhow::Result;
use mlua::prelude::*;
use std::path::{Path, PathBuf};

use crate::wm::WmState;
use lua_api::ActionQueue;

pub struct ScriptEngine {
    pub lua: Lua,
    pub queue: ActionQueue,
    rc_path: PathBuf,
}

impl ScriptEngine {
    pub fn new(config_dir: &Path, wm: &WmState) -> Result<Self> {
        let lua = Lua::new();

        {
            let package: LuaTable = lua.globals().get("package")?;
            let existing: String = package.get("path").unwrap_or_default();
            let dir = config_dir.to_string_lossy();
            package.set("path", format!("{dir}/?.lua;{dir}/?/init.lua;{existing}"))?;
            package.set("cpath", "")?;
        }

        signals::install_globals(&lua)?;
        let queue =
            lua_api::install(&lua, wm).map_err(|e| anyhow::anyhow!("Lua API install: {e}"))?;

        let rc_path = config_dir.join("axiom.rc.lua");
        write_default_rc_if_missing(&rc_path);

        Ok(Self {
            lua,
            queue,
            rc_path,
        })
    }

    /// Execute axiom.rc.lua. All require() calls resolve relative to the
    /// config directory automatically via the package.path set above.
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
            .map_err(|e| anyhow::anyhow!("RC error: {e}"))?;
        self.apply_rules(wm);
        Ok(())
    }

    pub fn reload(&self, wm: &mut WmState) -> Result<()> {
        // Clear rules and keybinds so reload doesn't accumulate duplicates.
        if let Ok(tbl) = self.lua.named_registry_value::<LuaTable>("axiom_rules") {
            for i in 1..=tbl.raw_len() {
                let _ = tbl.set(i, LuaValue::Nil);
            }
        }
        if let Ok(tbl) = self.lua.named_registry_value::<LuaTable>("axiom_keybinds") {
            for pair in tbl.clone().pairs::<LuaValue, LuaValue>() {
                if let Ok((k, _)) = pair {
                    let _ = tbl.set(k, LuaValue::Nil);
                }
            }
        }
        self.run_rc(wm)
    }

    pub fn fire_keybind(&self, combo: &str) -> Result<bool> {
        lua_api::fire_keybind(&self.lua, combo)
            .map_err(|e| anyhow::anyhow!("keybind '{combo}': {e}"))
    }

    pub fn drain(&mut self, state: &mut crate::state::Axiom) {
        lua_api::drain(&self.queue, state);
    }

    pub fn emit_client(&self, event: &str, win: &crate::wm::Window) {
        signals::emit_client_signal(&self.lua, event, win);
    }

    pub fn emit_bare(&self, event: &str) {
        lua_api::emit_bare(&self.lua, event);
    }

    pub fn tick(&self, wm: &WmState) {
        signals::update_client_list(&self.lua, wm);
        signals::update_screen_count(&self.lua, wm.monitors.len());
    }

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

// ── Default RC ────────────────────────────────────────────────────────────────

fn write_default_rc_if_missing(path: &Path) {
    if path.exists() {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Use a high-fence raw string so the Lua code's double-quoted strings
    // don't accidentally close the Rust raw string literal.
    let rc = r######"-- axiom.rc.lua  (auto-generated defaults — edit freely)

axiom.set {
    border_width    = 2,
    gap             = 6,
    bar_height      = 28,
    workspaces      = 9,
    border_active   = "#7dc4e4",
    border_inactive = "#45475a",
}

local mod = "super"

-- Terminal
axiom.key(mod.."+Return", function() axiom.spawn("foot")      end)

-- Launcher
axiom.key(mod.."+d", function() axiom.spawn("fuzzel")         end)
axiom.key(mod.."+p", function() axiom.spawn("rofi -show run") end)

-- Window control
axiom.key(mod.."+q",       function() axiom.close()      end)
axiom.key(mod.."+shift+q", function() axiom.quit()       end)
axiom.key(mod.."+f",       function() axiom.fullscreen() end)
axiom.key(mod.."+shift+f", function() axiom.float()      end)

-- Focus (vim + arrow keys)
axiom.key(mod.."+h",     function() axiom.focus("left")  end)
axiom.key(mod.."+l",     function() axiom.focus("right") end)
axiom.key(mod.."+k",     function() axiom.focus("up")    end)
axiom.key(mod.."+j",     function() axiom.focus("down")  end)
axiom.key(mod.."+Left",  function() axiom.focus("left")  end)
axiom.key(mod.."+Right", function() axiom.focus("right") end)
axiom.key(mod.."+Up",    function() axiom.focus("up")    end)
axiom.key(mod.."+Down",  function() axiom.focus("down")  end)

-- Cycle focus
axiom.key(mod.."+Tab",       function() axiom.cycle(1)  end)
axiom.key(mod.."+shift+Tab", function() axiom.cycle(-1) end)

-- Move window in layout
axiom.key(mod.."+shift+h", function() axiom.move("left")  end)
axiom.key(mod.."+shift+l", function() axiom.move("right") end)
axiom.key(mod.."+shift+k", function() axiom.move("up")    end)
axiom.key(mod.."+shift+j", function() axiom.move("down")  end)

-- Layouts
axiom.key(mod.."+space",       function() axiom.layout(axiom.ws(), "tile")    end)
axiom.key(mod.."+shift+space", function() axiom.layout(axiom.ws(), "monocle") end)
axiom.key(mod.."+b",           function() axiom.layout(axiom.ws(), "bsp")     end)
axiom.key(mod.."+equal",       function() axiom.inc_master() end)
axiom.key(mod.."+minus",       function() axiom.dec_master() end)

-- Workspaces 1-9
for i = 1, 9 do
    local n = i
    axiom.key(mod.."+"..n,       function() axiom.workspace(n) end)
    axiom.key(mod.."+shift+"..n, function() axiom.send(n)      end)
end

-- Reload / screenshot
axiom.key(mod.."+shift+r", function() axiom.reload() end)
axiom.key("Print",         function() axiom.spawn("grim ~/screenshot.png") end)
axiom.key("shift+Print",   function() axiom.spawn("grim -g \"$(slurp)\" ~/screenshot.png") end)
"######;
    let _ = std::fs::write(path, rc);
    tracing::info!("wrote default axiom.rc.lua to {:?}", path);
}
