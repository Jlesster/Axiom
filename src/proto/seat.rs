// src/proto/seat.rs — wl_seat, wl_keyboard, wl_pointer.

use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};

use wayland_server::{
    protocol::{
        wl_keyboard::{self, WlKeyboard},
        wl_pointer::{self, WlPointer},
        wl_seat::{self, Capability, WlSeat},
        wl_surface::WlSurface,
    },
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use xkbcommon::xkb;

use crate::{state::Axiom, wm::WindowId};

// ── SeatState ─────────────────────────────────────────────────────────────────

pub struct SeatState {
    pub xkb_state: xkb::State,
    pub keymap_str: String,

    keyboards: Vec<WlKeyboard>,
    pointers: Vec<WlPointer>,

    keyboard_focus: Option<WlSurface>,
    keyboard_focused_win: Option<WindowId>,
    pointer_focus: Option<WlSurface>,

    serial: u32,
}

impl SeatState {
    pub fn new() -> Self {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap =
            xkb::Keymap::new_from_names(&ctx, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
                .expect("default xkb keymap");
        let state = xkb::State::new(&keymap);
        let keymap_str = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        Self {
            xkb_state: state,
            keymap_str,
            keyboards: Vec::new(),
            pointers: Vec::new(),
            keyboard_focus: None,
            keyboard_focused_win: None,
            pointer_focus: None,
            serial: 0,
        }
    }

    pub fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    // ── Keyboard focus ────────────────────────────────────────────────────────

    pub fn keyboard_focus_id(&self) -> Option<WindowId> {
        self.keyboard_focused_win
    }

    pub fn set_keyboard_focus_win(&mut self, win_id: Option<WindowId>) {
        self.keyboard_focused_win = win_id;
    }

    pub fn set_keyboard_focus(&mut self, surface: Option<WlSurface>) {
        // Send leave to old surface's client only.
        if let Some(ref old) = self.keyboard_focus.take() {
            if old.is_alive() {
                let serial = self.next_serial();
                let old_client = old.client();
                for kb in &self.keyboards {
                    if kb.is_alive() && kb.client() == old_client {
                        kb.leave(serial, old);
                    }
                }
            }
        }
        self.keyboard_focused_win = None;

        if let Some(ref surf) = surface {
            if surf.is_alive() {
                let serial = self.next_serial();
                let surf_client = surf.client();
                let keys: Vec<u8> = Vec::new();
                for kb in &self.keyboards {
                    if kb.is_alive() && kb.client() == surf_client {
                        kb.enter(serial, surf, keys.clone());
                        // BUG FIX #1: send current modifier state on enter so
                        // the client immediately knows which modifiers are held.
                        self.send_modifiers_to(kb);
                    }
                }
            }
        }
        self.keyboard_focus = surface;
    }

    // ── Key events ────────────────────────────────────────────────────────────

    pub fn send_key(&mut self, time: u32, keycode: u32, state: wl_keyboard::KeyState) {
        let serial = self.next_serial();
        let focus_client =
            self.keyboard_focus
                .as_ref()
                .and_then(|s| if s.is_alive() { s.client() } else { None });
        for kb in &self.keyboards {
            if !kb.is_alive() {
                continue;
            }
            let client_ok = focus_client
                .as_ref()
                .map(|fc| kb.client().as_ref() == Some(fc))
                .unwrap_or(false); // BUG FIX #2: don't send to all when no focus
            if client_ok {
                kb.key(serial, time, keycode, state);
            }
        }
    }

    fn send_modifiers_to(&self, kb: &WlKeyboard) {
        let s = &self.xkb_state;
        kb.modifiers(
            self.serial,
            s.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            s.serialize_mods(xkb::STATE_MODS_LATCHED),
            s.serialize_mods(xkb::STATE_MODS_LOCKED),
            s.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        );
    }

    /// Send current modifier state to all keyboards belonging to the focused
    /// client. Call this after every key event that changes modifier state.
    pub fn send_modifiers(&self) {
        let focus_client =
            self.keyboard_focus
                .as_ref()
                .and_then(|s| if s.is_alive() { s.client() } else { None });
        for kb in &self.keyboards {
            if !kb.is_alive() {
                continue;
            }
            // BUG FIX #3: only send modifiers to the focused client, not all.
            let client_ok = focus_client
                .as_ref()
                .map(|fc| kb.client().as_ref() == Some(fc))
                .unwrap_or(false);
            if client_ok {
                self.send_modifiers_to(kb);
            }
        }
    }

    // ── Pointer focus ─────────────────────────────────────────────────────────

    pub fn set_pointer_focus(&mut self, surface: Option<WlSurface>, sx: f64, sy: f64) {
        // Avoid redundant enter/leave for the same surface.
        let new_id = surface.as_ref().map(|s| s.id());
        let old_id = self.pointer_focus.as_ref().map(|s| s.id());
        if new_id == old_id {
            return;
        }

        // Leave old surface.
        if let Some(ref old_surf) = self.pointer_focus.take() {
            if old_surf.is_alive() {
                let serial = self.next_serial();
                let old_client = old_surf.client();
                for ptr in &self.pointers {
                    if ptr.is_alive() && ptr.client() == old_client {
                        ptr.leave(serial, old_surf);
                        ptr.frame(); // BUG FIX #4: frame after leave
                    }
                }
            }
        }

        if let Some(ref surf) = surface {
            if surf.is_alive() {
                let serial = self.next_serial();
                let surf_client = surf.client();
                for ptr in &self.pointers {
                    if ptr.is_alive() && ptr.client() == surf_client {
                        ptr.enter(serial, surf, sx, sy);
                        ptr.frame(); // frame after enter
                    }
                }
                self.pointer_focus = Some(surf.clone());
            }
        }
    }

    pub fn send_pointer_motion(&self, time: u32, sx: f64, sy: f64) {
        // BUG FIX #5: only send motion to the focused surface's client.
        let focus_client =
            self.pointer_focus
                .as_ref()
                .and_then(|s| if s.is_alive() { s.client() } else { None });
        for ptr in &self.pointers {
            if !ptr.is_alive() {
                continue;
            }
            let client_ok = focus_client
                .as_ref()
                .map(|fc| ptr.client().as_ref() == Some(fc))
                .unwrap_or(false);
            if client_ok {
                ptr.motion(time, sx, sy);
                ptr.frame(); // BUG FIX #6: frame after motion (required wl_seat >= v5)
            }
        }
    }

    pub fn send_pointer_button(&mut self, time: u32, button: u32, state: wl_pointer::ButtonState) {
        let serial = self.next_serial();
        let focus_client =
            self.pointer_focus
                .as_ref()
                .and_then(|s| if s.is_alive() { s.client() } else { None });
        for ptr in &self.pointers {
            if !ptr.is_alive() {
                continue;
            }
            let client_ok = focus_client
                .as_ref()
                .map(|fc| ptr.client().as_ref() == Some(fc))
                .unwrap_or(false);
            if client_ok {
                ptr.button(serial, time, button, state);
                ptr.frame();
            }
        }
    }

    pub fn send_pointer_axis(&self, time: u32, axis: wl_pointer::Axis, value: f64) {
        let focus_client =
            self.pointer_focus
                .as_ref()
                .and_then(|s| if s.is_alive() { s.client() } else { None });
        for ptr in &self.pointers {
            if !ptr.is_alive() {
                continue;
            }
            let client_ok = focus_client
                .as_ref()
                .map(|fc| ptr.client().as_ref() == Some(fc))
                .unwrap_or(false);
            if client_ok {
                ptr.axis(time, axis, value);
                ptr.frame();
            }
        }
    }

    /// True if `surface` currently holds keyboard focus.
    pub fn has_keyboard_focus(&self, surface: &WlSurface) -> bool {
        self.keyboard_focus
            .as_ref()
            .map(|s| s.id() == surface.id())
            .unwrap_or(false)
    }

    /// Current pointer focus surface (if any).
    pub fn pointer_focus_surface(&self) -> Option<&WlSurface> {
        self.pointer_focus.as_ref().filter(|s| s.is_alive())
    }
}

// ── Global dispatch ───────────────────────────────────────────────────────────

impl GlobalDispatch<WlSeat, ()> for Axiom {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WlSeat>,
        _global_data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let seat = init.init(resource, ());
        seat.capabilities(Capability::Keyboard | Capability::Pointer);
        if seat.version() >= 2 {
            seat.name("seat0".into());
        }
    }
}

impl Dispatch<WlSeat, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_seat::Request::GetKeyboard { id } => {
                let kb = init.init(id, ());
                match create_keymap_fd(&state.seat.keymap_str) {
                    Ok((fd, size)) => {
                        use std::os::unix::io::AsFd;
                        kb.keymap(wl_keyboard::KeymapFormat::XkbV1, fd.as_fd(), size);
                    }
                    Err(e) => tracing::warn!("keymap fd: {e}"),
                }
                if kb.version() >= 4 {
                    // 25 keys/s repeat rate, 600ms delay before repeat starts.
                    kb.repeat_info(25, 600);
                }
                // BUG FIX #7: if a keyboard is created while a surface is
                // already focused (e.g. a client re-creates its keyboard object)
                // immediately send enter + modifiers so it isn't left deaf.
                if let Some(ref surf) = state.seat.keyboard_focus.clone() {
                    if surf.is_alive() {
                        if kb.client() == surf.client() {
                            let serial = state.seat.next_serial();
                            kb.enter(serial, surf, Vec::new());
                            state.seat.send_modifiers_to(&kb);
                        }
                    }
                }
                state.seat.keyboards.push(kb);
            }
            wl_seat::Request::GetPointer { id } => {
                state.seat.pointers.push(init.init(id, ()));
            }
            wl_seat::Request::GetTouch { id: _ } => {}
            wl_seat::Request::Release => {}
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Axiom {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlKeyboard,
        _request: wl_keyboard::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &WlKeyboard,
        _data: &(),
    ) {
        state.seat.keyboards.retain(|kb| kb.id() != resource.id());
    }
}

