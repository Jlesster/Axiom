// input.rs — raw libinput events → keybind dispatch → TWM actions.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, ButtonState, Event as BackendInputEvent, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerButtonEvent, PointerMotionAbsoluteEvent,
        PointerMotionEvent,
    },
    desktop::Window,
    input::keyboard::{FilterResult, KeysymHandle, ModifiersState},
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::SERIAL_COUNTER as SCOUNTER,
    wayland::seat::WaylandFocus,
};

use crate::{
    config::{KeyAction, KeyCombo, Modifiers},
    session::switch_vt,
    state::Trixie,
    twm::TwmAction,
};

// ── Main dispatcher ────────────────────────────────────────────────────────────

pub fn handle_input<B>(state: &mut Trixie, event: InputEvent<B>)
where
    B: InputBackend,
{
    match event {
        InputEvent::Keyboard { event } => handle_key::<B>(state, event),
        InputEvent::PointerMotion { event } => handle_pointer_motion::<B>(state, event),
        InputEvent::PointerMotionAbsolute { event } => handle_pointer_abs::<B>(state, event),
        InputEvent::PointerButton { event } => handle_pointer_button::<B>(state, event),
        _ => {}
    }
}

// ── Keyboard ──────────────────────────────────────────────────────────────────

fn handle_key<B>(state: &mut Trixie, event: B::KeyboardKeyEvent)
where
    B: InputBackend,
{
    let serial = SCOUNTER.next_serial();
    let Some(kbd) = state.seat.get_keyboard() else {
        return;
    };

    let action = kbd.input(
        state,
        event.key_code(),
        event.state(),
        serial,
        event.time_msec(),
        |state, modifiers, keysym| {
            if event.state() != KeyState::Pressed {
                return FilterResult::Forward;
            }

            // Ctrl+Alt+Fn → VT switch
            if modifiers.ctrl && modifiers.alt {
                let raw = keysym.modified_sym().raw();
                let base: u32 = 0x1008FE01;
                if raw >= base && raw <= base + 11 {
                    let n = (raw - base + 1) as i32;
                    return FilterResult::Intercept(KeyAction::SwitchVt(n));
                }
            }

            // Super+Shift+Print → emergency quit
            {
                let sym = keysym.modified_sym();
                if modifiers.logo && modifiers.shift && sym.raw() == 0xff61 {
                    return FilterResult::Intercept(KeyAction::EmergencyQuit);
                }
            }

            let combo = keysym_to_combo(modifiers, &keysym);
            if let Some(combo) = combo {
                for (bound, action) in &state.config.keybinds.clone() {
                    if bound == &combo {
                        return FilterResult::Intercept(action.clone());
                    }
                }
            }
            FilterResult::Forward
        },
    );

    if let Some(action) = action {
        dispatch_action(state, action);
    }
}

fn keysym_to_combo(mods: &ModifiersState, keysym: &KeysymHandle<'_>) -> Option<KeyCombo> {
    use smithay::input::keyboard::xkb;
    let sym = keysym.modified_sym();
    let key = xkb::keysym_get_name(sym).to_lowercase();
    if key == "nosymbol" || key.is_empty() {
        return None;
    }

    let mut m = Modifiers::empty();
    if mods.logo {
        m |= Modifiers::SUPER;
    }
    if mods.ctrl {
        m |= Modifiers::CTRL;
    }
    if mods.alt {
        m |= Modifiers::ALT;
    }
    if mods.shift {
        m |= Modifiers::SHIFT;
    }

    Some(KeyCombo { mods: m, key })
}

// ── Action dispatch ───────────────────────────────────────────────────────────

