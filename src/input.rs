// input.rs — raw libinput events → keybind dispatch → TWM actions.
//
// Changes vs original:
//   - Workspace switch dispatches record direction for AnimSet::workspace_transition().
//   - Pointer motion updates state.cursor.pos for hardware cursor tracking.

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
    state::{DragState, Trixie},
    twm::{anim::WsDir, TwmAction},
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
    let sym = keysym
        .raw_syms()
        .first()
        .copied()
        .unwrap_or_else(|| keysym.modified_sym());
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

        // Workspace switches — record direction for slide animation.
        KeyAction::Workspace(n) => {
            let old = state.twm.active_ws;
            state.twm.dispatch(TwmAction::Workspace(n));
            let new = state.twm.active_ws;
            if new != old {
                let dir = if new > old { WsDir::Right } else { WsDir::Left };
                state.anim.workspace_transition(dir);
            }
            state.sync_focus();
        }
        KeyAction::NextWorkspace => {
            let old = state.twm.active_ws;
            state.twm.dispatch(TwmAction::NextWorkspace);
            let new = state.twm.active_ws;
            if new != old {
                state.anim.workspace_transition(WsDir::Right);
            }
            state.sync_focus();
        }
        KeyAction::PrevWorkspace => {
            let old = state.twm.active_ws;
            state.twm.dispatch(TwmAction::PrevWorkspace);
            let new = state.twm.active_ws;
            if new != old {
                state.anim.workspace_transition(WsDir::Left);
            }
            state.sync_focus();
        }

        other => {
            if let Some(a) = key_to_twm(other) {
                // For layout changes, snapshot rects before and diff after.
                let is_layout_change = matches!(
                    &a,
                    TwmAction::NextLayout
                        | TwmAction::PrevLayout
                        | TwmAction::GrowMain
                        | TwmAction::ShrinkMain
                        | TwmAction::ToggleFloat
                );
                let old_rects = if is_layout_change {
                    state.twm.pane_rects_snapshot()
                } else {
                    vec![]
                };

                state.twm.dispatch(a);

                if is_layout_change && !old_rects.is_empty() {
                    let new_rects = state.twm.pane_rects_snapshot();
                    crate::twm::anim::diff_and_morph(&mut state.anim, &old_rects, &new_rects);
                }

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
        ToggleFloat => TwmAction::ToggleFloat,
        NextLayout => TwmAction::NextLayout,
        PrevLayout => TwmAction::PrevLayout,
        GrowMain => TwmAction::GrowMain,
        ShrinkMain => TwmAction::ShrinkMain,
        MoveToWorkspace(n) => TwmAction::MoveToWorkspace(n),
        ToggleBar => TwmAction::ToggleBar,
        ToggleScratchpad(name) => TwmAction::ToggleScratchpad(name),
        _ => return None,
    })
}

// ── Pointer hit testing ───────────────────────────────────────────────────────

fn pane_at(
    state: &Trixie,
    loc: smithay::utils::Point<f64, smithay::utils::Logical>,
) -> Option<crate::twm::PaneId> {
    let px = loc.x as u32;
    let py = loc.y as u32;
    let bw = state.twm.border_w;
    let ws = &state.twm.workspaces[state.twm.active_ws];

    // Floating panes checked first (they sit on top of tiled).
    ws.panes.iter().rev().find_map(|&id| {
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
    })
}

fn window_under(
    state: &Trixie,
    loc: smithay::utils::Point<f64, smithay::utils::Logical>,
) -> Option<(
    WlSurface,
    smithay::utils::Point<f64, smithay::utils::Logical>,
)> {
    let pane_id = pane_at(state, loc)?;

    let window = state.space.elements().find(|w| {
        w.wl_surface()
            .map(|s| state.surface_to_pane.get(&s.as_ref().id()).copied() == Some(pane_id))
            .unwrap_or(false)
    })?;

    let pane = state.twm.panes.get(&pane_id)?;
    let bw = state.twm.border_w;
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

    // Update cursor manager position for hardware cursor plane.
    state.cursor.pos = new_loc;

    // Handle active drag.
    let dx = delta.x as i32;
    let dy = delta.y as i32;
    match state.drag.clone() {
        DragState::Moving(pane_id) => {
            state.twm.dispatch(TwmAction::FloatMove(pane_id, dx, dy));
            state.needs_redraw = true;
            ptr.motion(
                state,
                None,
                &smithay::input::pointer::MotionEvent {
                    location: new_loc,
                    serial,
                    time: event.time_msec(),
                },
            );
            ptr.frame(state);
            return;
        }
        DragState::Resizing(pane_id) => {
            state.twm.dispatch(TwmAction::FloatResize(pane_id, dx, dy));
            state.needs_redraw = true;
            ptr.motion(
                state,
                None,
                &smithay::input::pointer::MotionEvent {
                    location: new_loc,
                    serial,
                    time: event.time_msec(),
                },
            );
            ptr.frame(state);
            return;
        }
        DragState::None => {}
    }

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

    // Update cursor manager position.
    state.cursor.pos = new_loc;

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
    let button = event.button_code();

    const BTN_LEFT: u32 = 272;
    const BTN_RIGHT: u32 = 273;

    if button_state == ButtonState::Released {
        if !matches!(state.drag, crate::state::DragState::None) {
            state.drag = crate::state::DragState::None;
            tracing::debug!("drag ended");
        }
    }

    if button_state == ButtonState::Pressed {
        let loc = ptr.current_location();
        let mods = state
            .seat
            .get_keyboard()
            .map(|k| k.modifier_state())
            .unwrap_or_default();
        let super_held = mods.logo;

        let clicked_pane = pane_at(state, loc);

        if let Some(pane_id) = clicked_pane {
            state.twm.set_focused(pane_id);

            let is_floating = state
                .twm
                .panes
                .get(&pane_id)
                .map(|p| p.floating)
                .unwrap_or(false);

            if super_held && button == BTN_LEFT && is_floating {
                state.drag = crate::state::DragState::Moving(pane_id);
                tracing::debug!("drag move start: pane={pane_id}");
                ptr.button(
                    state,
                    &smithay::input::pointer::ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                ptr.frame(state);
                return;
            }

            if super_held && button == BTN_RIGHT && is_floating {
                state.drag = crate::state::DragState::Resizing(pane_id);
                tracing::debug!("drag resize start: pane={pane_id}");
                ptr.button(
                    state,
                    &smithay::input::pointer::ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                ptr.frame(state);
                return;
            }

            // Normal click — focus the surface.
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
            button,
            state: button_state,
            serial,
            time: event.time_msec(),
        },
    );
    ptr.frame(state);
}
