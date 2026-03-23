// src/input/mod.rs — Input pipeline.

use std::collections::HashMap;
use std::os::unix::io::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};

use input::{
    event::{
        keyboard::{KeyState, KeyboardEvent, KeyboardEventTrait},
        pointer::{Axis, PointerEvent, PointerEventTrait, PointerScrollEvent},
        Event,
    },
    Libinput, LibinputInterface,
};
use wayland_protocols::xdg::shell::server::xdg_popup::XdgPopup;
use xkbcommon::xkb::{self, Keycode};

use crate::state::Axiom;

// ── libseat FFI ───────────────────────────────────────────────────────────────

enum LibseatOpaque {}

#[link(name = "seat")]
extern "C" {
    fn libseat_open_device(seat: *mut LibseatOpaque, path: *const i8, device_id: *mut i32) -> i32;
    fn libseat_close_device(seat: *mut LibseatOpaque, device_id: i32) -> i32;
}

// ── LibinputUdev ──────────────────────────────────────────────────────────────

pub struct LibinputUdev {
    seat: *mut LibseatOpaque,
    open_ids: HashMap<RawFd, i32>,
}

unsafe impl Send for LibinputUdev {}

impl LibinputUdev {
    pub unsafe fn new(raw_seat: *mut std::ffi::c_void) -> Self {
        Self {
            seat: raw_seat as *mut LibseatOpaque,
            open_ids: HashMap::new(),
        }
    }
}

impl LibinputInterface for LibinputUdev {
    fn open_restricted(&mut self, path: &std::path::Path, _flags: i32) -> Result<OwnedFd, i32> {
        use std::ffi::CString;
        let cpath = match path.to_str().and_then(|s| CString::new(s).ok()) {
            Some(c) => c,
            None => return Err(-1),
        };
        let mut device_id: i32 = -1;
        let fd = unsafe { libseat_open_device(self.seat, cpath.as_ptr(), &mut device_id) };
        if fd < 0 {
            Err(fd)
        } else {
            self.open_ids.insert(fd, device_id);
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        let raw = fd.as_raw_fd();
        if let Some(device_id) = self.open_ids.remove(&raw) {
            unsafe {
                libseat_close_device(self.seat, device_id);
            }
        }
        drop(fd);
    }
}

// ── Modifier mask ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mods(pub u32);

impl Mods {
    pub const NONE: Self = Self(0);
    pub const SUPER: Self = Self(1 << 0);
    pub const ALT: Self = Self(1 << 1);
    pub const CTRL: Self = Self(1 << 2);
    pub const SHIFT: Self = Self(1 << 3);

    pub fn from_xkb(state: &xkb::State) -> Self {
        let mut m = 0u32;
        if state.mod_name_is_active(xkb::MOD_NAME_LOGO, xkb::STATE_MODS_EFFECTIVE) {
            m |= 1 << 0;
        }
        if state.mod_name_is_active(xkb::MOD_NAME_ALT, xkb::STATE_MODS_EFFECTIVE) {
            m |= 1 << 1;
        }
        if state.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE) {
            m |= 1 << 2;
        }
        if state.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE) {
            m |= 1 << 3;
        }
        Self(m)
    }
}

// ── Actions ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Action {
    Lua(String),
    Close,
    FocusDir(u8),
    MoveDir(u8),
    SwitchWorkspace(usize),
    MoveToWorkspace(usize),
    ToggleFloat,
    ToggleFullscreen,
    IncMaster,
    DecMaster,
    Reload,
    Quit,
    Spawn(String),
}

// ── Keybind table ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct KeybindTable {
    bindings: HashMap<(Mods, xkb::Keysym), Action>,
}

impl KeybindTable {
    pub fn bind(&mut self, mods: Mods, sym: xkb::Keysym, action: Action) {
        self.bindings.insert((mods, sym), action);
    }
    pub fn lookup(&self, mods: Mods, sym: xkb::Keysym) -> Option<&Action> {
        self.bindings.get(&(mods, sym))
    }
    pub fn bind_lua(&mut self, mods: Mods, sym: xkb::Keysym, fn_name: String) {
        self.bind(mods, sym, Action::Lua(fn_name));
    }
}

// ── InputState ────────────────────────────────────────────────────────────────

