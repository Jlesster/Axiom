// src/scripting/lua_api.rs
//
// Axiom Lua API
// All functions live on the global `axiom` table.
//
// Quick reference
// ───────────────
//   axiom.set { border_width, gap, bar_height, workspaces,
//               bar_at_bottom, border_active, border_inactive, bar_bg }
//   axiom.spawn(cmd)
//   axiom.notify(msg [, ms])
//
//   axiom.key(combo, fn)          -- register keybind
//   axiom.unkey(combo)            -- remove keybind
//
//   axiom.workspace(n)            -- switch to workspace n  (replaces goto)
//   axiom.send(n)                 -- move focused window to workspace n
//   axiom.ws()                    -- current workspace index (1-based)
//   axiom.layout(ws, name)        -- set layout for workspace ws
//
//   axiom.focus(dir)              -- focus in direction: left/right/up/down
//   axiom.cycle(delta)            -- cycle focus (+1 / -1)
//   axiom.move(dir)               -- move window in direction
//   axiom.close()                 -- close focused window
//   axiom.float()                 -- toggle float on focused window
//   axiom.fullscreen()            -- toggle fullscreen on focused window
//   axiom.inc_master()            -- grow master count
//   axiom.dec_master()            -- shrink master count
//
//   axiom.clients()               -- list of client tables
//   axiom.focused()               -- client table for focused window, or nil
//   axiom.screens()               -- list of monitor tables
//   axiom.rule { match, action }  -- add window rule
//
//   axiom.on(event, fn)           -- subscribe to compositor event
//   axiom.off(event)              -- clear all handlers for event
//
//   axiom.reload()                -- reload config
//   axiom.quit()                  -- exit compositor

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

// ── Macro: queue a simple action with no closure args ─────────────────────────

macro_rules! queue_fn {
    ($lua:expr, $queue:expr, $action:expr) => {{
        let q = Arc::clone(&$queue);
        $lua.create_function(move |_, ()| {
            q.lock().unwrap().push($action);
            Ok(())
        })?
    }};
}

// ── Install ───────────────────────────────────────────────────────────────────

