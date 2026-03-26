use anyhow::{Context, Result};
use calloop::LoopHandle;
use input::event::{
    keyboard::{KeyState, KeyboardEvent, KeyboardEventTrait},
    pointer::{Axis, ButtonState, PointerEvent, PointerEventTrait, PointerScrollEvent},
};
use input::{Event, Libinput, LibinputInterface};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::{Arc, Mutex};
use xkbcommon::xkb;

use crate::wm::{WindowId, WmState};

struct LibinputIface;

impl LibinputInterface for LibinputIface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> std::result::Result<OwnedFd, i32> {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(path)
            .map(OwnedFd::from)
            .map_err(|e| e.raw_os_error().unwrap_or(-1))
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(fd);
    }
}

// ── Keyboard ─────────────────────────────────────────────────────────────────

pub struct KeyboardState {
    pub context: xkb::Context,
    pub keymap: xkb::Keymap,
    pub state: xkb::State,
    /// XKB layout string (e.g. "us", "gb", "de")
    pub layout: String,
    /// XKB variant (e.g. "dvorak", "colemak")
    pub variant: String,
    /// XKB options (e.g. "caps:escape")
    pub options: Option<String>,
}

impl KeyboardState {
    pub fn new() -> Result<Self> {
        Self::new_with_layout("", "", None)
    }

    pub fn new_with_layout(layout: &str, variant: &str, options: Option<&str>) -> Result<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "", // rules
            "", // model
            layout,
            variant,
            // new_from_names wants Option<String>, not Option<&str>
            options.map(str::to_string),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .context("create xkb keymap")?;
        let state = xkb::State::new(&keymap);
        Ok(Self {
            context,
            keymap,
            state,
            layout: layout.to_string(),
            variant: variant.to_string(),
            options: options.map(str::to_string),
        })
    }

    fn process_key(&mut self, keycode: u32, dir: xkb::KeyDirection) {
        let xkb_key = xkb::Keycode::new(keycode + 8);
        self.state.update_key(xkb_key, dir);
    }

    fn combo_for_key(&self, keycode: u32) -> String {
        let xkb_key = xkb::Keycode::new(keycode + 8);
        let mut parts = vec![];
        let mods = [
            (xkb::MOD_NAME_LOGO, "super"),
            (xkb::MOD_NAME_ALT, "alt"),
            (xkb::MOD_NAME_CTRL, "ctrl"),
            (xkb::MOD_NAME_SHIFT, "shift"),
        ];
        for (mod_name, label) in mods {
            let idx = self.keymap.mod_get_index(mod_name);
            if self
                .state
                .mod_index_is_active(idx, xkb::STATE_MODS_EFFECTIVE)
            {
                parts.push(label.to_string());
            }
        }
        let sym = self.state.key_get_one_sym(xkb_key);
        let name = xkb::keysym_get_name(sym).to_lowercase();
        if ![
            "super_l",
            "super_r",
            "alt_l",
            "alt_r",
            "control_l",
            "control_r",
            "shift_l",
            "shift_r",
        ]
        .contains(&name.as_str())
        {
            parts.push(name);
        }
        parts.join("+")
    }

    pub fn keymap_string(&self) -> String {
        self.keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    }

    /// Get current depressed/latched/locked/group modifier state for
    /// sending in wl_keyboard.modifiers events.
    pub fn modifier_state(&self) -> (u32, u32, u32, u32) {
        let dep = self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED);
        let lat = self.state.serialize_mods(xkb::STATE_MODS_LATCHED);
        let lock = self.state.serialize_mods(xkb::STATE_MODS_LOCKED);
        let grp = self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
        (dep, lat, lock, grp)
    }
}

#[derive(Default)]
pub struct FocusedSurface {
    pub keyboard: Option<WindowId>,
    pub pointer: Option<WindowId>,
}

// ── InputState ───────────────────────────────────────────────────────────────

pub struct InputState {
    libinput: Arc<Mutex<Libinput>>,
    pub keyboard: KeyboardState,
    pub pointer_x: f64,
    pub pointer_y: f64,
    pub focus: FocusedSurface,
}

impl InputState {
    pub fn new() -> Result<Self> {
        let mut li = Libinput::new_with_udev(LibinputIface);
        li.udev_assign_seat("seat0").map_err(|_| {
            anyhow::anyhow!(
                "assign seat0 — ensure you have access to /dev/input (udev rules or input group)"
            )
        })?;
        let keyboard = KeyboardState::new()?;
        Ok(Self {
            libinput: Arc::new(Mutex::new(li)),
            keyboard,
            pointer_x: 0.0,
            pointer_y: 0.0,
            focus: FocusedSurface::default(),
        })
    }