pub struct InputState {
    pub libinput: Libinput,
    pub keybinds: KeybindTable,
    pub pointer_x: f64,
    pub pointer_y: f64,
    pub cursor_pos: (f64, f64),
    pub hw_cursor_active: bool,
    pub cursor_surface: Option<wayland_server::protocol::wl_surface::WlSurface>,
    pub popup_grab: Option<XdgPopup>,
    pub screen_w: f64,
    pub screen_h: f64,
    /// Whether any mouse button is currently held — used to avoid dropping
    /// pointer focus mid-drag.
    pub button_held: bool,
}

impl InputState {
    pub fn new(session: &crate::backend::session::Session) -> anyhow::Result<Self> {
        let udev = unsafe { LibinputUdev::new(session.raw_seat()) };
        let mut libinput = Libinput::new_with_udev(udev);
        libinput
            .udev_assign_seat("seat0")
            .map_err(|_| anyhow::anyhow!("libinput: failed to assign seat0"))?;
        Ok(Self {
            libinput,
            keybinds: KeybindTable::default(),
            pointer_x: 0.0,
            pointer_y: 0.0,
            cursor_pos: (0.0, 0.0),
            hw_cursor_active: false,
            cursor_surface: None,
            popup_grab: None,
            screen_w: 1920.0,
            screen_h: 1080.0,
            button_held: false,
        })
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.libinput.as_fd().as_raw_fd()
    }
    pub fn set_popup_grab(&mut self, popup: XdgPopup) {
        self.popup_grab = Some(popup);
    }
    pub fn clear_popup_grab(&mut self) {
        self.popup_grab = None;
    }
}

// ── Full event dispatch ───────────────────────────────────────────────────────

pub fn dispatch_libinput_events(state: &mut Axiom) {
    if let Err(e) = state.input.libinput.dispatch() {
        tracing::error!("libinput: {e}");
        return;
    }
    loop {
        match state.input.libinput.next() {
            Some(Event::Keyboard(k)) => handle_keyboard(state, k),
            Some(Event::Pointer(p)) => handle_pointer(state, p),
            Some(_) => {}
            None => break,
        }
    }
}

// ── Keyboard ──────────────────────────────────────────────────────────────────

fn handle_keyboard(state: &mut Axiom, event: KeyboardEvent) {
    let KeyboardEvent::Key(key) = event else {
        return;
    };

    let keycode = key.key();
    let xkb_keycode = Keycode::new(keycode + 8);
    let key_state = key.key_state();
    let time = key.time();

    let dir = match key_state {
        KeyState::Pressed => xkb::KeyDirection::Down,
        KeyState::Released => xkb::KeyDirection::Up,
    };
    state.seat.xkb_state.update_key(xkb_keycode, dir);

    // Always send modifier-only keys (Shift, Ctrl, Alt, Super) straight
    // through so clients track modifier state correctly.
    let sym = state.seat.xkb_state.key_get_one_sym(xkb_keycode);
    if is_modifier_sym(sym) {
        let wl_state = wl_key_state(key_state);
        state.seat.send_key(time, keycode, wl_state);
        return;
    }

    if key_state == KeyState::Pressed {
        let mods = Mods::from_xkb(&state.seat.xkb_state);

        // ── Hard-coded emergency binds ────────────────────────────────────────
        if mods == Mods(Mods::SUPER.0 | Mods::SHIFT.0) && sym == xkb::Keysym::Print {
            tracing::info!("emergency quit (Super+Shift+Print)");
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        }

        if mods == Mods(Mods::CTRL.0 | Mods::ALT.0) {
            if let Some(vt) = vt_from_sym(sym) {
                vt_switch(state, vt);
                return;
            }
        }

        // ── Lua keybinds ──────────────────────────────────────────────────────
        // fire_keybind returns Err when the combo is not registered in Lua.
        // We must NOT swallow the key if no bind matched.
        let combo = combo_string(mods, sym);
        let lua_matched = match crate::scripting::lua_api::fire_keybind(&state.script.lua, &combo) {
            Ok(matched) => matched,
            Err(e) => {
                // Lua threw — log but don't swallow the key.
                tracing::error!("keybind '{combo}': {e}");
                false
            }
        };

        if lua_matched {
            let queue = state.script.queue.clone();
            crate::scripting::lua_api::drain(&queue, state);
            return; // key was consumed by a bind
        }

        // ── Native keybind table ──────────────────────────────────────────────
        if let Some(action) = state.input.keybinds.lookup(mods, sym).cloned() {
            dispatch_action(state, action);
            return;
        }

        // ── No bind matched — fall through to client ──────────────────────────
    }

    // Deliver key press/release to the focused Wayland client.
    let wl_state = wl_key_state(key_state);
    state.seat.send_key(time, keycode, wl_state);
}

