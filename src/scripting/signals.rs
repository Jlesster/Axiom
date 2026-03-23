// src/scripting/signals.rs — AwesomeWM-compatible Lua signal system.

use crate::wm::{Window, WmState};
use mlua::prelude::*;

// ── Signal name constants ─────────────────────────────────────────────────────

pub const SIG_MANAGE: &str = "manage";
pub const SIG_UNMANAGE: &str = "unmanage";
pub const SIG_FOCUS: &str = "focus";
pub const SIG_UNFOCUS: &str = "unfocus";
pub const SIG_PROP_TITLE: &str = "property::title";
pub const SIG_PROP_FLOATING: &str = "property::floating";
pub const SIG_PROP_FULLSCREEN: &str = "property::fullscreen";
pub const SIG_PROP_URGENT: &str = "property::urgent";
pub const SIG_TAG_SELECTED: &str = "property::selected";

// ── Client → Lua table ────────────────────────────────────────────────────────

/// Build a Lua table snapshot of a window.  The lifetime `'lua` ties the
/// returned table to the `Lua` instance, not to `win`.
pub fn client_to_lua<'lua>(lua: &'lua Lua, win: &Window) -> LuaResult<LuaTable<'lua>> {
    let t = lua.create_table()?;
    t.set("id", win.id)?;
    t.set("app_id", win.app_id.clone())?;
    t.set("class", win.app_id.clone())?;
    t.set("instance", win.app_id.clone())?;
    t.set("name", win.title.clone())?;
    t.set("title", win.title.clone())?;
    t.set("floating", win.floating)?;
    t.set("fullscreen", win.fullscreen)?;
    t.set("maximized", win.maximized)?;
    t.set("x", win.rect.x)?;
    t.set("y", win.rect.y)?;
    t.set("width", win.rect.w)?;
    t.set("height", win.rect.h)?;
    t.set("tags", lua.create_table()?)?;
    Ok(t)
}

// ── install_globals ───────────────────────────────────────────────────────────

pub fn install_globals(lua: &Lua) -> LuaResult<()> {
    // ── client ────────────────────────────────────────────────────────────────
    lua.set_named_registry_value("axiom_client_signals", lua.create_table()?)?;

    let connect_signal = lua.create_function(|lua, (signal, func): (String, LuaFunction)| {
        let tbl: LuaTable = lua.named_registry_value("axiom_client_signals")?;
        let list: LuaTable = match tbl.get::<_, LuaValue>(signal.clone())? {
            LuaValue::Table(t) => t,
            _ => {
                let t = lua.create_table()?;
                tbl.set(signal.clone(), t.clone())?;
                t
            }
        };
        list.push(func)?;
        Ok(())
    })?;

    let disconnect_signal = lua.create_function(|lua, signal: String| {
        let tbl: LuaTable = lua.named_registry_value("axiom_client_signals")?;
        tbl.set(signal, LuaValue::Nil)?;
        Ok(())
    })?;

    let client = lua.create_table()?;
    client.set("connect_signal", connect_signal.clone())?;
    client.set("disconnect_signal", disconnect_signal.clone())?;
    client.set(
        "get",
        lua.create_function(|lua, ()| {
            Ok(
                match lua.named_registry_value::<LuaTable>("axiom_client_list") {
                    Ok(t) => t,
                    Err(_) => lua.create_table()?,
                },
            )
        })?,
    )?;
    lua.globals().set("client", client)?;

    // ── tag ───────────────────────────────────────────────────────────────────
    lua.set_named_registry_value("axiom_tag_signals", lua.create_table()?)?;

    let tag_connect = lua.create_function(|lua, (signal, func): (String, LuaFunction)| {
        let tbl: LuaTable = lua.named_registry_value("axiom_tag_signals")?;
        let list: LuaTable = match tbl.get::<_, LuaValue>(signal.clone())? {
            LuaValue::Table(t) => t,
            _ => {
                let t = lua.create_table()?;
                tbl.set(signal.clone(), t.clone())?;
                t
            }
        };
        list.push(func)?;
        Ok(())
    })?;

    let tag = lua.create_table()?;
    tag.set("connect_signal", tag_connect)?;
    tag.set("disconnect_signal", disconnect_signal)?;
    lua.globals().set("tag", tag)?;

    // ── screen ────────────────────────────────────────────────────────────────
    let screen = lua.create_table()?;
    screen.set(
        "count",
        lua.create_function(|lua, ()| {
            Ok(
                match lua.named_registry_value::<LuaInteger>("axiom_screen_count") {
                    Ok(n) => n,
                    Err(_) => 1,
                },
            )
        })?,
    )?;
    lua.globals().set("screen", screen)?;

    Ok(())
}

// ── Emit helpers ──────────────────────────────────────────────────────────────

pub fn emit_client_signal(lua: &Lua, signal: &str, win: &Window) {
    let tbl: LuaTable = match lua.named_registry_value("axiom_client_signals") {
        Ok(t) => t,
        Err(_) => return,
    };
    let list: LuaTable = match tbl.get::<_, LuaValue>(signal) {
        Ok(LuaValue::Table(t)) => t,
        _ => return,
    };
    let client_tbl = match client_to_lua(lua, win) {
        Ok(t) => t,
        Err(_) => return,
    };
    for i in 1..=list.raw_len() {
        if let Ok(LuaValue::Function(f)) = list.get::<_, LuaValue>(i) {
            let _ = f.call::<_, ()>(client_tbl.clone());
        }
    }
}

pub fn update_client_list(lua: &Lua, wm: &WmState) {
    let list = match lua.create_table() {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut seen = std::collections::HashSet::new();
    for ws in &wm.workspaces {
        for &id in &ws.windows {
            if seen.insert(id) {
                if let Some(win) = wm.windows.get(&id) {
                    if let Ok(t) = client_to_lua(lua, win) {
                        let _ = list.push(t);
                    }
                }
            }
        }
    }
    let _ = lua.set_named_registry_value("axiom_client_list", list);
}

pub fn update_screen_count(lua: &Lua, count: usize) {
    let _ = lua.set_named_registry_value("axiom_screen_count", count as LuaInteger);
}