impl Dispatch<WlPointer, ()> for Axiom {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WlPointer,
        request: wl_pointer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        // BUG FIX #8: handle set_cursor so the client's cursor surface is
        // respected instead of always drawing the white box.
        if let wl_pointer::Request::SetCursor {
            serial: _,
            surface,
            hotspot_x,
            hotspot_y,
        } = request
        {
            state.input.cursor_surface = surface.clone();
            state.input.cursor_hotspot = (hotspot_x, hotspot_y);
            if let Some(surf) = surface {
                // Mark role.
                use crate::proto::compositor::{SurfaceData, SurfaceRole};
                use std::sync::Arc;
                if let Some(sd) = surf.data::<Arc<SurfaceData>>() {
                    *sd.role.lock().unwrap() = SurfaceRole::Cursor;
                }
            }
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &WlPointer,
        _data: &(),
    ) {
        state.seat.pointers.retain(|ptr| ptr.id() != resource.id());
    }
}

// ── Keymap memfd ──────────────────────────────────────────────────────────────

fn create_keymap_fd(keymap: &str) -> anyhow::Result<(OwnedFd, u32)> {
    use std::ffi::CString;
    use std::io::Write;

    let bytes = keymap.as_bytes();
    let size = bytes.len() + 1;

    let name = CString::new("xkb-keymap").unwrap();
    let fd: RawFd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        anyhow::bail!("memfd_create failed: errno {}", unsafe {
            *libc::__errno_location()
        });
    }

    if unsafe { libc::ftruncate(fd, size as libc::off_t) } < 0 {
        unsafe { libc::close(fd) };
        anyhow::bail!("ftruncate failed");
    }

    let dup_fd = unsafe { libc::dup(fd) };
    if dup_fd < 0 {
        unsafe { libc::close(fd) };
        anyhow::bail!("dup failed");
    }
    {
        let mut f = unsafe { std::fs::File::from_raw_fd(dup_fd) };
        f.write_all(bytes)?;
        f.write_all(&[0u8])?;
    }

    Ok((unsafe { OwnedFd::from_raw_fd(fd) }, size as u32))
}

mod libc {
    extern "C" {
        pub fn memfd_create(name: *const std::ffi::c_char, flags: u32) -> i32;
        pub fn ftruncate(fd: i32, length: i64) -> i32;
        pub fn close(fd: i32) -> i32;
        pub fn dup(oldfd: i32) -> i32;
        pub fn __errno_location() -> *mut i32;
    }
    pub use std::os::raw::c_char;
    pub const MFD_CLOEXEC: u32 = 0x0001;
    pub type off_t = i64;
}