// ── Pointer ───────────────────────────────────────────────────────────────────

fn handle_pointer(state: &mut Axiom, event: PointerEvent) {
    match event {
        PointerEvent::Motion(m) => {
            let time = m.time();
            state.input.pointer_x =
                (state.input.pointer_x + m.dx()).clamp(0.0, state.input.screen_w - 1.0);
            state.input.pointer_y =
                (state.input.pointer_y + m.dy()).clamp(0.0, state.input.screen_h - 1.0);
            state.input.cursor_pos = (state.input.pointer_x, state.input.pointer_y);
            let (px, py) = (state.input.pointer_x, state.input.pointer_y);

            state.update_interactive_grab(px, py);

            if let Some((surface, sx, sy)) = state.surface_at(px, py) {
                state.seat.set_pointer_focus(Some(surface), sx, sy);
                state.seat.send_pointer_motion(time, sx, sy);
            } else if !state.input.button_held {
                // Only clear focus when no button is held — avoids dropping
                // pointer focus during fast drags that leave the surface rect.
                if matches!(state.grab, crate::state::GrabKind::None) {
                    state.seat.set_pointer_focus(None, 0.0, 0.0);
                }
            }

            state.needs_redraw = true;
        }

        PointerEvent::Button(b) => {
            let button = b.button();
            let time = b.time();
            let pressed = b.button_state() == input::event::pointer::ButtonState::Pressed;
            let wl_state = if pressed {
                wayland_server::protocol::wl_pointer::ButtonState::Pressed
            } else {
                wayland_server::protocol::wl_pointer::ButtonState::Released
            };

            state.input.button_held = pressed;

            if pressed {
                let (px, py) = (state.input.pointer_x, state.input.pointer_y);

                if let Some(id) = state.wm.window_at(px as i32, py as i32) {
                    if state.wm.focused_window() != Some(id) {
                        state.wm.focus_window(id);
                        state.sync_keyboard_focus();
                        state.needs_redraw = true;
                    }
                }

                // Re-establish pointer focus so the click is delivered.
                if let Some((surface, sx, sy)) = state.surface_at(px, py) {
                    state.seat.set_pointer_focus(Some(surface), sx, sy);
                    // Deliver the button event to the now-focused surface.
                    state.seat.send_pointer_button(time, button, wl_state);
                    return;
                }
            } else {
                state.end_grab();
            }

            state.seat.send_pointer_button(time, button, wl_state);
        }

        PointerEvent::ScrollWheel(s) => {
            let time = s.time();
            if s.has_axis(Axis::Vertical) {
                state.seat.send_pointer_axis(
                    time,
                    wayland_server::protocol::wl_pointer::Axis::VerticalScroll,
                    s.scroll_value(Axis::Vertical),
                );
            }
            if s.has_axis(Axis::Horizontal) {
                state.seat.send_pointer_axis(
                    time,
                    wayland_server::protocol::wl_pointer::Axis::HorizontalScroll,
                    s.scroll_value(Axis::Horizontal),
                );
            }
        }

        _ => {}
    }
}

// ── Action dispatch ───────────────────────────────────────────────────────────