    pub fn register_fd(
        &self,
        loop_handle: &LoopHandle<'static, crate::state::Axiom>,
    ) -> Result<()> {
        use calloop::generic::Generic;

        let raw = self.libinput.lock().unwrap().as_raw_fd();
        let dup_raw = unsafe { libc::dup(raw) };
        if dup_raw < 0 {
            anyhow::bail!("dup libinput fd failed");
        }
        let owned = unsafe { OwnedFd::from_raw_fd(dup_raw) };

        loop_handle
            .insert_source(
                Generic::new(owned, calloop::Interest::READ, calloop::Mode::Level),
                |_, _, state: &mut crate::state::Axiom| {
                    let ptr = state as *mut crate::state::Axiom;
                    unsafe { (*ptr).input.dispatch_events(ptr) };
                    Ok(calloop::PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("register libinput: {e}"))?;
        Ok(())
    }

    pub fn dispatch_events(&mut self, axiom: *mut crate::state::Axiom) {
        let events: Vec<Event> = {
            let mut li = self.libinput.lock().unwrap();
            li.dispatch().ok();
            li.by_ref().collect()
        };
        for ev in events {
            self.handle_event(ev, axiom);
        }
    }

    fn handle_event(&mut self, event: Event, axiom: *mut crate::state::Axiom) {
        let state = unsafe { &mut *axiom };

        match event {
            // ── Keyboard ──────────────────────────────────────────────────────
            Event::Keyboard(KeyboardEvent::Key(ev)) => {
                let keycode = ev.key();
                let is_down = matches!(ev.key_state(), KeyState::Pressed);
                self.keyboard.process_key(
                    keycode,
                    if is_down {
                        xkb::KeyDirection::Down
                    } else {
                        xkb::KeyDirection::Up
                    },
                );

                if is_down {
                    let combo = self.keyboard.combo_for_key(keycode);
                    if !combo.is_empty() && state.script.fire_keybind(&combo) {
                        // Compositor consumed this key — don't forward to client
                        return;
                    }
                }

                // Forward to focused client
                self.forward_key(ev.key(), ev.key_state(), ev.time_usec(), state);
            }

            // ── Pointer motion ────────────────────────────────────────────────
            Event::Pointer(PointerEvent::Motion(ev)) => {
                let out_w = state
                    .backend
                    .outputs
                    .first()
                    .map(|o| o.width as f64)
                    .unwrap_or(1920.0);
                let out_h = state
                    .backend
                    .outputs
                    .first()
                    .map(|o| o.height as f64)
                    .unwrap_or(1080.0);
                self.pointer_x = (self.pointer_x + ev.dx()).clamp(0.0, out_w - 1.0);
                self.pointer_y = (self.pointer_y + ev.dy()).clamp(0.0, out_h - 1.0);

                let (px, py) = (self.pointer_x as i32, self.pointer_y as i32);

                // Hit test: find window under new pointer position
                let hit = hit_test(&state.wm, px, py);

                // Check if pointer crossed a window boundary
                let old_focus = self.focus.pointer;
                if hit != old_focus {
                    // Send pointer enter/leave
                    crate::proto::send_pointer_enter(
                        &state.dh,
                        old_focus,
                        hit.unwrap_or(0),
                        self.surface_local_x(&state.wm, hit, px),
                        self.surface_local_y(&state.wm, hit, py),
                    );
                    self.focus.pointer = hit;
                }

                // Send motion to current pointer focus
                if let Some(id) = self.focus.pointer {
                    let sx = self.surface_local_x(&state.wm, Some(id), px);
                    let sy = self.surface_local_y(&state.wm, Some(id), py);
                    crate::proto::send_pointer_motion(&state.dh, id, ev.time_usec(), sx, sy);
                }

                // Request a redraw so the software cursor moves
                state.needs_redraw = true;
            }

            // ── Pointer button ────────────────────────────────────────────────
            Event::Pointer(PointerEvent::Button(ev)) => {
                let (px, py) = (self.pointer_x as i32, self.pointer_y as i32);
                let hit = hit_test(&state.wm, px, py);

                if ev.button_state() == ButtonState::Pressed {
                    if let Some(id) = hit {
                        // Focus on click
                        if state.wm.focused_window() != Some(id) {
                            state.wm.focus_window(id);
                            state.sync_keyboard_focus();
                            state.needs_redraw = true;
                        }
                    }
                }

                // Forward button event to pointer focus
                use wayland_server::protocol::wl_pointer::ButtonState as WlBS;
                let wl_state = match ev.button_state() {
                    ButtonState::Pressed => WlBS::Pressed,
                    ButtonState::Released => WlBS::Released,
                };
                if let Some(id) = self.focus.pointer {
                    crate::proto::send_pointer_button(
                        &state.dh,
                        id,
                        ev.time_usec(),
                        ev.button(),
                        wl_state,
                    );
                }
            }

            // ── Scroll wheel ──────────────────────────────────────────────────
            // The libinput crate deprecated AxisSource/PointerScrollWheelEvent
            // in favour of PointerEvent::ScrollWheel. scroll_value() returns
            // f64 directly (not Option<f64>), so we just compare to 0.0.
            Event::Pointer(PointerEvent::ScrollWheel(ev)) => {
                if let Some(id) = self.focus.pointer {
                    use wayland_server::protocol::wl_pointer::Axis as WlAxis;
                    let v = ev.scroll_value(Axis::Vertical);
                    if v != 0.0 {
                        crate::proto::send_pointer_axis(
                            &state.dh,
                            id,
                            ev.time_usec(),
                            WlAxis::VerticalScroll,
                            v * 10.0,
                        );
                    }
                    let h = ev.scroll_value(Axis::Horizontal);
                    if h != 0.0 {
                        crate::proto::send_pointer_axis(
                            &state.dh,
                            id,
                            ev.time_usec(),
                            WlAxis::HorizontalScroll,
                            h * 10.0,
                        );
                    }
                }
            }

            _ => {}
        }
    }

    fn forward_key(
        &self,
        key: u32,
        key_state: KeyState,
        time_usec: u64,
        axiom: &mut crate::state::Axiom,
    ) {
        use wayland_server::protocol::wl_keyboard::KeyState as WlKS;
        if let Some(id) = self.focus.keyboard {
            crate::proto::send_key_event(
                &axiom.dh,
                id,
                key,
                time_usec,
                match key_state {
                    KeyState::Pressed => WlKS::Pressed,
                    KeyState::Released => WlKS::Released,
                },
            );
        }
    }

    pub fn set_keyboard_focus(
        &mut self,
        id: WindowId,
        dh: &wayland_server::DisplayHandle,
        _wm: &WmState,
    ) {
        // Send leave to previous focus
        if let Some(old_id) = self.focus.keyboard {
            if old_id != id {
                crate::proto::send_keyboard_leave(dh, old_id);
            }
        }
        self.focus.keyboard = Some(id);
        // Send enter to new focus
        crate::proto::send_keyboard_focus(dh, id);
    }

    pub fn clear_keyboard_focus(&mut self, dh: &wayland_server::DisplayHandle) {
        if let Some(old_id) = self.focus.keyboard.take() {
            crate::proto::send_keyboard_leave(dh, old_id);
        }
    }

    pub fn keymap_string(&self) -> String {
        self.keyboard.keymap_string()
    }

    pub fn pointer_pos(&self) -> (f64, f64) {
        (self.pointer_x, self.pointer_y)
    }

    /// Surface-local X coordinate for a window at global px.
    fn surface_local_x(&self, wm: &WmState, id: Option<WindowId>, px: i32) -> f64 {
        let border = wm.config.border_w as i32;
        id.and_then(|id| wm.windows.get(&id))
            .map(|w| (px - w.rect.x - border) as f64)
            .unwrap_or(0.0)
    }

    /// Surface-local Y coordinate for a window at global py.
    fn surface_local_y(&self, wm: &WmState, id: Option<WindowId>, py: i32) -> f64 {
        let border = wm.config.border_w as i32;
        id.and_then(|id| wm.windows.get(&id))
            .map(|w| (py - w.rect.y - border) as f64)
            .unwrap_or(0.0)
    }
}

fn hit_test(wm: &WmState, px: i32, py: i32) -> Option<WindowId> {
    let ws_idx = wm.active_ws();
    let ws = wm.workspaces.get(ws_idx)?;
    // Check focused window first (prioritise it on overlap)
    if let Some(fid) = ws.focused {
        if let Some(win) = wm.windows.get(&fid) {
            if win.rect.contains(px, py) {
                return Some(fid);
            }
        }
    }
    // Then check in reverse stack order (top-most first)
    for &id in ws.windows.iter().rev() {
        if let Some(win) = wm.windows.get(&id) {
            if win.rect.contains(px, py) {
                return Some(id);
            }
        }
    }
    None
}