fn dispatch_action(state: &mut Trixie, action: KeyAction) {
    match action {
        KeyAction::EmergencyQuit => {
            tracing::warn!("Emergency quit triggered (Super+Shift+Print)");
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        KeyAction::SwitchVt(n) => switch_vt(state, n),
        KeyAction::Exec(cmd, args) => state.spawn(&cmd, &args),
        KeyAction::Quit => {
            state
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        KeyAction::Reload => state.apply_config_reload(),
        other => {
            if let Some(a) = key_to_twm(other) {
                state.twm.dispatch(a);
                state.sync_focus();
            }
        }
    }
}

fn key_to_twm(action: KeyAction) -> Option<TwmAction> {
    use KeyAction::*;
    Some(match action {
        FocusLeft => TwmAction::FocusLeft,
        FocusRight => TwmAction::FocusRight,
        FocusUp => TwmAction::FocusUp,
        FocusDown => TwmAction::FocusDown,
        MoveLeft => TwmAction::MoveLeft,
        MoveRight => TwmAction::MoveRight,
        MoveUp => TwmAction::MoveUp,
        MoveDown => TwmAction::MoveDown,
        Close => TwmAction::Close,
        Fullscreen => TwmAction::Fullscreen,
        NextLayout => TwmAction::NextLayout,
        PrevLayout => TwmAction::PrevLayout,
        GrowMain => TwmAction::GrowMain,
        ShrinkMain => TwmAction::ShrinkMain,
        Workspace(n) => TwmAction::Workspace(n),
        MoveToWorkspace(n) => TwmAction::MoveToWorkspace(n),
        NextWorkspace => TwmAction::NextWorkspace,
        PrevWorkspace => TwmAction::PrevWorkspace,
        ToggleBar => TwmAction::ToggleBar,
        _ => return None,
    })
}

// ── Pointer hit testing ───────────────────────────────────────────────────────

fn window_under(
    state: &Trixie,
    loc: smithay::utils::Point<f64, smithay::utils::Logical>,
) -> Option<(
    WlSurface,
    smithay::utils::Point<f64, smithay::utils::Logical>,
)> {
    let px = loc.x as u32;
    let py = loc.y as u32;
    let bw = state.twm.border_w;
    let ws = &state.twm.workspaces[state.twm.active_ws];

    let pane_id = ws.panes.iter().rev().find_map(|&id| {
        let pane = state.twm.panes.get(&id)?;
        let inner = if pane.fullscreen || bw == 0 {
            pane.rect
        } else {
            pane.rect.inset(bw)
        };
        if inner.contains(px, py) {
            Some(id)
        } else {
            None
        }
    })?;

    let window = state.space.elements().find(|w| {
        w.wl_surface()
            .map(|s| state.surface_to_pane.get(&s.as_ref().id()).copied() == Some(pane_id))
            .unwrap_or(false)
    })?;

    let pane = state.twm.panes.get(&pane_id)?;
    let inner = if pane.fullscreen || bw == 0 {
        pane.rect
    } else {
        pane.rect.inset(bw)
    };

    let surf_local = smithay::utils::Point::<f64, smithay::utils::Logical>::from((
        loc.x - inner.x as f64,
        loc.y - inner.y as f64,
    ));

    let surf = window.wl_surface()?.into_owned();
    Some((surf, surf_local))
}

// ── Pointer motion ────────────────────────────────────────────────────────────

fn handle_pointer_motion<B>(state: &mut Trixie, event: B::PointerMotionEvent)
where
    B: InputBackend,
{
    let serial = SCOUNTER.next_serial();
    let delta = event.delta();
    let Some(ptr) = state.seat.get_pointer() else {
        return;
    };

    let current = ptr.current_location();
    let new_loc = smithay::utils::Point::<f64, smithay::utils::Logical>::from((
        (current.x + delta.x)
            .max(0.0)
            .min(state.twm.screen_w as f64),
        (current.y + delta.y)
            .max(0.0)
            .min(state.twm.screen_h as f64),
    ));

    let under = window_under(state, new_loc);

    ptr.motion(
        state,
        under,
        &smithay::input::pointer::MotionEvent {
            location: new_loc,
            serial,
            time: event.time_msec(),
        },
    );
    // wl_pointer.frame must be sent after every group of pointer events.
    // Without this clients buffer the enter/motion events indefinitely and
    // never process them — this was preventing windows from receiving input.
    ptr.frame(state);
}

fn handle_pointer_abs<B>(state: &mut Trixie, event: B::PointerMotionAbsoluteEvent)
where
    B: InputBackend,
{
    let serial = SCOUNTER.next_serial();
    let new_loc = smithay::utils::Point::<f64, smithay::utils::Logical>::from((
        event.x_transformed(state.twm.screen_w as i32),
        event.y_transformed(state.twm.screen_h as i32),
    ));

    let Some(ptr) = state.seat.get_pointer() else {
        return;
    };
    let under = window_under(state, new_loc);

    ptr.motion(
        state,
        under,
        &smithay::input::pointer::MotionEvent {
            location: new_loc,
            serial,
            time: event.time_msec(),
        },
    );
    // Same as above — frame event required to flush the event group.
    ptr.frame(state);
}

// ── Pointer button ────────────────────────────────────────────────────────────

fn handle_pointer_button<B>(state: &mut Trixie, event: B::PointerButtonEvent)
where
    B: InputBackend,
{
    let serial = SCOUNTER.next_serial();
    let Some(ptr) = state.seat.get_pointer() else {
        return;
    };
    let button_state = event.state();

    if button_state == ButtonState::Pressed {
        let loc = ptr.current_location();
        let px = loc.x as u32;
        let py = loc.y as u32;
        let bw = state.twm.border_w;
        let ws_idx = state.twm.active_ws;

        let clicked_pane = state.twm.workspaces[ws_idx]
            .panes
            .iter()
            .rev()
            .find_map(|&id| {
                let pane = state.twm.panes.get(&id)?;
                let inner = if pane.fullscreen || bw == 0 {
                    pane.rect
                } else {
                    pane.rect.inset(bw)
                };
                if inner.contains(px, py) {
                    Some(id)
                } else {
                    None
                }
            });

        if let Some(pane_id) = clicked_pane {
            state.twm.set_focused(pane_id);

            let surf = state.surface_to_pane.iter().find_map(|(oid, &pid)| {
                if pid != pane_id {
                    return None;
                }
                state.space.elements().find_map(|w| {
                    let s = w.wl_surface()?;
                    if s.as_ref().id() == *oid {
                        Some(s.into_owned())
                    } else {
                        None
                    }
                })
            });

            if let Some(surf) = surf {
                if let Some(kbd) = state.seat.get_keyboard() {
                    kbd.set_focus(state, Some(surf), serial);
                }
            }
        }
    }

    ptr.button(
        state,
        &smithay::input::pointer::ButtonEvent {
            button: event.button_code(),
            state: button_state,
            serial,
            time: event.time_msec(),
        },
    );
    // Frame event required after button events too.
    ptr.frame(state);
}
