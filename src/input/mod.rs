// src/input/mod.rs — Input pipeline.

use std::collections::HashMap;
use std::os::unix::io::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

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

use wayland_server::Resource as _;

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
            unsafe { libseat_close_device(self.seat, device_id) };
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

// ── Key-repeat state ──────────────────────────────────────────────────────────

/// Compositor-driven key repeat (wl_keyboard.repeat_info was advertised, so
/// we must actually deliver repeated key events).
pub struct RepeatState {
    /// The raw evdev keycode (libinput keycode, NOT xkb keycode) being repeated.
    pub keycode: u32,
    pub time_ms: u32,
    /// Wall-clock time of the last repeat fire.
    pub last_fire: Instant,
    /// Wall-clock time when repeat should first start (after the delay).
    pub start_at: Instant,
    pub active: bool,
}

impl Default for RepeatState {
    fn default() -> Self {
        Self {
            keycode: 0,
            time_ms: 0,
            last_fire: Instant::now(),
            start_at: Instant::now(),
            active: false,
        }
    }
}

impl RepeatState {
    // 600 ms initial delay, then 25 cps = 40 ms between repeats.
    const DELAY_MS: u64 = 600;
    const INTERVAL_MS: u64 = 40; // 1000 / 25

    pub fn press(&mut self, keycode: u32, time_ms: u32) {
        self.keycode = keycode;
        self.time_ms = time_ms;
        let now = Instant::now();
        self.last_fire = now;
        self.start_at = now + Duration::from_millis(Self::DELAY_MS);
        self.active = true;
    }

    pub fn release(&mut self) {
        self.active = false;
    }

