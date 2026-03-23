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
    pointer_focus: Option<(WlSurface, f64, f64)>,

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
        // Send leave to the old surface, but only to keyboards owned by the
        // same client as that surface — sending cross-client events panics in
        // wayland-backend ("Attempting to send an event with objects from wrong client").
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
                // keys is a wl_array of currently-pressed keycodes as raw bytes
                let keys: Vec<u8> = Vec::new();
                for kb in &self.keyboards {
                    if kb.is_alive() && kb.client() == surf_client {
                        kb.enter(serial, surf, keys.clone());
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
        // Only forward to the keyboard belonging to the focused surface's client.
        let focus_client =
            self.keyboard_focus
                .as_ref()
                .and_then(|s| if s.is_alive() { s.client() } else { None });
        for kb in &self.keyboards {
            if kb.is_alive() {
                // If we have a focused surface, gate on matching client.
                // If no surface is focused, send to all (e.g. unfocused key release).
                let client_ok = focus_client
                    .as_ref()
                    .map(|fc| kb.client().as_ref() == Some(fc))
                    .unwrap_or(true);
                if client_ok {
                    kb.key(serial, time, keycode, state);
                }
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

    pub fn send_modifiers(&self) {
        for kb in &self.keyboards {
            if kb.is_alive() {
                self.send_modifiers_to(kb);
            }
        }
    }

    // ── Pointer events ────────────────────────────────────────────────────────

    pub fn set_pointer_focus(&mut self, surface: Option<WlSurface>, sx: f64, sy: f64) {
        if let Some((ref old_surf, _, _)) = self.pointer_focus.take() {
            if old_surf.is_alive() {
                let serial = self.next_serial();
                let old_client = old_surf.client();
                for ptr in &self.pointers {
                    if ptr.is_alive() && ptr.client() == old_client {
                        ptr.leave(serial, old_surf);
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
                    }
                }
                self.pointer_focus = Some((surf.clone(), sx, sy));
            }
        }
    }

    pub fn send_pointer_motion(&self, time: u32, sx: f64, sy: f64) {
        for ptr in &self.pointers {
            if ptr.is_alive() {
                ptr.motion(time, sx, sy);
            }
        }
    }

    pub fn send_pointer_button(&mut self, time: u32, button: u32, state: wl_pointer::ButtonState) {
        let serial = self.next_serial();
        for ptr in &self.pointers {
            if ptr.is_alive() {
                ptr.button(serial, time, button, state);
                ptr.frame();
            }
        }
    }

    pub fn send_pointer_axis(&self, time: u32, axis: wl_pointer::Axis, value: f64) {
        for ptr in &self.pointers {
            if ptr.is_alive() {
                ptr.axis(time, axis, value);
                ptr.frame();
            }
        }
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
                    kb.repeat_info(25, 600);
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
        _state: &mut Self,
        _client: &Client,
        _resource: &WlPointer,
        _request: wl_pointer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
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

/// Write keymap to an anonymous fd (memfd_create via libc) and return
/// (OwnedFd, byte_size_including_nul).  No extra crates needed.
fn create_keymap_fd(keymap: &str) -> anyhow::Result<(OwnedFd, u32)> {
    use std::ffi::CString;
    use std::io::Write;

    let bytes = keymap.as_bytes();
    let size = bytes.len() + 1; // +1 for NUL terminator

    // memfd_create(2) — available on Linux 3.17+.
    let name = CString::new("xkb-keymap").unwrap();
    let fd: RawFd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        anyhow::bail!("memfd_create failed: errno {}", unsafe {
            *libc::__errno_location()
        });
    }

    // ftruncate to the required size.
    if unsafe { libc::ftruncate(fd, size as libc::off_t) } < 0 {
        unsafe {
            libc::close(fd);
        }
        anyhow::bail!("ftruncate failed");
    }

    // Write via a dup so the OwnedFd retains ownership of the original.
    let dup_fd = unsafe { libc::dup(fd) };
    if dup_fd < 0 {
        unsafe {
            libc::close(fd);
        }
        anyhow::bail!("dup failed");
    }
    {
        let mut f = unsafe { std::fs::File::from_raw_fd(dup_fd) };
        f.write_all(bytes)?;
        f.write_all(&[0u8])?; // NUL terminator
    }

    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok((owned, size as u32))
}

// ── libc shim ─────────────────────────────────────────────────────────────────

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