pub fn install(lua: &Lua, wm: &WmState) -> LuaResult<ActionQueue> {
    let queue: ActionQueue = Arc::new(Mutex::new(Vec::new()));
    let ax = lua.create_table()?;

    // Store WmState pointer for read-only closures
    let wm_ptr = wm as *const WmState as usize;
    lua.set_named_registry_value("axiom_wm_ptr", wm_ptr)?;

    // ── axiom.set { ... } ─────────────────────────────────────────────────────
    {
        let cfg_ptr = std::ptr::addr_of!(wm.config) as *mut WmConfig as usize;
        ax.set(
            "set",
            lua.create_function(move |_, t: LuaTable| {
                let cfg = unsafe { &mut *(cfg_ptr as *mut WmConfig) };
                macro_rules! maybe {
                    ($key:literal, $field:ident) => {
                        if let Ok(v) = t.get::<_, _>($key) {
                            cfg.$field = v;
                        }
                    };
                }
                macro_rules! maybe_color {
                    ($key:literal, $field:ident) => {
                        if let Ok(s) = t.get::<_, String>($key) {
                            if let Some(c) = parse_color(&s) {
                                cfg.$field = c;
                            }
                        }
                    };
                }
                maybe!("border_width", border_w);
                maybe!("gap", gap);
                maybe!("bar_height", bar_height);
                maybe!("workspaces", workspaces_count);
                maybe!("bar_at_bottom", bar_at_bottom);
                maybe_color!("border_active", active_border);
                maybe_color!("border_inactive", inactive_border);
                maybe_color!("bar_bg", bar_bg);
                Ok(())
            })?,
        )?;
    }

    // ── axiom.spawn(cmd) ──────────────────────────────────────────────────────
    ax.set(
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

    // ── axiom.notify(msg [, ms]) ──────────────────────────────────────────────
    ax.set(
        "notify",
        lua.create_function(|_, (msg, ms): (String, Option<u32>)| {
            std::process::Command::new("notify-send")
                .args(["-t", &ms.unwrap_or(3000).to_string(), "Axiom", &msg])
                .spawn()
                .ok();
            Ok(())
        })?,
    )?;

    // ── axiom.key(combo, fn) / axiom.unkey(combo) ─────────────────────────────
    lua.set_named_registry_value("axiom_keybinds", lua.create_table()?)?;

    ax.set(
        "key",
        lua.create_function(|lua, (combo, cb): (String, LuaFunction)| {
            let kb: LuaTable = lua.named_registry_value("axiom_keybinds")?;
            kb.set(normalise_combo(&combo), cb)?;
            Ok(())
        })?,
    )?;

    ax.set(
        "unkey",
        lua.create_function(|lua, combo: String| {
            let kb: LuaTable = lua.named_registry_value("axiom_keybinds")?;
            kb.set(normalise_combo(&combo), LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    // ── axiom.workspace(n) — switch workspace (safe alternative to goto) ──────
    {
        let q = Arc::clone(&queue);
        ax.set(
            "workspace",
            lua.create_function(move |_, n: usize| {
                q.lock()
                    .unwrap()
                    .push(LuaAction::SwitchWorkspace(n.saturating_sub(1)));
                Ok(())
            })?,
        )?;
    }

    // ── axiom.send(n) — move focused window to workspace n ───────────────────
    {
        let q = Arc::clone(&queue);
        ax.set(
            "send",
            lua.create_function(move |lua, n: usize| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                if let Some(id) = wm.focused_window() {
                    q.lock()
                        .unwrap()
                        .push(LuaAction::MoveToWorkspace(id, n.saturating_sub(1)));
                }
                Ok(())
            })?,
        )?;
    }

    // ── axiom.ws() — current workspace (1-based) ─────────────────────────────
    ax.set(
        "ws",
        lua.create_function(|lua, ()| {
            let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
            Ok(wm.active_ws() + 1)
        })?,
    )?;

    // ── axiom.layout(ws, name) ────────────────────────────────────────────────
    {
        let q = Arc::clone(&queue);
        ax.set(
            "layout",
            lua.create_function(move |_, (n, name): (usize, String)| {
                let layout = parse_layout(&name)?;
                q.lock()
                    .unwrap()
                    .push(LuaAction::SetLayout(n.saturating_sub(1), layout));
                Ok(())
            })?,
        )?;
    }

    // ── Focus / move / cycle ──────────────────────────────────────────────────
    lua.set_named_registry_value("axiom_pending_focus_dir", lua.create_table()?)?;
    lua.set_named_registry_value("axiom_pending_move_dir", lua.create_table()?)?;

    ax.set(
        "focus",
        lua.create_function(|lua, dir: String| {
            lua.named_registry_value::<LuaTable>("axiom_pending_focus_dir")?
                .push(dir)?;
            Ok(())
        })?,
    )?;

    ax.set(
        "cycle",
        lua.create_function(|lua, delta: i32| {
            lua.named_registry_value::<LuaTable>("axiom_pending_focus_dir")?
                .push(if delta > 0 { "cycle+" } else { "cycle-" })?;
            Ok(())
        })?,
    )?;

    ax.set(
        "move",
        lua.create_function(|lua, dir: String| {
            lua.named_registry_value::<LuaTable>("axiom_pending_move_dir")?
                .push(dir)?;
            Ok(())
        })?,
    )?;

    // ── Focused-window shortcuts ──────────────────────────────────────────────
    {
        let q = Arc::clone(&queue);
        ax.set(
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
        ax.set(
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
        ax.set(
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

    ax.set("inc_master", queue_fn!(lua, queue, LuaAction::IncMaster))?;
    ax.set("dec_master", queue_fn!(lua, queue, LuaAction::DecMaster))?;
    ax.set("reload", queue_fn!(lua, queue, LuaAction::Reload))?;
    ax.set("quit", queue_fn!(lua, queue, LuaAction::Quit))?;

    // ── axiom.clients() ───────────────────────────────────────────────────────
    {
        let q = Arc::clone(&queue);
        ax.set(
            "clients",
            lua.create_function(move |lua, ()| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                let list = lua.create_table()?;
                let mut seen = std::collections::HashSet::new();
                for ws in &wm.workspaces {
                    for &id in &ws.windows {
                        if seen.insert(id) {
                            if let Some(win) = wm.windows.get(&id) {
                                list.push(build_client(lua, win, Arc::clone(&q))?)?;
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
        ax.set(
            "focused",
            lua.create_function(move |lua, ()| {
                let wm = unsafe { &*(get_wm_ptr(lua)? as *const WmState) };
                match wm.focused_window().and_then(|id| wm.windows.get(&id)) {
                    Some(win) => Ok(LuaValue::Table(build_client(lua, win, Arc::clone(&q))?)),
                    None => Ok(LuaValue::Nil),
                }
            })?,
        )?;
    }

    // ── axiom.screens() ───────────────────────────────────────────────────────
    ax.set(
        "screens",
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

    // ── axiom.rule { ... } ────────────────────────────────────────────────────
    lua.set_named_registry_value("axiom_rules", lua.create_table()?)?;
    ax.set(
        "rule",
        lua.create_function(|lua, rule: LuaTable| {
            lua.named_registry_value::<LuaTable>("axiom_rules")?
                .push(rule)?;
            Ok(())
        })?,
    )?;

    // ── axiom.on(event, fn) / axiom.off(event) ───────────────────────────────
    lua.set_named_registry_value("axiom_signals", lua.create_table()?)?;

    ax.set(
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

    ax.set(
        "off",
        lua.create_function(|lua, event: String| {
            lua.named_registry_value::<LuaTable>("axiom_signals")?
                .set(event, LuaValue::Nil)?;
            Ok(())
        })?,
    )?;

    lua.globals().set("axiom", ax)?;
    Ok(queue)
}

// ── Fire a keybind ────────────────────────────────────────────────────────────

pub fn fire_keybind(lua: &Lua, combo: &str) -> LuaResult<bool> {
    let tbl: LuaTable = lua.named_registry_value("axiom_keybinds")?;
    match tbl.get::<_, LuaValue>(combo)? {
        LuaValue::Function(f) => {
            f.call::<_, ()>(())?;
            Ok(true)
        }
        _ => Ok(false), // Nil or anything else → not registered
    }
}

// ── Emit signals ──────────────────────────────────────────────────────────────

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

pub fn emit_bare(lua: &Lua, event: &str) {
    emit_table(lua, event, None);
}

// ── Drain actions ─────────────────────────────────────────────────────────────

pub fn drain(queue: &ActionQueue, state: &mut crate::state::Axiom) {
    drain_dir_actions(state);
    let actions: Vec<LuaAction> = std::mem::take(&mut *queue.lock().unwrap());
    for action in actions {
        apply(state, action);
    }
}

// drain_actions is kept pub for call-sites that already have a Vec<LuaAction>
pub fn drain_actions(actions: Vec<LuaAction>, state: &mut crate::state::Axiom) {
    for action in actions {
        apply(state, action);
    }
}

fn drain_dir_actions(state: &mut crate::state::Axiom) {
    // focus directions
    let focus_dirs = drain_string_table(&state.script.lua, "axiom_pending_focus_dir");
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
                let ws = state.wm.active_ws();
                state.wm.workspaces[ws].cycle_focus(1);
                state.sync_keyboard_focus();
            }
            "cycle-" => {
                let ws = state.wm.active_ws();
                state.wm.workspaces[ws].cycle_focus(-1);
                state.sync_keyboard_focus();
            }
            _ => {}
        }
        state.needs_redraw = true;
    }

    // move directions
    let move_dirs = drain_string_table(&state.script.lua, "axiom_pending_move_dir");
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

/// Drain a Lua registry table that holds a sequence of strings, clearing it in place.
fn drain_string_table(lua: &Lua, key: &str) -> Vec<String> {
    let Ok(tbl) = lua.named_registry_value::<LuaTable>(key) else {
        return Vec::new();
    };
    let v: Vec<String> = (1..=tbl.raw_len())
        .filter_map(|i| tbl.get::<_, String>(i).ok())
        .collect();
    for i in 1..=tbl.raw_len() {
        let _ = tbl.set(i, LuaValue::Nil);
    }
    v
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

// ── Client table ──────────────────────────────────────────────────────────────

fn build_client<'lua>(
    lua: &'lua Lua,
    win: &crate::wm::Window,
    q: ActionQueue,
) -> LuaResult<LuaTable<'lua>> {
    let id = win.id;
    let c = lua.create_table()?;

    // Properties
    c.set("id", id)?;
    c.set("app_id", win.app_id.clone())?;
    c.set("title", win.title.clone())?;
    c.set("floating", win.floating)?;
    c.set("fullscreen", win.fullscreen)?;
    c.set("maximized", win.maximized)?;
    c.set("x", win.rect.x)?;
    c.set("y", win.rect.y)?;
    c.set("width", win.rect.w)?;
    c.set("height", win.rect.h)?;

    // Methods
    let qc = Arc::clone(&q);
    c.set(
        "close",
        lua.create_function(move |_, _: LuaValue| {
            qc.lock().unwrap().push(LuaAction::CloseId(id));
            Ok(())
        })?,
    )?;

    let qf = Arc::clone(&q);
    c.set(
        "focus",
        lua.create_function(move |_, _: LuaValue| {
            qf.lock().unwrap().push(LuaAction::FocusId(id));
            Ok(())
        })?,
    )?;

    let qfs = Arc::clone(&q);
    c.set(
        "set_fullscreen",
        lua.create_function(move |_, (_, on): (LuaValue, bool)| {
            qfs.lock().unwrap().push(LuaAction::SetFullscreen(id, on));
            Ok(())
        })?,
    )?;

    let qfl = Arc::clone(&q);
    c.set(
        "set_float",
        lua.create_function(move |_, (_, on): (LuaValue, bool)| {
            qfl.lock().unwrap().push(LuaAction::SetFloat(id, on));
            Ok(())
        })?,
    )?;

    let qmv = Arc::clone(&q);
    c.set(
        "move_to",
        lua.create_function(move |_, (_, ws): (LuaValue, usize)| {
            qmv.lock()
                .unwrap()
                .push(LuaAction::MoveToWorkspace(id, ws.saturating_sub(1)));
            Ok(())
        })?,
    )?;

    Ok(c)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn get_wm_ptr(lua: &Lua) -> LuaResult<usize> {
    lua.named_registry_value::<usize>("axiom_wm_ptr")
}

fn parse_layout(name: &str) -> LuaResult<Layout> {
    match name {
        "tile" | "master_stack" => Ok(Layout::MasterStack),
        "bsp" => Ok(Layout::Bsp),
        "monocle" | "max" => Ok(Layout::Monocle),
        "float" => Ok(Layout::Float),
        other => Err(LuaError::RuntimeError(format!(
            "unknown layout '{other}' — use tile/bsp/monocle/float"
        ))),
    }
}

pub fn normalise_combo(s: &str) -> String {
    s.split('+')
        .map(|part| match part.to_lowercase().as_str() {
            "mod4" | "super" | "logo" => "super".to_string(),
            "mod1" | "alt" => "alt".to_string(),
            "control" | "ctrl" => "ctrl".to_string(),
            "shift" => "shift".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
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