    /// Returns a list of (time_ms) values for any repeat events that should fire
    /// right now. Advances internal state accordingly.
    pub fn drain_pending(&mut self) -> Vec<u32> {
        if !self.active {
            return Vec::new();
        }
        let now = Instant::now();
        if now < self.start_at {
            return Vec::new();
        }
        let interval = Duration::from_millis(Self::INTERVAL_MS);
        let mut events = Vec::new();
        while self.last_fire + interval <= now {
            self.last_fire += interval;
            self.time_ms = self.time_ms.wrapping_add(Self::INTERVAL_MS as u32);
            events.push(self.time_ms);
        }
        events
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
    /// Hotspot in surface-local pixels for the current cursor surface.
    pub cursor_hotspot: (i32, i32),
    pub popup_grab: Option<XdgPopup>,
    pub screen_w: f64,
    pub screen_h: f64,
    pub button_held: bool,
    pub repeat: RepeatState,
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
            cursor_hotspot: (0, 0),
            popup_grab: None,
            screen_w: 1920.0,
            screen_h: 1080.0,
            button_held: false,
            repeat: RepeatState::default(),
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

/// Called once per frame tick to fire any pending key-repeat events.
/// Must be called from the main loop before rendering.
pub fn tick_key_repeat(state: &mut Axiom) {
    // Snapshot pending repeat times without holding any locks.
    let times: Vec<u32> = state.input.repeat.drain_pending();
    if times.is_empty() {
        return;
    }
    let keycode = state.input.repeat.keycode;

    // BUG FIX #9: popup grab suppresses repeat delivery to the base window,
    // same as it does for initial key presses.
    if state.input.popup_grab.is_some() {
        return;
    }

    for time in times {
        state.seat.send_key(
            time,
            keycode,
            wayland_server::protocol::wl_keyboard::KeyState::Pressed,
        );
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
    let mod_changed = state.seat.xkb_state.update_key(xkb_keycode, dir);

    let sym = state.seat.xkb_state.key_get_one_sym(xkb_keycode);

    // Always forward modifier keys and always send updated modifier state.
    if is_modifier_sym(sym) {
        let wl_state = wl_key_state(key_state);
        state.seat.send_key(time, keycode, wl_state);
        // BUG FIX #10: send modifiers after every modifier key event, not just
        // on keyboard enter. Without this clients see stale modifier state.
        state.seat.send_modifiers();
        return;
    }

    // BUG FIX #11: send modifiers whenever xkb says state changed, even for
    // non-modifier keys (e.g. Caps Lock as a tap).
    if mod_changed != 0 {
        state.seat.send_modifiers();
    }

    if key_state == KeyState::Pressed {
        let mods = Mods::from_xkb(&state.seat.xkb_state);

        // Emergency quit — always fires regardless of grabs.
        if mods == Mods(Mods::SUPER.0 | Mods::SHIFT.0) && sym == xkb::Keysym::Print {
            tracing::info!("emergency quit (Super+Shift+Print)");
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        }

        // VT switching — always fires regardless of grabs.
        if mods == Mods(Mods::CTRL.0 | Mods::ALT.0) {
            if let Some(vt) = vt_from_sym(sym) {
                vt_switch(state, vt);
                return;
            }
        }

        // BUG FIX #12: if a popup has an exclusive grab, deliver to the popup's
        // surface rather than the compositor keybind table or the base window.
        if state.input.popup_grab.is_some() {
            let wl_state = wl_key_state(key_state);
            state.seat.send_key(time, keycode, wl_state);
            // Start repeat for the grabbed key too.
            if !is_non_repeating(sym) {
                state.input.repeat.press(keycode, time);
            }
            return;
        }

        // Compositor keybinds (Lua first, then native table).
        let combo = combo_string(mods, sym);
        let lua_matched = match crate::scripting::lua_api::fire_keybind(&state.script.lua, &combo) {
            Ok(matched) => matched,
            Err(e) => {
                tracing::error!("keybind '{combo}': {e}");
                false
            }
        };
        if lua_matched {
            let queue = state.script.queue.clone();
            crate::scripting::lua_api::drain(&queue, state);
            state.input.repeat.release(); // compositor consumed the key — no repeat
            return;
        }

        if let Some(action) = state.input.keybinds.lookup(mods, sym).cloned() {
            dispatch_action(state, action);
            state.input.repeat.release(); // compositor consumed
            return;
        }

        // Not a compositor keybind — pass through and start repeat.
        state.seat.send_key(time, keycode, wl_key_state(key_state));
        if !is_non_repeating(sym) {
            state.input.repeat.press(keycode, time);
        }
    } else {
        // Key release — stop repeat and forward to client.
        if state.input.repeat.active && state.input.repeat.keycode == keycode {
            state.input.repeat.release();
        }
        state.seat.send_key(time, keycode, wl_key_state(key_state));
    }
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

            if !matches!(state.grab, crate::state::GrabKind::None) {
                state.update_interactive_grab(px, py);
                state.needs_redraw = true;
                return;
            }

            // BUG FIX #13: use surface_at for pointer focus, which correctly
            // handles the draw order (topmost/focused window on top).
            if let Some((surface, sx, sy)) = state.surface_at(px, py) {
                state.seat.set_pointer_focus(Some(surface.clone()), sx, sy);
                // Only send motion if the surface is still focused after set.
                if state.seat.pointer_focus_surface().map(|s| s.id()) == Some(surface.id()) {
                    state.seat.send_pointer_motion(time, sx, sy);
                }
            } else if !state.input.button_held {
                state.seat.set_pointer_focus(None, 0.0, 0.0);
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

                // BUG FIX #14: close popup grab on click outside the popup.
                // If a popup grab is active and the click is outside its surface,
                // dismiss the popup first (xdg_popup.popup_done), then fall
                // through to normal focus handling.
                if let Some(ref popup) = state.input.popup_grab.clone() {
                    let popup_alive = popup.is_alive();
                    let click_in_popup = popup_alive && {
                        // Find the popup surface in our surface map and check hit.
                        let hit = state.surface_at(px, py);
                        // If the surface under cursor is the popup's wl_surface, allow.
                        // Otherwise dismiss.
                        hit.map(|(surf, _, _)| -> bool {
                            // Compare against popup's underlying wl_surface via xdg_data.
                            use crate::proto::xdg_shell::PopupDataRef;
                            popup
                                .data::<PopupDataRef>()
                                .and_then(|pd| pd.lock().ok())
                                .map(|pd| pd.xdg_data.lock().unwrap().wl_surface.id() == surf.id())
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
                    };

                    if !click_in_popup {
                        popup.popup_done();
                        state.input.clear_popup_grab();
                        // Don't process click further — the dismiss IS the interaction.
                        return;
                    }
                }

                if let Some(win_id) = state.window_at(px, py) {
                    if state.wm.focused_window() != Some(win_id) {
                        state.focus_window_by_click(win_id);
                    }
                }

                if let Some((surface, sx, sy)) = state.surface_at(px, py) {
                    state.seat.set_pointer_focus(Some(surface), sx, sy);
                    state.seat.send_pointer_button(time, button, wl_state);
                    return;
                }
                state.seat.send_pointer_button(time, button, wl_state);
            } else {
                state.end_grab();
                state.seat.send_pointer_button(time, button, wl_state);
            }
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
            if let Some(id) = state.wm.focused_window() {
                if let Some(surf) = state.wm.windows.get(&id).and_then(|w| w.surface.clone()) {
                    state.send_configure_for_surface(&surf, id);
                }
            }
            state.needs_redraw = true;
        }
        Action::MoveDir(dir) => {
            state.wm.move_direction(dir);
            state.send_configure_all();
        }
        Action::SwitchWorkspace(idx) => {
            state.wm.switch_workspace(idx);
            state.send_configure_all();
            state.sync_keyboard_focus();
        }
        Action::MoveToWorkspace(idx) => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.move_to_workspace(id, idx);
                state.send_configure_all();
                state.sync_keyboard_focus();
            }
        }
        Action::ToggleFloat => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.toggle_float(id);
                state.send_configure_all();
            }
        }
        Action::ToggleFullscreen => {
            if let Some(id) = state.wm.focused_window() {
                state.wm.toggle_fullscreen(id);
                if let Some(surf) = state.wm.windows.get(&id).and_then(|w| w.surface.clone()) {
                    state.send_configure_for_surface(&surf, id);
                }
                state.needs_redraw = true;
            }
        }
        Action::IncMaster => {
            state.wm.inc_master();
            state.send_configure_all();
        }
        Action::DecMaster => {
            state.wm.dec_master();
            state.send_configure_all();
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

/// Keys that should not generate repeat events (function keys used as
/// compositor actions are already consumed before reaching this check; this
/// covers protocol-level non-repeating keys).
fn is_non_repeating(sym: xkb::Keysym) -> bool {
    is_modifier_sym(sym)
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
