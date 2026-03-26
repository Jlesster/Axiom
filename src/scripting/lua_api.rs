use anyhow::Result;
use mlua::prelude::*;

use crate::scripting::{ActionQueue, LuaAction};
use crate::wm::{Layout, WmConfig, WmState};

pub fn install(lua: &Lua, queue: ActionQueue, _wm: &mut WmState) -> Result<()> {
    lua.globals().set("_axiom_keybinds", lua.create_table()?)?;
    lua.globals().set("_axiom_signals", lua.create_table()?)?;
    lua.globals().set("_axiom_rules", lua.create_table()?)?;
    lua.globals().set("_axiom_clients", lua.create_table()?)?;
    lua.globals().set("_axiom_screens", lua.create_table()?)?;
    lua.globals().set("_axiom_active_ws", 1usize)?;
    lua.globals().set("_axiom_focused", LuaValue::Nil)?;

    let axiom = lua.create_table()?;

    // ── axiom.set { ... } ────────────────────────────────────────────────────
    // We queue a Spawn action with a magic prefix to carry the config through.
    // A cleaner approach would be a dedicated LuaAction::SetConfig — but we
    // keep Spawn for now and detect the prefix in apply_action.
    {
        let q = queue.clone();
        axiom.set(
            "set",
            lua.create_function(move |_lua, tbl: LuaTable| {
                let mut cfg = WmConfig::default();

                if let Ok(v) = tbl.get::<_, u32>("border_width") {
                    cfg.border_w = v;
                }
                if let Ok(v) = tbl.get::<_, u32>("gap") {
                    cfg.gap = v;
                }
                if let Ok(v) = tbl.get::<_, u32>("outer_gap") {
                    cfg.outer_gap = v;
                }
                if let Ok(v) = tbl.get::<_, u32>("bar_height") {
                    cfg.bar_height = v;
                }
                if let Ok(v) = tbl.get::<_, usize>("workspaces") {
                    cfg.workspaces_count = v;
                }
                if let Ok(v) = tbl.get::<_, bool>("bar_at_bottom") {
                    cfg.bar_at_bottom = v;
                }

                // hex_to_rgba returns [f32;4] — convert to [u8;4] before storing
                if let Ok(v) = tbl.get::<_, String>("border_active") {
                    cfg.active_border = f32x4_to_u8(hex_to_rgba(&v));
                }
                if let Ok(v) = tbl.get::<_, String>("border_inactive") {
                    cfg.inactive_border = f32x4_to_u8(hex_to_rgba(&v));
                }
                if let Ok(v) = tbl.get::<_, String>("bar_bg") {
                    cfg.bar_bg = f32x4_to_u8(hex_to_rgba(&v));
                }

                let json = serde_json::to_string(&cfg).unwrap_or_default();
                q.lock()
                    .unwrap()
                    .push(LuaAction::Spawn(format!("__cfg__{json}")));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.spawn(cmd) ─────────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "spawn",
            lua.create_function(move |_, cmd: String| {
                q.lock().unwrap().push(LuaAction::Spawn(cmd));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.workspace(n) ───────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "workspace",
            lua.create_function(move |_, n: usize| {
                q.lock()
                    .unwrap()
                    .push(LuaAction::SwitchWorkspace(n.saturating_sub(1)));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.send(n) ────────────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "send",
            lua.create_function(move |lua, n: usize| {
                if let Some(id) = get_focused_id(lua) {
                    q.lock()
                        .unwrap()
                        .push(LuaAction::MoveToWorkspace(id, n.saturating_sub(1)));
                }
                Ok(())
            })?,
        )?;
    }

    // ── axiom.layout(ws, name) ───────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "layout",
            lua.create_function(move |_, (ws, name): (usize, String)| {
                q.lock().unwrap().push(LuaAction::SetLayout(
                    ws.saturating_sub(1),
                    Layout::from_str(&name),
                ));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.focus(dir) ─────────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "focus",
            lua.create_function(move |_, dir: String| {
                q.lock()
                    .unwrap()
                    .push(LuaAction::FocusDirection(dir_to_u8(&dir)));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.cycle(delta) ───────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "cycle",
            lua.create_function(move |_, delta: i32| {
                q.lock().unwrap().push(LuaAction::CycleFocus(delta));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.move(dir) ──────────────────────────────────────────────────────
    axiom.set("move", lua.create_function(|_, _dir: String| Ok(()))?)?;

    // ── axiom.close() ────────────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "close",
            lua.create_function(move |lua, ()| {
                if let Some(id) = get_focused_id(lua) {
                    q.lock().unwrap().push(LuaAction::CloseId(id));
                }
                Ok(())
            })?,
        )?;
    }

    // ── axiom.float() ────────────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "float",
            lua.create_function(move |lua, ()| {
                if let Some(id) = get_focused_id(lua) {
                    q.lock().unwrap().push(LuaAction::SetFloat(id, true));
                }
                Ok(())
            })?,
        )?;
    }

    // ── axiom.fullscreen() ───────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "fullscreen",
            lua.create_function(move |lua, ()| {
                if let Some(id) = get_focused_id(lua) {
                    q.lock().unwrap().push(LuaAction::SetFullscreen(id, true));
                }
                Ok(())
            })?,
        )?;
    }

    // ── axiom.inc_master / dec_master ────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "inc_master",
            lua.create_function(move |_, ()| {
                q.lock().unwrap().push(LuaAction::IncMaster);
                Ok(())
            })?,
        )?;
    }
    {
        let q = queue.clone();
        axiom.set(
            "dec_master",
            lua.create_function(move |_, ()| {
                q.lock().unwrap().push(LuaAction::DecMaster);
                Ok(())
            })?,
        )?;
    }

    // ── axiom.reload / quit ──────────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "reload",
            lua.create_function(move |_, ()| {
                q.lock().unwrap().push(LuaAction::Reload);
                Ok(())
            })?,
        )?;
    }
    {
        let q = queue.clone();
        axiom.set(
            "quit",
            lua.create_function(move |_, ()| {
                q.lock().unwrap().push(LuaAction::Quit);
                Ok(())
            })?,
        )?;
    }

    // ── axiom.key / unkey ────────────────────────────────────────────────────
    axiom.set(
        "key",
        lua.create_function(|lua, (combo, func): (String, LuaFunction)| {
            let kb: LuaTable = lua.globals().get("_axiom_keybinds")?;
            kb.set(combo, func)?;
            Ok(())
        })?,
    )?;
    axiom.set(
        "unkey",
        lua.create_function(|lua, combo: String| {
            let kb: LuaTable = lua.globals().get("_axiom_keybinds")?;
            kb.set(combo, LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    // ── axiom.on / off ───────────────────────────────────────────────────────
    axiom.set(
        "on",
        lua.create_function(|lua, (event, func): (String, LuaFunction)| {
            let sig: LuaTable = lua.globals().get("_axiom_signals")?;
            sig.set(event, func)?;
            Ok(())
        })?,
    )?;
    axiom.set(
        "off",
        lua.create_function(|lua, event: String| {
            let sig: LuaTable = lua.globals().get("_axiom_signals")?;
            sig.set(event, LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    // ── axiom.ws() ───────────────────────────────────────────────────────────
    axiom.set(
        "ws",
        lua.create_function(|lua, ()| {
            let ws: usize = lua.globals().get("_axiom_active_ws")?;
            Ok(ws)
        })?,
    )?;

    // ── axiom.clients() / focused() / screens() ──────────────────────────────
    axiom.set(
        "clients",
        lua.create_function(|lua, ()| {
            let t: LuaTable = lua.globals().get("_axiom_clients")?;
            Ok(t)
        })?,
    )?;
    axiom.set(
        "focused",
        lua.create_function(|lua, ()| {
            let v: LuaValue = lua.globals().get("_axiom_focused")?;
            Ok(v)
        })?,
    )?;
    axiom.set(
        "screens",
        lua.create_function(|lua, ()| {
            let t: LuaTable = lua.globals().get("_axiom_screens")?;
            Ok(t)
        })?,
    )?;

    // ── axiom.notify(msg [,ms]) ──────────────────────────────────────────────
    {
        let q = queue.clone();
        axiom.set(
            "notify",
            lua.create_function(move |_, (msg, _ms): (String, Option<u32>)| {
                tracing::info!("[notify] {msg}");
                q.lock()
                    .unwrap()
                    .push(LuaAction::Spawn(format!("notify-send Axiom \"{msg}\"")));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.rule { ... } ───────────────────────────────────────────────────
    axiom.set(
        "rule",
        lua.create_function(|lua, tbl: LuaTable| {
            let rules: LuaTable = lua.globals().get("_axiom_rules")?;
            let len = rules.raw_len();
            rules.raw_set(len + 1, tbl)?;
            Ok(())
        })?,
    )?;

    lua.globals().set("axiom", axiom)?;
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

pub fn get_focused_id(lua: &Lua) -> Option<u32> {
    let v: LuaValue = lua.globals().get("_axiom_focused").ok()?;
    if let LuaValue::Table(t) = v {
        t.get::<_, u32>("id").ok() // ← get::<_, V>(key) form
    } else {
        None
    }
}

fn dir_to_u8(s: &str) -> u8 {
    match s {
        "left" => 0,
        "right" => 1,
        "up" => 2,
        "down" => 3,
        _ => 0,
    }
}

fn f32x4_to_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8,
        (c[3] * 255.0) as u8,
    ]
}

pub fn hex_to_rgba(hex: &str) -> [f32; 4] {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return [1.0; 4];
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255) as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255) as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255) as f32 / 255.0;
    [r, g, b, 1.0]
}
