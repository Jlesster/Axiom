use crate::state::Axiom;
use crate::wm::WindowId;
use wayland_server::{
    protocol::{
        wl_keyboard::{self, WlKeyboard},
        wl_pointer::{self, WlPointer},
        wl_seat::{self, Capability, WlSeat},
        wl_surface::WlSurface,
        wl_touch::{self, WlTouch},
    },
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

// ── Per-client seat data ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct SeatData {}

// ── Global device lists ───────────────────────────────────────────────────────

thread_local! {
    static ALL_KEYBOARDS: std::cell::RefCell<Vec<WlKeyboard>> =
        std::cell::RefCell::new(Vec::new());
    static ALL_POINTERS: std::cell::RefCell<Vec<WlPointer>> =
        std::cell::RefCell::new(Vec::new());
}

// ── GlobalDispatch / Dispatch ─────────────────────────────────────────────────

impl GlobalDispatch<WlSeat, ()> for Axiom {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        res: New<WlSeat>,
        _: &(),
        di: &mut DataInit<'_, Self>,
    ) {
        let seat = di.init(res, SeatData::default());
        seat.capabilities(Capability::Keyboard | Capability::Pointer);
        if seat.version() >= 2 {
            seat.name("seat0".to_string());
        }
    }
}

impl Dispatch<WlSeat, SeatData> for Axiom {
    fn request(
        state: &mut Self,
        _: &Client,
        _seat: &WlSeat,
        req: wl_seat::Request,
        _data: &SeatData,
        _: &DisplayHandle,
        di: &mut DataInit<'_, Self>,
    ) {
        match req {
            wl_seat::Request::GetKeyboard { id } => {
                let kb = di.init(id, ());

                // Send keymap via memfd
                let keymap_str = state.input.keymap_string();
                let keymap_bytes = keymap_str.into_bytes();
                if let Ok(owned_fd) = crate::sys::memfd_create(&keymap_bytes) {
                    use std::os::fd::AsFd;
                    kb.keymap(
                        wl_keyboard::KeymapFormat::XkbV1,
                        owned_fd.as_fd(),
                        keymap_bytes.len() as u32,
                    );
                }

                // Keyboard repeat: 25 cps, 600ms initial delay
                if kb.version() >= 4 {
                    kb.repeat_info(25, 600);
                }

                ALL_KEYBOARDS.with(|v| v.borrow_mut().push(kb));
            }
            wl_seat::Request::GetPointer { id } => {
                let ptr = di.init(id, ());
                ALL_POINTERS.with(|v| v.borrow_mut().push(ptr));
            }
            wl_seat::Request::GetTouch { id } => {
                di.init(id, ());
            }
            wl_seat::Request::Release => {}
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlKeyboard,
        _: wl_keyboard::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<WlPointer, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlPointer,
        req: wl_pointer::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match req {
            // Client requests a cursor image — we note this but don't render
            // a cursor shape yet (that's a future addition with xcursor).
            wl_pointer::Request::SetCursor { .. } => {}
            wl_pointer::Request::Release => {}
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for Axiom {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlTouch,
        _: wl_touch::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

// ── Keyboard focus ────────────────────────────────────────────────────────────

/// Send wl_keyboard.enter to all keyboards belonging to the client that owns
/// window `id`. This is required by the Wayland spec before any key events.
pub fn send_keyboard_focus(_dh: &DisplayHandle, id: WindowId) {
    let surface = match surface_for_window(id) {
        Some(s) => s,
        None => return,
    };
    let target_client = surface.client();

    ALL_KEYBOARDS.with(|v| {
        v.borrow_mut().retain(|kb| kb.is_alive());
        let serial = next_serial();
        for kb in v.borrow().iter() {
            if kb.is_alive() && kb.client() == target_client {
                // keys: currently-held keycodes as raw u32 array.
                // Sending empty is correct and safe — the client re-syncs
                // its key state from subsequent key events.
                kb.enter(serial, &surface, vec![]);
                let mod_serial = next_serial();
                kb.modifiers(mod_serial, 0, 0, 0, 0);
            }
        }
    });
}

/// Send wl_keyboard.leave to all keyboards currently focused on window `id`.
pub fn send_keyboard_leave(_dh: &DisplayHandle, id: WindowId) {
    let surface = match surface_for_window(id) {
        Some(s) => s,
        None => return,
    };
    let target_client = surface.client();

    ALL_KEYBOARDS.with(|v| {
        v.borrow_mut().retain(|kb| kb.is_alive());
        let serial = next_serial();
        for kb in v.borrow().iter() {
            if kb.is_alive() && kb.client() == target_client {
                kb.leave(serial, &surface);
            }
        }
    });
}

/// Clear keyboard focus (no window focused). Sends leave to previously focused.
pub fn clear_keyboard_focus(_dh: &DisplayHandle) {
    // Nothing to do here without tracking the previous focus surface.
    // The next send_keyboard_focus call will implicitly replace focus.
}

/// Forward a key event to the window with `id`.
pub fn send_key_event(
    _dh: &DisplayHandle,
    id: WindowId,
    key: u32,
    time_usec: u64,
    key_state: wl_keyboard::KeyState,
) {
    let surface = match surface_for_window(id) {
        Some(s) => s,
        None => return,
    };
    let target_client = surface.client();
    let time_ms = (time_usec / 1000) as u32;

    ALL_KEYBOARDS.with(|v| {
        for kb in v.borrow().iter() {
            if kb.is_alive() && kb.client() == target_client {
                let key_serial = next_serial();
                kb.key(key_serial, time_ms, key, key_state);
                let mod_serial = next_serial();
                kb.modifiers(mod_serial, 0, 0, 0, 0);
            }
        }
    });
}

// ── Pointer events ────────────────────────────────────────────────────────────

/// Send pointer leave to `old_id` and pointer enter to `new_id`.
pub fn send_pointer_enter(
    _dh: &DisplayHandle,
    old_id: Option<WindowId>,
    new_id: WindowId,
    surface_x: f64,
    surface_y: f64,
) {
    // Leave old
    if let Some(old) = old_id {
        if let Some(old_surf) = surface_for_window(old) {
            let old_client = old_surf.client();
            let serial = next_serial();
            ALL_POINTERS.with(|v| {
                v.borrow_mut().retain(|p| p.is_alive());
                for ptr in v.borrow().iter() {
                    if ptr.is_alive() && ptr.client() == old_client {
                        ptr.leave(serial, &old_surf);
                        if ptr.version() >= 5 {
                            ptr.frame();
                        }
                    }
                }
            });
        }
    }

    // Enter new
    // wayland-server 0.31: ptr.enter() takes f64 for surface coordinates,
    // NOT wl_fixed (i32). Pass surface_x/surface_y directly.
    if let Some(new_surf) = surface_for_window(new_id) {
        let new_client = new_surf.client();
        let serial = next_serial();
        ALL_POINTERS.with(|v| {
            v.borrow_mut().retain(|p| p.is_alive());
            for ptr in v.borrow().iter() {
                if ptr.is_alive() && ptr.client() == new_client {
                    ptr.enter(serial, &new_surf, surface_x, surface_y);
                    if ptr.version() >= 5 {
                        ptr.frame();
                    }
                }
            }
        });
    }
}

/// Send pointer leave without a following enter (pointer left the surface).
pub fn send_pointer_leave(_dh: &DisplayHandle, id: WindowId) {
    let surface = match surface_for_window(id) {
        Some(s) => s,
        None => return,
    };
    let client = surface.client();
    let serial = next_serial();
    ALL_POINTERS.with(|v| {
        v.borrow_mut().retain(|p| p.is_alive());
        for ptr in v.borrow().iter() {
            if ptr.is_alive() && ptr.client() == client {
                ptr.leave(serial, &surface);
                if ptr.version() >= 5 {
                    ptr.frame();
                }
            }
        }
    });
}

/// Send wl_pointer.motion to the window under the pointer.
pub fn send_pointer_motion(
    _dh: &DisplayHandle,
    id: WindowId,
    time_usec: u64,
    surface_x: f64,
    surface_y: f64,
) {
    let surface = match surface_for_window(id) {
        Some(s) => s,
        None => return,
    };
    let client = surface.client();
    let time_ms = (time_usec / 1000) as u32;

    // wayland-server 0.31: ptr.motion() takes f64 coords, not wl_fixed.
    ALL_POINTERS.with(|v| {
        for ptr in v.borrow().iter() {
            if ptr.is_alive() && ptr.client() == client {
                ptr.motion(time_ms, surface_x, surface_y);
                if ptr.version() >= 5 {
                    ptr.frame();
                }
            }
        }
    });
}

/// Send wl_pointer.button to the window under the pointer.
pub fn send_pointer_button(
    _dh: &DisplayHandle,
    id: WindowId,
    time_usec: u64,
    button: u32,
    btn_state: wl_pointer::ButtonState,
) {
    let surface = match surface_for_window(id) {
        Some(s) => s,
        None => return,
    };
    let client = surface.client();
    let time_ms = (time_usec / 1000) as u32;
    let serial = next_serial();

    ALL_POINTERS.with(|v| {
        for ptr in v.borrow().iter() {
            if ptr.is_alive() && ptr.client() == client {
                ptr.button(serial, time_ms, button, btn_state);
                if ptr.version() >= 5 {
                    ptr.frame();
                }
            }
        }
    });
}

/// Send wl_pointer.axis (scroll).
pub fn send_pointer_axis(
    _dh: &DisplayHandle,
    id: WindowId,
    time_usec: u64,
    axis: wl_pointer::Axis,
    value: f64,
) {
    let surface = match surface_for_window(id) {
        Some(s) => s,
        None => return,
    };
    let client = surface.client();
    let time_ms = (time_usec / 1000) as u32;

    // wayland-server 0.31: ptr.axis() takes f64, not wl_fixed.
    ALL_POINTERS.with(|v| {
        for ptr in v.borrow().iter() {
            if ptr.is_alive() && ptr.client() == client {
                ptr.axis(time_ms, axis, value);
                if ptr.version() >= 5 {
                    ptr.frame();
                }
            }
        }
    });
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn surface_for_window(id: WindowId) -> Option<WlSurface> {
    crate::proto::xdg_shell::SURFACE_MAP.with(|m| {
        for tl_ref in m.borrow().values() {
            let tl = tl_ref.lock().unwrap();
            if tl.window_id == Some(id) {
                return Some(tl.wl_surface.clone());
            }
        }
        None
    })
}

fn next_serial() -> u32 {
    static S: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    S.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