fn dispatch_action(state: &mut Axiom, action: Action) {
    match action {
        Action::Lua(fn_name) => {
            if let Err(e) = state.script.fire_keybind(&fn_name) {
                tracing::error!("Lua keybind '{fn_name}': {e}");
            }
        }
        Action::Close => {
            if let Some(id) = state.wm.focused_window() {
                state.close_window(id);
            }
        }
        Action::FocusDir(dir) => {
            state.wm.focus_direction(dir);
            state.sync_keyboard_focus();
        }
        Action::MoveDir(dir) => {
            state.wm.move_direction(dir);
            state.needs_redraw = true;
        }
        Action::SwitchWorkspace(idx) => {
            state.wm.switch_workspace(idx);
            state.needs_redraw = true;
        }
        Action::MoveToWorkspace(idx) => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.move_to_workspace(id, idx);
                state.needs_redraw = true;
            }
        }
        Action::ToggleFloat => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.toggle_float(id);
                state.needs_redraw = true;
            }
        }
        Action::ToggleFullscreen => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.toggle_fullscreen(id);
                state.send_configure_focused();
                state.needs_redraw = true;
            }
        }
        Action::IncMaster => {
            state.wm.inc_master();
            state.needs_redraw = true;
        }
        Action::DecMaster => {
            state.wm.dec_master();
            state.needs_redraw = true;
        }
        Action::Reload => {
            state.reload_config();
        }
        Action::Quit => {
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        Action::Spawn(cmd) => {
            spawn(&cmd);
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns true for pure modifier keysyms that should always pass through.
fn is_modifier_sym(sym: xkb::Keysym) -> bool {
    matches!(
        sym,
        xkb::Keysym::Shift_L
            | xkb::Keysym::Shift_R
            | xkb::Keysym::Control_L
            | xkb::Keysym::Control_R
            | xkb::Keysym::Alt_L
            | xkb::Keysym::Alt_R
            | xkb::Keysym::Super_L
            | xkb::Keysym::Super_R
            | xkb::Keysym::Caps_Lock
            | xkb::Keysym::Num_Lock
            | xkb::Keysym::ISO_Level3_Shift
            | xkb::Keysym::Meta_L
            | xkb::Keysym::Meta_R
            | xkb::Keysym::Hyper_L
            | xkb::Keysym::Hyper_R
    )
}

fn wl_key_state(ks: KeyState) -> wayland_server::protocol::wl_keyboard::KeyState {
    match ks {
        KeyState::Pressed => wayland_server::protocol::wl_keyboard::KeyState::Pressed,
        KeyState::Released => wayland_server::protocol::wl_keyboard::KeyState::Released,
    }
}

fn vt_from_sym(sym: xkb::Keysym) -> Option<u32> {
    match sym {
        xkb::Keysym::F1 => Some(1),
        xkb::Keysym::F2 => Some(2),
        xkb::Keysym::F3 => Some(3),
        xkb::Keysym::F4 => Some(4),
        xkb::Keysym::F5 => Some(5),
        xkb::Keysym::F6 => Some(6),
        xkb::Keysym::F7 => Some(7),
        xkb::Keysym::F8 => Some(8),
        _ => None,
    }
}

fn vt_switch(state: &mut Axiom, vt: u32) {
    tracing::info!("VT switch → vt{vt}");
    for out in &mut state.outputs {
        out.frame_pending = true;
    }
    if let Err(e) = state.backend.session.switch_vt(vt) {
        tracing::warn!("VT switch to vt{vt} failed: {e}");
    }
}

fn combo_string(mods: Mods, sym: xkb::Keysym) -> String {
    let sym_name = xkb::keysym_get_name(sym);
    let mut parts: Vec<&str> = Vec::with_capacity(5);
    if mods.0 & Mods::SUPER.0 != 0 {
        parts.push("super");
    }
    if mods.0 & Mods::ALT.0 != 0 {
        parts.push("alt");
    }
    if mods.0 & Mods::CTRL.0 != 0 {
        parts.push("ctrl");
    }
    if mods.0 & Mods::SHIFT.0 != 0 {
        parts.push("shift");
    }
    let mut combo = parts.join("+");
    if !combo.is_empty() {
        combo.push('+');
    }
    combo.push_str(&sym_name);
    combo
}

fn spawn(cmd: &str) {
    let mut parts = cmd.split_whitespace();
    let Some(prog) = parts.next() else { return };
    let args: Vec<&str> = parts.collect();
    match std::process::Command::new(prog).args(&args).spawn() {
        Ok(c) => tracing::debug!("spawned '{cmd}' pid={}", c.id()),
        Err(e) => tracing::error!("spawn '{cmd}': {e}"),
    }
}
