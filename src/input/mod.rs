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
    // Process events directly from the iterator — no intermediate Vec allocation.
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

    if key_state == KeyState::Pressed {
        let sym = state.seat.xkb_state.key_get_one_sym(xkb_keycode);
        let mods = Mods::from_xkb(&state.seat.xkb_state);

        // ── Hard-coded emergency binds (not overridable from Lua) ─────────────

        // Super+Shift+Print → immediate quit (always works, even if Lua is broken)
        if mods == Mods(Mods::SUPER.0 | Mods::SHIFT.0) && sym == xkb::Keysym::Print {
            tracing::info!("emergency quit (Super+Shift+Print)");
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        }

        // Ctrl+Alt+F1–F8 → VT switch
        if mods == Mods(Mods::CTRL.0 | Mods::ALT.0) {
            if let Some(vt) = vt_from_sym(sym) {
                vt_switch(state, vt);
                return;
            }
        }

        if let Some(action) = state.input.keybinds.lookup(mods, sym).cloned() {
            dispatch_action(state, action);
            return;
        }
    }

    let wl_state = match key_state {
        KeyState::Pressed => wayland_server::protocol::wl_keyboard::KeyState::Pressed,
        KeyState::Released => wayland_server::protocol::wl_keyboard::KeyState::Released,
    };
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
            if let Some((surface, sx, sy)) = state.surface_at(px, py) {
                state.seat.set_pointer_focus(Some(surface), sx, sy);
                state.seat.send_pointer_motion(time, sx, sy);
            } else {
                state.seat.set_pointer_focus(None, 0.0, 0.0);
            }
            state.update_interactive_grab(px, py);
        }

        PointerEvent::Button(b) => {
            let button = b.button();
            let time = b.time();
            let wl_state = if b.button_state() == input::event::pointer::ButtonState::Pressed {
                wayland_server::protocol::wl_pointer::ButtonState::Pressed
            } else {
                wayland_server::protocol::wl_pointer::ButtonState::Released
            };
            if wl_state == wayland_server::protocol::wl_pointer::ButtonState::Pressed {
                let (px, py) = (state.input.pointer_x as i32, state.input.pointer_y as i32);
                if let Some(id) = state.wm.window_at(px, py) {
                    state.wm.focus_window(id);
                    state.sync_keyboard_focus();
                }
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

// ── VT switching ──────────────────────────────────────────────────────────────

/// Map XKB F1–F8 keysyms to VT numbers 1–8.
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

/// Switch to `vt`, pausing DRM rendering first.
///
/// Flow:
///   1. Mark all outputs `frame_pending` so render_all skips them.
///   2. Ask libseat to switch VT — libseat will call our `disable_seat`
///      callback asynchronously, which sets `disable_pending`.  We don't
///      need to do anything extra here; the main-loop session handler
///      already responds to `take_disable_pending()` by keeping
///      `frame_pending = true` on all outputs.
///   3. When the user switches back, libseat calls `enable_seat` → the
///      session fd becomes readable → the calloop source fires
///      `session.dispatch()` → libseat re-enables the seat.  At that point
///      we clear `frame_pending` so rendering resumes.
///
/// Resuming is handled in the existing seat-fd calloop source in main.rs
/// via a new `take_enable_pending()` call (see below).
fn vt_switch(state: &mut Axiom, vt: u32) {
    tracing::info!("VT switch → vt{vt}");

    // Pause rendering before we lose DRM master.
    for out in &mut state.outputs {
        out.frame_pending = true;
    }

    if let Err(e) = state.backend.session.switch_vt(vt) {
        tracing::warn!("VT switch to vt{vt} failed: {e}");
        // Rendering will remain paused until libseat re-enables us anyway,
        // so no need to undo frame_pending here.
    }
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
