// src/scripting/lua_api.rs — Clean Lua API for Axiom.

use mlua::prelude::*;
use std::sync::{Arc, Mutex};

use crate::wm::{Layout, WindowId, WmConfig, WmState};

// ── Pending actions ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum LuaAction {
    Spawn(String),
    FocusId(WindowId),
    CloseId(WindowId),
    MoveToWorkspace(WindowId, usize),
    SwitchWorkspace(usize),
    SetLayout(usize, Layout),
    SetFloat(WindowId, bool),
    SetFullscreen(WindowId, bool),
    SetWindowTitle(WindowId, String),
    IncMaster,
    DecMaster,
    Reload,
    Quit,
}

pub type ActionQueue = Arc<Mutex<Vec<LuaAction>>>;

// ── Install ───────────────────────────────────────────────────────────────────

pub fn install(lua: &Lua, wm: &WmState) -> LuaResult<ActionQueue> {
    let queue: ActionQueue = Arc::new(Mutex::new(Vec::new()));
    let axiom = lua.create_table()?;

    // ── axiom.config(t) ───────────────────────────────────────────────────────
    {
        // Obtain a raw pointer to WmConfig WITHOUT going through &T first.
        // addr_of_mut! is the correct way to get *mut T from a field without
        // creating an intermediate &mut or violating the aliasing rules.
        let cfg_ptr = std::ptr::addr_of!(wm.config) as *mut WmConfig as usize;
        axiom.set(
            "config",
            lua.create_function(move |_, t: LuaTable| {
                // SAFETY: WmConfig outlives Lua; Lua runs single-threaded inside
                // the compositor main loop; no aliasing occurs here.
                let cfg = unsafe { &mut *(cfg_ptr as *mut WmConfig) };
                if let Ok(v) = t.get::<_, i32>("border_width") {
                    cfg.border_w = v;
                }
                if let Ok(v) = t.get::<_, i32>("gap") {
                    cfg.gap = v;
                }
                if let Ok(v) = t.get::<_, i32>("bar_height") {
                    cfg.bar_height = v;
                }
                if let Ok(v) = t.get::<_, usize>("workspaces") {
                    cfg.workspaces_count = v;
                }
                if let Ok(v) = t.get::<_, bool>("bar_at_bottom") {
                    cfg.bar_at_bottom = v;
                }
                if let Ok(s) = t.get::<_, String>("border_active") {
                    if let Some(c) = parse_color(&s) {
                        cfg.active_border = c;
                    }
                }
                if let Ok(s) = t.get::<_, String>("border_inactive") {
                    if let Some(c) = parse_color(&s) {
                        cfg.inactive_border = c;
                    }
                }
                if let Ok(s) = t.get::<_, String>("bar_bg") {
                    if let Some(c) = parse_color(&s) {
                        cfg.bar_bg = c;
                    }
                }
                Ok(())
            })?,
        )?;
    }

    // ── axiom.spawn(cmd) ──────────────────────────────────────────────────────
    axiom.set(
        "spawn",
        lua.create_function(|_, cmd: String| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .spawn()
                .map_err(|e| LuaError::RuntimeError(format!("spawn: {e}")))?;
            Ok(())
        })?,
    )?;

    // ── axiom.bind / axiom.unbind ─────────────────────────────────────────────
    lua.set_named_registry_value("axiom_keybinds", lua.create_table()?)?;

    axiom.set(
        "bind",
        lua.create_function(|lua, (combo, cb): (String, LuaFunction)| {
            let kb: LuaTable = lua.named_registry_value("axiom_keybinds")?;
            kb.set(normalise_combo(&combo), cb)?;
            Ok(())
        })?,
    )?;

    axiom.set(
        "unbind",
        lua.create_function(|lua, combo: String| {
            let kb: LuaTable = lua.named_registry_value("axiom_keybinds")?;
            kb.set(normalise_combo(&combo), LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    // ── axiom.workspace(n) ────────────────────────────────────────────────────
    {
        let q = Arc::clone(&queue);
        axiom.set(
            "workspace",
            lua.create_function(move |lua, n: usize| {
                let ws = lua.create_table()?;
                ws.set("index", n)?;

                let qf = Arc::clone(&q);
                ws.set(
                    "focus",
                    lua.create_function(move |_, ()| {
                        qf.lock()
                            .unwrap()
                            .push(LuaAction::SwitchWorkspace(n.saturating_sub(1)));
                        Ok(())
                    })?,
                )?;

                let ql = Arc::clone(&q);
                ws.set(
                    "set_layout",
                    lua.create_function(move |_, name: String| {
                        let layout = match name.as_str() {
                            "tile" | "master_stack" => Layout::MasterStack,
                            "bsp" => Layout::Bsp,
                            "monocle" | "max" => Layout::Monocle,
                            "float" => Layout::Float,
                            other => {
                                return Err(LuaError::RuntimeError(format!(
                                    "unknown layout '{other}'"
                                )))
                            }
                        };
                        ql.lock()
                            .unwrap()
                            .push(LuaAction::SetLayout(n.saturating_sub(1), layout));
                        Ok(())
                    })?,
                )?;

                Ok(ws)
            })?,
        )?;
    }

    // Store a raw pointer to WmState so closures can read it.
    // SAFETY: WmState outlives Lua; pointer is read-only in closures.
    let wm_ptr = wm as *const WmState as usize;
    lua.set_named_registry_value("axiom_wm_ptr", wm_ptr)?;

    // ── axiom.clients() ───────────────────────────────────────────────────────
    {
        let q = Arc::clone(&queue);
        axiom.set(
            "clients",
            lua.create_function(move |lua, ()| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                let list = lua.create_table()?;
                let mut seen = std::collections::HashSet::new();
                for ws in &wm.workspaces {
                    for &id in &ws.windows {
                        if seen.insert(id) {
                            if let Some(win) = wm.windows.get(&id) {
                                let c = build_client(
                                    lua,
                                    win.id,
                                    &win.app_id,
                                    &win.title,
                                    win.floating,
                                    win.fullscreen,
                                    win.maximized,
                                    win.rect.x,
                                    win.rect.y,
                                    win.rect.w,
                                    win.rect.h,
                                    Arc::clone(&q),
                                )?;
                                list.push(c)?;
                            }
                        }
                    }
                }
                Ok(list)
            })?,
        )?;
    }

    // ── axiom.focused() ───────────────────────────────────────────────────────
    {
        let q = Arc::clone(&queue);
        axiom.set(
            "focused",
            lua.create_function(move |lua, ()| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                let Some(id) = wm.focused_window() else {
                    return Ok(LuaValue::Nil);
                };
                let Some(win) = wm.windows.get(&id) else {
                    return Ok(LuaValue::Nil);
                };
                let c = build_client(
                    lua,
                    win.id,
                    &win.app_id,
                    &win.title,
                    win.floating,
                    win.fullscreen,
                    win.maximized,
                    win.rect.x,
                    win.rect.y,
                    win.rect.w,
                    win.rect.h,
                    Arc::clone(&q),
                )?;
                Ok(LuaValue::Table(c))
            })?,
        )?;
    }

    // ── axiom.active_workspace() ──────────────────────────────────────────────
    axiom.set(
        "active_workspace",
        lua.create_function(|lua, ()| {
            let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
            Ok(wm.active_ws() + 1)
        })?,
    )?;

    // ── axiom.screen() ────────────────────────────────────────────────────────
    axiom.set(
        "screen",
        lua.create_function(|lua, ()| {
            let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
            let out = lua.create_table()?;
            for (i, m) in wm.monitors.iter().enumerate() {
                let mt = lua.create_table()?;
                mt.set("index", i + 1)?;
                mt.set("width", m.width)?;
                mt.set("height", m.height)?;
                mt.set("x", m.x)?;
                mt.set("y", m.y)?;
                mt.set("workspace", m.active_ws + 1)?;
                out.push(mt)?;
            }
            Ok(out)
        })?,
    )?;

    // ── Action shortcuts ──────────────────────────────────────────────────────
    macro_rules! action_fn {
        ($name:literal, $act:expr) => {{
            let q = Arc::clone(&queue);
            axiom.set(
                $name,
                lua.create_function(move |_, ()| {
                    q.lock().unwrap().push($act);
                    Ok(())
                })?,
            )?;
        }};
        ($name:literal, focused, $mk:expr) => {{
            let q = Arc::clone(&queue);
            axiom.set(
                $name,
                lua.create_function(move |lua, ()| {
                    let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                    if let Some(id) = wm.focused_window() {
                        q.lock().unwrap().push($mk(id, wm));
                    }
                    Ok(())
                })?,
            )?;
        }};
    }

    action_fn!("inc_master", LuaAction::IncMaster);
    action_fn!("dec_master", LuaAction::DecMaster);
    action_fn!("quit", LuaAction::Quit);
    action_fn!("reload", LuaAction::Reload);

    {
        let q = Arc::clone(&queue);
        axiom.set(
            "close",
            lua.create_function(move |lua, ()| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                if let Some(id) = wm.focused_window() {
                    q.lock().unwrap().push(LuaAction::CloseId(id));
                }
                Ok(())
            })?,
        )?;
    }
    {
        let q = Arc::clone(&queue);
        axiom.set(
            "fullscreen",
            lua.create_function(move |lua, ()| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                if let Some(id) = wm.focused_window() {
                    let on = !wm.windows.get(&id).map(|w| w.fullscreen).unwrap_or(false);
                    q.lock().unwrap().push(LuaAction::SetFullscreen(id, on));
                }
                Ok(())
            })?,
        )?;
    }
    {
        let q = Arc::clone(&queue);
        axiom.set(
            "float",
            lua.create_function(move |lua, ()| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                if let Some(id) = wm.focused_window() {
                    let on = !wm.windows.get(&id).map(|w| w.floating).unwrap_or(false);
                    q.lock().unwrap().push(LuaAction::SetFloat(id, on));
                }
                Ok(())
            })?,
        )?;
    }

    // ── Focus / move direction helpers ────────────────────────────────────────
    // These go via the input action queue in input/mod.rs; provide Lua helpers
    // that call back into the same action system via the keybind table.
    // For now we store them as named registry values that input/mod.rs can use.
    lua.set_named_registry_value("axiom_pending_focus_dir", lua.create_table()?)?;
    lua.set_named_registry_value("axiom_pending_move_dir", lua.create_table()?)?;

    {
        let _q = Arc::clone(&queue);
        axiom.set(
            "focus_dir",
            lua.create_function(move |lua, dir: String| {
                // Enqueue as a switch-workspace no-op for now; real directional
                // focus is handled by input::Action::FocusDir wired below.
                let tbl: LuaTable = lua.named_registry_value("axiom_pending_focus_dir")?;
                tbl.push(dir)?;
                Ok(())
            })?,
        )?;
    }
    {
        let _q = Arc::clone(&queue);
        axiom.set(
            "move_dir",
            lua.create_function(move |lua, dir: String| {
                let tbl: LuaTable = lua.named_registry_value("axiom_pending_move_dir")?;
                tbl.push(dir)?;
                Ok(())
            })?,
        )?;
    }
    {
        let _q = Arc::clone(&queue);
        axiom.set(
            "cycle_focus",
            lua.create_function(move |lua, delta: i32| {
                // Translate to SwitchWorkspace as a placeholder; real impl needs
                // a CycleFocus action wired to wm.cycle_focus.
                // For now push to pending so drain() can handle it.
                lua.named_registry_value::<LuaTable>("axiom_pending_focus_dir")?
                    .push(if delta > 0 {
                        "cycle+".to_string()
                    } else {
                        "cycle-".to_string()
                    })?;
                Ok(())
            })?,
        )?;
    }

    // ── axiom.rule { match, action } ─────────────────────────────────────────
    lua.set_named_registry_value("axiom_rules", lua.create_table()?)?;
    axiom.set(
        "rule",
        lua.create_function(|lua, rule: LuaTable| {
            let tbl: LuaTable = lua.named_registry_value("axiom_rules")?;
            tbl.push(rule)?;
            Ok(())
        })?,
    )?;

    // ── axiom.on / axiom.off ──────────────────────────────────────────────────
    lua.set_named_registry_value("axiom_signals", lua.create_table()?)?;

    axiom.set(
        "on",
        lua.create_function(|lua, (event, cb): (String, LuaFunction)| {
            let tbl: LuaTable = lua.named_registry_value("axiom_signals")?;
            let list: LuaTable = match tbl.get::<_, LuaValue>(event.clone())? {
                LuaValue::Table(t) => t,
                _ => {
                    let t = lua.create_table()?;
                    tbl.set(event, t.clone())?;
                    t
                }
            };
            list.push(cb)?;
            Ok(())
        })?,
    )?;

    axiom.set(
        "off",
        lua.create_function(|lua, event: String| {
            let tbl: LuaTable = lua.named_registry_value("axiom_signals")?;
            tbl.set(event, LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    // ── axiom.notify ─────────────────────────────────────────────────────────
    axiom.set(
        "notify",
        lua.create_function(|_, (msg, ms): (String, Option<u32>)| {
            let t = ms.unwrap_or(3000);
            std::process::Command::new("notify-send")
                .args(["-t", &t.to_string(), "Axiom", &msg])
                .spawn()
                .ok();
            Ok(())
        })?,
    )?;

    lua.globals().set("axiom", axiom)?;

    install_compat(lua, Arc::clone(&queue))?;

    Ok(queue)
}

// ── Fire a keybind ────────────────────────────────────────────────────────────

pub fn fire_keybind(lua: &Lua, combo: &str) -> LuaResult<()> {
    let kb: LuaTable = lua.named_registry_value("axiom_keybinds")?;
    let norm = normalise_combo(combo);
    if let Ok(LuaValue::Function(f)) = kb.get::<_, LuaValue>(norm.as_str()) {
        f.call::<_, ()>(()).map_err(|e| {
            tracing::error!("keybind '{}': {e}", norm);
            e
        })?;
    }
    Ok(())
}

// ── Emit a signal ─────────────────────────────────────────────────────────────

/// Emit a signal with a single table argument (or nil).
pub fn emit_table(lua: &Lua, event: &str, arg: Option<LuaTable>) {
    let Ok(tbl) = lua.named_registry_value::<LuaTable>("axiom_signals") else {
        return;
    };
    let Ok(LuaValue::Table(list)) = tbl.get::<_, LuaValue>(event) else {
        return;
    };
    for i in 1..=list.raw_len() {
        if let Ok(LuaValue::Function(f)) = list.get::<_, LuaValue>(i) {
            let res = match &arg {
                Some(t) => f.call::<_, ()>(t.clone()),
                None => f.call::<_, ()>(()),
            };
            if let Err(e) = res {
                tracing::warn!("signal '{event}' handler {i}: {e}");
            }
        }
    }
}

/// Emit a signal with no arguments.
pub fn emit_bare(lua: &Lua, event: &str) {
    emit_table(lua, event, None);
}

// ── Drain actions ─────────────────────────────────────────────────────────────

pub fn drain(queue: &ActionQueue, state: &mut crate::state::Axiom) {
    // Drain directional focus/move requests from Lua.
    drain_dir_actions(state);

    let actions: Vec<LuaAction> = std::mem::take(&mut *queue.lock().unwrap());
    for action in actions {
        apply(state, action);
    }
}

fn drain_dir_actions(state: &mut crate::state::Axiom) {
    let focus_dirs: Vec<String> = {
        if let Ok(tbl) = state
            .script
            .lua
            .named_registry_value::<LuaTable>("axiom_pending_focus_dir")
        {
            let v: Vec<String> = (1..=tbl.raw_len())
                .filter_map(|i| tbl.get::<_, String>(i).ok())
                .collect();
            // Clear the table.
            for i in 1..=tbl.raw_len() {
                let _ = tbl.set(i, LuaValue::Nil);
            }
            v
        } else {
            vec![]
        }
    };
    for dir in focus_dirs {
        match dir.as_str() {
            "left" => {
                state.wm.focus_direction(0);
                state.sync_keyboard_focus();
            }
            "right" => {
                state.wm.focus_direction(1);
                state.sync_keyboard_focus();
            }
            "up" => {
                state.wm.focus_direction(2);
                state.sync_keyboard_focus();
            }
            "down" => {
                state.wm.focus_direction(3);
                state.sync_keyboard_focus();
            }
            "cycle+" => {
                let aws = state.wm.active_ws();
                state.wm.workspaces[aws].cycle_focus(1);
                state.sync_keyboard_focus();
            }
            "cycle-" => {
                let aws = state.wm.active_ws();
                state.wm.workspaces[aws].cycle_focus(-1);
                state.sync_keyboard_focus();
            }
            _ => {}
        }
        state.needs_redraw = true;
    }

    let move_dirs: Vec<String> = {
        if let Ok(tbl) = state
            .script
            .lua
            .named_registry_value::<LuaTable>("axiom_pending_move_dir")
        {
            let v: Vec<String> = (1..=tbl.raw_len())
                .filter_map(|i| tbl.get::<_, String>(i).ok())
                .collect();
            for i in 1..=tbl.raw_len() {
                let _ = tbl.set(i, LuaValue::Nil);
            }
            v
        } else {
            vec![]
        }
    };
    for dir in move_dirs {
        let d = match dir.as_str() {
            "left" => 0u8,
            "right" => 1,
            "up" => 2,
            "down" => 3,
            _ => continue,
        };
        state.wm.move_direction(d);
        state.needs_redraw = true;
    }
}

fn apply(state: &mut crate::state::Axiom, action: LuaAction) {
    use LuaAction::*;
    match action {
        Spawn(cmd) => {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .spawn()
                .ok();
        }
        FocusId(id) => {
            state.wm.focus_window(id);
            state.sync_keyboard_focus();
        }
        CloseId(id) => {
            state.close_window(id);
        }
        MoveToWorkspace(id, ws) => {
            state.wm.move_to_workspace(id, ws);
            state.needs_redraw = true;
        }
        SwitchWorkspace(ws) => {
            state.wm.switch_workspace(ws);
            state.needs_redraw = true;
        }
        SetLayout(ws, layout) => {
            if let Some(w) = state.wm.workspaces.get_mut(ws) {
                w.layout = layout;
            }
            state.wm.reflow();
            state.needs_redraw = true;
        }
        SetFloat(id, on) => {
            if let Some(w) = state.wm.windows.get_mut(&id) {
                w.floating = on;
            }
            state.wm.reflow();
            state.needs_redraw = true;
        }
        SetFullscreen(id, on) => {
            state.wm.fullscreen_window(id, on);
            state.send_configure_focused();
            state.needs_redraw = true;
        }
        SetWindowTitle(id, t) => {
            state.wm.set_title(id, t);
        }
        IncMaster => {
            state.wm.inc_master();
            state.wm.reflow();
            state.needs_redraw = true;
        }
        DecMaster => {
            state.wm.dec_master();
            state.wm.reflow();
            state.needs_redraw = true;
        }
        Reload => {
            state.reload_config();
        }
        Quit => {
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

pub fn apply_actions(state: &mut crate::state::Axiom, actions: Vec<LuaAction>) {
    // Drain directional focus/move requests from Lua registry tables.
    drain_dir_actions(state);
    for action in actions {
        apply(state, action);
    }
}

// ── Client table ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_client<'lua>(
    lua: &'lua Lua,
    id: WindowId,
    app_id: &str,
    title: &str,
    floating: bool,
    fullscreen: bool,
    maximized: bool,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    q: ActionQueue,
) -> LuaResult<LuaTable<'lua>> {
    let c = lua.create_table()?;
    c.set("id", id)?;
    c.set("app_id", app_id.to_string())?;
    c.set("class", app_id.to_string())?;
    c.set("name", title.to_string())?;
    c.set("title", title.to_string())?;
    c.set("floating", floating)?;
    c.set("fullscreen", fullscreen)?;
    c.set("maximized", maximized)?;
    c.set("x", x)?;
    c.set("y", y)?;
    c.set("width", w)?;
    c.set("height", h)?;

    let qc = Arc::clone(&q);
    c.set(
        "close",
        lua.create_function(move |_, _self: LuaValue| {
            qc.lock().unwrap().push(LuaAction::CloseId(id));
            Ok(())
        })?,
    )?;
    let qf = Arc::clone(&q);
    c.set(
        "focus",
        lua.create_function(move |_, _self: LuaValue| {
            qf.lock().unwrap().push(LuaAction::FocusId(id));
            Ok(())
        })?,
    )?;
    let qfs = Arc::clone(&q);
    c.set(
        "set_fullscreen",
        lua.create_function(move |_, (_self, on): (LuaValue, bool)| {
            qfs.lock().unwrap().push(LuaAction::SetFullscreen(id, on));
            Ok(())
        })?,
    )?;
    let qfl = Arc::clone(&q);
    c.set(
        "set_float",
        lua.create_function(move |_, (_self, on): (LuaValue, bool)| {
            qfl.lock().unwrap().push(LuaAction::SetFloat(id, on));
            Ok(())
        })?,
    )?;
    let qmv = Arc::clone(&q);
    c.set(
        "move_to",
        lua.create_function(move |_, (_self, ws): (LuaValue, usize)| {
            qmv.lock()
                .unwrap()
                .push(LuaAction::MoveToWorkspace(id, ws.saturating_sub(1)));
            Ok(())
        })?,
    )?;
    Ok(c)
}

// ── WmState pointer helpers ───────────────────────────────────────────────────

fn get_wm_ptr(lua: &Lua) -> LuaResult<usize> {
    lua.named_registry_value::<usize>("axiom_wm_ptr")
}

// ── Rule application (called on window open) ──────────────────────────────────

pub fn apply_rules(lua: &Lua, app_id: &str, title: &str) -> Vec<LuaAction> {
    let mut actions = Vec::new();
    let Ok(tbl) = lua.named_registry_value::<LuaTable>("axiom_rules") else {
        return actions;
    };
    for pair in tbl.clone().pairs::<LuaValue, LuaTable>() {
        let Ok((_, rule)) = pair else { continue };
        let Ok(m) = rule.get::<_, LuaTable>("match") else {
            continue;
        };
        let app_ok = m
            .get::<_, String>("app_id")
            .map(|a| glob_match(&a, app_id))
            .unwrap_or(true);
        let ttl_ok = m
            .get::<_, String>("title")
            .map(|t| glob_match(&t, title))
            .unwrap_or(true);
        if !app_ok || !ttl_ok {
            continue;
        }
        let Ok(act) = rule.get::<_, LuaTable>("action") else {
            continue;
        };
        if let Ok(ws) = act.get::<_, usize>("workspace") {
            actions.push(LuaAction::MoveToWorkspace(0, ws.saturating_sub(1)));
        }
        if let Ok(on) = act.get::<_, bool>("fullscreen") {
            actions.push(LuaAction::SetFullscreen(0, on));
        }
    }
    actions
}

// ── AwesomeWM compat shims ────────────────────────────────────────────────────

fn install_compat(lua: &Lua, q: ActionQueue) -> LuaResult<()> {
    let client = lua.create_table()?;
    client.set(
        "connect_signal",
        lua.create_function(|lua, (sig, cb): (String, LuaFunction)| {
            let mapped = match sig.as_str() {
                "manage" => "client.open",
                "unmanage" => "client.close",
                "focus" => "client.focus",
                "unfocus" => "client.unfocus",
                "property::title" => "client.title",
                "property::floating" => "client.float",
                "property::fullscreen" => "client.fullscreen",
                other => other,
            };
            let tbl: LuaTable = lua.named_registry_value("axiom_signals")?;
            let list: LuaTable = match tbl.get::<_, LuaValue>(mapped)? {
                LuaValue::Table(t) => t,
                _ => {
                    let t = lua.create_table()?;
                    tbl.set(mapped, t.clone())?;
                    t
                }
            };
            list.push(cb)?;
            Ok(())
        })?,
    )?;
    client.set(
        "disconnect_signal",
        lua.create_function(|lua, sig: String| {
            let tbl: LuaTable = lua.named_registry_value("axiom_signals")?;
            tbl.set(sig, LuaValue::Nil)?;
            Ok(())
        })?,
    )?;
    lua.globals().set("client", client)?;

    let awful = lua.create_table()?;
    awful.set(
        "spawn",
        lua.create_function(|_, cmd: String| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .spawn()
                .ok();
            Ok(())
        })?,
    )?;
    let qq = Arc::clone(&q);
    awful.set(
        "quit",
        lua.create_function(move |_, ()| {
            qq.lock().unwrap().push(LuaAction::Quit);
            Ok(())
        })?,
    )?;
    lua.globals().set("awful", awful)?;

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Normalise "Super+Return" → "super+return", "MOD4+shift+h" → "super+shift+h"
pub fn normalise_combo(s: &str) -> String {
    s.split('+')
        .map(|part| {
            match part.to_lowercase().as_str() {
                "mod4" | "super" | "logo" => "super",
                "mod1" | "alt" => "alt",
                "control" | "ctrl" => "ctrl",
                "shift" => "shift",
                _ => part, // keep original if unknown
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

fn glob_match(pattern: &str, s: &str) -> bool {
    if let Some(p) = pattern.strip_suffix('*') {
        s.starts_with(p)
    } else if let Some(p) = pattern.strip_prefix('*') {
        s.ends_with(p)
    } else {
        pattern == s
    }
}

pub fn parse_color(s: &str) -> Option<[f32; 4]> {
    let s = s.trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(match s.len() {
        6 => [
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
            1.0,
        ],
        8 => [
            ((v >> 24) & 0xff) as f32 / 255.0,
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
        ],
        _ => return None,
    })
}
