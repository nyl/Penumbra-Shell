//! Input routing for the native DRM backend.
//!
//! libinput reports absolute touch and pointer positions in physical panel
//! pixels. They are mapped onto the logical phone canvas and delivered through
//! the seat's pointer and keyboard. Touch is emulated with a single pointer: the first finger
//! drives motion and `BTN_LEFT`, which is what layer-shell and xdg clients expect.
//!
//! Android system-wide lateral edge swipes (left/right screen edges) trigger Back navigation.
//! Hardware Power button toggles screen blanking on short press and opens the Power Dialog on hold.
//! Hardware Volume Up / Down buttons adjust system audio volume via `pactl`.

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, ButtonState, InputEvent, KeyState, KeyboardKeyEvent,
            Keycode, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, TouchEvent,
        },
        libinput::LibinputInputBackend,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent, PointerHandle},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Serial},
};

use crate::{
    native::Canvas,
    phone::{self, card_transform, runtime_path, write_atomic, CARD_STEP, NAV_BAR_Y, PHONE_H, PHONE_W, STATUS_BAR_H, PhoneMode},
    server::PenumbraServerState,
};

/// Linux `BTN_LEFT` key code.
const BTN_LEFT: u32 = 0x110;

/// Linux hardware key codes (both raw evdev and Smithay/XKB scancodes).
const KEY_POWER_RAW: u32 = 116;
const KEY_POWER_XKB: u32 = 124;
const KEY_VOLUMEUP_RAW: u32 = 115;
const KEY_VOLUMEUP_XKB: u32 = 123;
const KEY_VOLUMEDOWN_RAW: u32 = 114;
const KEY_VOLUMEDOWN_XKB: u32 = 122;

/// Focus target and origin for a pointer position.
type Focus = Option<(WlSurface, Point<f64, Logical>)>;

/// The finger currently driving the emulated pointer.
struct TouchContact {
    slot: i32,
    /// Surface that received the touch-down; motion stays with it until release.
    focus: Focus,
}

/// Tracks a potential Android lateral back swipe from screen edges.
struct EdgeBackGesture {
    slot: i32,
    start_pos: Point<f64, Logical>,
    from_left: bool,
    triggered: bool,
}

/// Tracks a touch gesture driving navigation (bottom nav bar or Overview switcher).
pub(crate) struct NavTouchTracker {
    pub slot: i32,
    pub start_pos: Point<f64, Logical>,
    pub start_mode: PhoneMode,
    pub start_scroll: f64,
    pub grabbed_card: Option<usize>,
    pub tracker: penumbra_gestures::VelocityTracker,
}

/// Input bookkeeping for the native panel.
pub(crate) struct InputState {
    pointer: PointerHandle<PenumbraServerState>,
    canvas: Canvas,
    /// Panel size in physical pixels.
    panel: (i32, i32),
    serial: u32,
    started: std::time::Instant,
    touch: Option<TouchContact>,
    edge_gesture: Option<EdgeBackGesture>,
    pub nav_tracker: Option<NavTouchTracker>,
    pub power_press_time: Option<std::time::Instant>,
    pub power_dialog_triggered: bool,
    pub last_tap_time: Option<std::time::Instant>,
    pub last_tap_pos: Option<Point<f64, Logical>>,
}

impl InputState {
    pub(crate) fn new(
        pointer: PointerHandle<PenumbraServerState>,
        canvas: Canvas,
        panel: (i32, i32),
    ) -> Self {
        Self {
            pointer,
            canvas,
            panel,
            serial: 0,
            started: std::time::Instant::now(),
            touch: None,
            edge_gesture: None,
            nav_tracker: None,
            power_press_time: None,
            power_dialog_triggered: false,
            last_tap_time: None,
            last_tap_pos: None,
        }
    }

    pub fn next_serial(&mut self) -> Serial {
        self.serial = self.serial.wrapping_add(1);
        self.serial.into()
    }

    pub fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    /// Maps a physical panel pixel onto the logical canvas, clamped to its bounds.
    fn panel_point(&self, x: f64, y: f64) -> Point<f64, Logical> {
        let (lx, ly) = self.canvas.to_logical(x, y);
        clamp_to_canvas(lx, ly)
    }
}

fn clamp_to_canvas(x: f64, y: f64) -> Point<f64, Logical> {
    (
        x.clamp(0.0, (PHONE_W - 1) as f64),
        y.clamp(0.0, (PHONE_H - 1) as f64),
    )
        .into()
}

/// Moves the seat pointer to `pos`, delivering the motion to `focus`.
fn send_motion(
    server: &mut PenumbraServerState,
    input: &mut InputState,
    pos: Point<f64, Logical>,
    focus: Focus,
) {
    server.pointer_location = pos;
    let event = MotionEvent {
        location: pos,
        serial: input.next_serial(),
        time: input.time(),
    };
    input.pointer.motion(server, focus, &event);
    input.pointer.frame(server);
}

fn send_button(
    server: &mut PenumbraServerState,
    input: &mut InputState,
    button: u32,
    state: ButtonState,
) {
    let event = ButtonEvent {
        button,
        state,
        serial: input.next_serial(),
        time: input.time(),
    };
    input.pointer.button(server, &event);
    input.pointer.frame(server);
}

/// Hover or mouse motion: focus follows the position under the pointer.
fn hover(server: &mut PenumbraServerState, input: &mut InputState, pos: Point<f64, Logical>) {
    let focus = server.phone_focus_under(pos);
    send_motion(server, input, pos, focus);
}

/// Releases the emulated button when the tracked finger lifts or is cancelled.
fn end_touch(server: &mut PenumbraServerState, input: &mut InputState, slot: i32) {
    if !matches!(input.touch, Some(ref contact) if contact.slot == slot) {
        return;
    }
    input.touch = None;
    send_button(server, input, BTN_LEFT, ButtonState::Released);
}

fn handle_keyboard(
    server: &mut PenumbraServerState,
    input: &mut InputState,
    keycode: Keycode,
    state: KeyState,
) {
    let key: u32 = keycode.into();
    tracing::debug!(key, ?state, "Hardware key event");
    match key {
        KEY_POWER_RAW | KEY_POWER_XKB => match state {
            KeyState::Pressed => {
                input.power_press_time = Some(std::time::Instant::now());
                input.power_dialog_triggered = false;
            }
            KeyState::Released => {
                if let Some(start) = input.power_press_time.take() {
                    let elapsed = start.elapsed();
                    if input.power_dialog_triggered {
                        // Already handled by hold detector
                    } else if elapsed >= std::time::Duration::from_millis(500) {
                        tracing::info!(
                            elapsed_ms = elapsed.as_millis(),
                            "Power button long-press -> Power Dialog"
                        );
                        server.show_power_dialog();
                    } else {
                        tracing::info!(
                            elapsed_ms = elapsed.as_millis(),
                            "Power button short-press -> Toggle Screen Blanking"
                        );
                        server.toggle_screen_blanking();
                    }
                }
            }
        },
        KEY_VOLUMEUP_RAW | KEY_VOLUMEUP_XKB => {
            if state == KeyState::Pressed {
                tracing::info!("Hardware Volume Up button pressed (+5%)");
                server.adjust_volume(0.05);
            }
        }
        KEY_VOLUMEDOWN_RAW | KEY_VOLUMEDOWN_XKB => {
            if state == KeyState::Pressed {
                tracing::info!("Hardware Volume Down button pressed (-5%)");
                server.adjust_volume(-0.05);
            }
        }
        _ => {
            if let Some(keyboard) = server.seat.get_keyboard() {
                let serial = input.next_serial();
                let time = input.time();
                keyboard.input::<(), _>(
                    server,
                    keycode,
                    state,
                    serial,
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }
    }
}

/// Routes one libinput event into the phone compositor.
pub(crate) fn handle(
    server: &mut PenumbraServerState,
    input: &mut InputState,
    event: InputEvent<LibinputInputBackend>,
) {
    match event {
        InputEvent::PointerMotion { event } => {
            if server.screen_blanked {
                return;
            }
            let delta = event.delta();
            let pos = clamp_to_canvas(
                server.pointer_location.x + delta.x,
                server.pointer_location.y + delta.y,
            );
            hover(server, input, pos);
        }
        InputEvent::PointerMotionAbsolute { event } => {
            if server.screen_blanked {
                return;
            }
            let (width, height) = input.panel;
            let pos = input.panel_point(
                event.x_transformed(width),
                event.y_transformed(height),
            );
            hover(server, input, pos);
        }
        InputEvent::PointerButton { event } => {
            if server.screen_blanked {
                return;
            }
            send_button(server, input, event.button_code(), event.state());
        }
        InputEvent::PointerAxis { event } => {
            if server.screen_blanked {
                return;
            }
            let horizontal = event.amount(Axis::Horizontal).unwrap_or(0.0);
            let vertical = event.amount(Axis::Vertical).unwrap_or(0.0);

            let mut frame = AxisFrame::new(input.time()).source(event.source());
            if horizontal != 0.0 {
                frame = frame.value(Axis::Horizontal, horizontal);
            }
            if vertical != 0.0 {
                frame = frame.value(Axis::Vertical, vertical);
            }
            input.pointer.axis(server, frame);
            input.pointer.frame(server);
        }
        InputEvent::TouchDown { event } => {
            let slot: i32 = event.slot().into();
            let (width, height) = input.panel;
            let pos = input.panel_point(
                event.x_transformed(width),
                event.y_transformed(height),
            );

            // Double-tap to wake when screen is blanked:
            if server.screen_blanked {
                let now = std::time::Instant::now();
                if let (Some(last_time), Some(last_pos)) = (input.last_tap_time, input.last_tap_pos) {
                    let elapsed = now.duration_since(last_time);
                    let dx = (pos.x - last_pos.x).abs();
                    let dy = (pos.y - last_pos.y).abs();
                    if elapsed <= std::time::Duration::from_millis(400) && dx < 50.0 && dy < 50.0 {
                        tracing::info!(elapsed_ms = elapsed.as_millis(), "Double tap to wake recognized -> unblanking screen");
                        server.toggle_screen_blanking();
                        input.last_tap_time = None;
                        input.last_tap_pos = None;
                        return;
                    }
                }
                input.last_tap_time = Some(now);
                input.last_tap_pos = Some(pos);
                return;
            }

            input.last_tap_time = None;
            input.last_tap_pos = None;

            // Detect Android system-wide lateral back gesture starting from left or right edge.
            // Edge zone: 28px margin.
            // Vertical zone: 35.0..873.0 (excluding status bar top and nav bar bottom).
            const EDGE_ZONE: f64 = 28.0;
            let is_left_edge = pos.x <= EDGE_ZONE;
            let is_right_edge = pos.x >= (PHONE_W as f64 - EDGE_ZONE);
            let in_vertical_zone = pos.y >= 35.0 && pos.y <= (NAV_BAR_Y as f64 - 10.0);

            if (is_left_edge || is_right_edge) && in_vertical_zone && input.edge_gesture.is_none() {
                input.edge_gesture = Some(EdgeBackGesture {
                    slot,
                    start_pos: pos,
                    from_left: is_left_edge,
                    triggered: false,
                });
                return;
            }

            // Navigation bar or Overview gesture tracking:
            // Intercept if touching the generous bottom navigation swipe area (y >= 835.0, full width)
            // OR if already in Overview mode (where the entire screen is an interactive task switcher).
            let is_nav_zone = pos.y >= (NAV_BAR_Y as f64 - 48.0);
            let is_overview = server.shell_state.mode == PhoneMode::Overview;
            let in_overview_zone = is_overview && pos.y >= STATUS_BAR_H as f64;

            if (is_nav_zone && (!server.phone_tasks.is_empty() || server.phone_app_visible() || is_overview || server.shell_state.mode == PhoneMode::App)) || in_overview_zone {
                if input.nav_tracker.is_none() {
                    let mut grabbed_card = None;
                    if is_overview {
                        for (index, _) in server.phone_tasks.iter().enumerate() {
                            let (cx, cy, cw, ch, _) = card_transform(
                                index,
                                server.overview_scroll,
                                server.shell_state.overview_index,
                                0.0,
                            );
                            if pos.x >= cx as f64
                                && pos.x <= (cx + cw) as f64
                                && pos.y >= cy as f64
                                && pos.y <= (cy + ch) as f64
                            {
                                grabbed_card = Some(index);
                                break;
                            }
                        }
                    }

                    let mut tracker = penumbra_gestures::VelocityTracker::new(0.8);
                    tracker.start(pos.x, pos.y, std::time::Duration::ZERO);
                    input.nav_tracker = Some(NavTouchTracker {
                        slot,
                        start_pos: pos,
                        start_mode: server.shell_state.mode,
                        start_scroll: server.shell_state.overview_scroll,
                        grabbed_card,
                        tracker,
                    });
                    if !is_overview {
                        server.nav_gesture_active = true;
                        server.nav_gesture_progress = 0.0;
                        server.nav_gesture_dy = 0.0;
                        server.nav_gesture_vy = 0.0;
                        server.nav_spring = None;
                    }
                    return;
                }
            }

            // Only the first finger drives the emulated pointer.
            if input.touch.is_some() {
                return;
            }
            let focus = server.phone_focus_under(pos);
            input.touch = Some(TouchContact {
                slot,
                focus: focus.clone(),
            });
            send_motion(server, input, pos, focus.clone());
            send_button(server, input, BTN_LEFT, ButtonState::Pressed);

            let serial = input.next_serial();
            if let Some((ref target_surf, _)) = focus {
                let is_kbd = smithay::desktop::layer_map_for_output(&server.output_a)
                    .layers()
                    .find(|l| l.wl_surface() == target_surf)
                    .map(|l| {
                        let ns = l.namespace();
                        ns.contains("keyboard") || ns.contains("osk") || ns.contains("squeekboard") || ns.contains("maliit")
                    })
                    .unwrap_or(false);
                if !is_kbd {
                    if let Some(keyboard) = server.seat.get_keyboard() {
                        keyboard.set_focus(server, Some(target_surf.clone()), serial);
                    }
                }
            }
        }
        InputEvent::TouchMotion { event } => {
            if server.screen_blanked {
                return;
            }
            let slot: i32 = event.slot().into();
            let (width, height) = input.panel;
            let pos = input.panel_point(
                event.x_transformed(width),
                event.y_transformed(height),
            );

            if let Some(ref mut nav) = input.nav_tracker {
                if nav.slot == slot {
                    let elapsed = input.started.elapsed();
                    let (vx, vy) = nav.tracker.update(pos.x, pos.y, elapsed);
                    let dx = pos.x - nav.start_pos.x;
                    let dy = pos.y - nav.start_pos.y;

                    if nav.start_mode == PhoneMode::Overview {
                        // Case A: Downward pull gesture to restore active application to fullscreen
                        if dy > 15.0 && dy > dx.abs() * 1.1 {
                            if let Some(card_idx) = nav.grabbed_card {
                                server.shell_state.overview_index = card_idx;
                                server.shell_state.overview_scroll = card_idx as f64;
                                server.overview_scroll = card_idx as f64;
                            }
                            server.nav_gesture_active = true;
                            server.shell_state.is_dragging = false;
                            server.shell_state.active_card_dy = 0.0;
                            let denom = (PHONE_H as f64) * 0.45;
                            let active_dy = (dy - 15.0).max(0.0);
                            let t = (1.0 - (active_dy / denom)).clamp(0.0, 1.0);
                            server.nav_gesture_progress = t;
                            server.nav_gesture_dy = dy;
                            server.nav_gesture_vy = vy;
                        }
                        // Case B: Upward drag to dismiss active card ONLY if grabbed_card is Some
                        else if nav.grabbed_card.is_some() && dy < -15.0 && (-dy) > dx.abs() * 0.8 {
                            server.nav_gesture_active = false;
                            server.shell_state.is_dragging = true;
                            server.shell_state.active_card_dy = dy;
                        }
                        // Case B2: Upward drag from bottom nav bar in Overview mode -> cancel switcher to Home
                        else if nav.grabbed_card.is_none() && nav.start_pos.y >= (NAV_BAR_Y as f64 - 48.0) && dy < -15.0 && (-dy) > dx.abs() * 0.8 {
                            server.shell_state.is_dragging = false;
                            server.nav_gesture_active = true;
                            let denom = (PHONE_H as f64) * 0.35;
                            let t = (-dy / denom).clamp(0.0, 1.0);
                            server.nav_gesture_progress = t;
                            server.nav_gesture_dy = dy;
                            server.nav_gesture_vy = vy;
                        }
                        // Case C: Horizontal carousel scrubbing
                        else if dx.abs() > 8.0 {
                            server.nav_gesture_active = false;
                            server.shell_state.is_dragging = true;
                            server.shell_state.active_card_dy = 0.0;
                            let delta_cards = dx / (CARD_STEP as f64);
                            let max_idx = server.phone_tasks.len().saturating_sub(1) as f64;
                            let new_scroll = (nav.start_scroll + delta_cards).clamp(-0.25, max_idx + 0.25);
                            server.shell_state.overview_scroll = new_scroll;
                            server.shell_state.overview_index = new_scroll.round().clamp(0.0, max_idx) as usize;
                            server.overview_scroll = new_scroll;
                        }
                        return;
                    }

                    // Home screen mode:
                    if nav.start_mode == PhoneMode::Home {
                        let is_horizontal = dx.abs() > 24.0 && dx.abs() > dy.abs() * 1.2;
                        if !is_horizontal {
                            server.nav_gesture_active = true;
                            let progress = penumbra_gestures::compute_normalized_progress(dy, PHONE_H as f64);
                            server.nav_gesture_progress = progress;
                            server.nav_gesture_dy = dy;
                            server.nav_gesture_vy = vy;

                            if progress >= 0.25 && vy > -550.0 {
                                tracing::info!(progress, vy, "Bottom nav swipe up from Home -> promote to Overview");
                                server.shell_state.mode = PhoneMode::Overview;
                                server.shell_state.overview_index = 0;
                                server.shell_state.overview_scroll = 0.0;
                                server.overview_scroll = 0.0;
                                server.nav_gesture_active = false;
                                server.nav_gesture_progress = 0.0;
                                server.shell_state.is_dragging = true;
                                server.shell_state.active_card_dy = 0.0;
                                nav.start_mode = PhoneMode::Overview;
                                nav.start_scroll = 0.0;
                                nav.start_pos = pos;
                                let _ = write_atomic(
                                    &runtime_path(phone::SHELL_STATE_FILE),
                                    &server.shell_state.serialize(),
                                );
                            }
                        }
                        return;
                    }

                    // Active App mode:
                    let is_horizontal = dx.abs() > 24.0 && dx.abs() > dy.abs() * 1.2;
                    if is_horizontal {
                        server.nav_switch_active = true;
                        server.nav_switch_dx = dx;
                        server.nav_switch_vx = vx;
                        server.nav_gesture_active = false;
                        server.nav_gesture_progress = 0.0;
                    } else {
                        server.nav_switch_active = false;
                        server.nav_gesture_active = true;
                        let progress = penumbra_gestures::compute_normalized_progress(dy, PHONE_H as f64);
                        server.nav_gesture_progress = progress;
                        server.nav_gesture_dy = dy;
                        server.nav_gesture_vy = vy;

                        // Android dynamic transition: moving up at normal speed into overview zone
                        // (progress >= 0.28 and vy > -550.0) seamlessly promotes gesture directly into live
                        // Overview mode without releasing finger.
                        if progress >= 0.28 && vy > -550.0 {
                            tracing::info!(progress, vy, "Continuous pull-up promoted to live Overview under finger");
                            server.shell_state.mode = PhoneMode::Overview;
                            server.shell_state.overview_index = 0;
                            server.shell_state.overview_scroll = 0.0;
                            server.overview_scroll = 0.0;
                            server.nav_gesture_active = false;
                            server.nav_gesture_progress = 0.0;
                            server.shell_state.is_dragging = true;
                            server.shell_state.active_card_dy = 0.0;
                            nav.start_mode = PhoneMode::Overview;
                            nav.start_scroll = 0.0;
                            nav.start_pos = pos;
                            let _ = write_atomic(
                                &runtime_path(phone::SHELL_STATE_FILE),
                                &server.shell_state.serialize(),
                            );
                        }
                    }
                    return;
                }
            }

            if let Some(ref mut edge) = input.edge_gesture {
                if edge.slot == slot {
                    let dx = pos.x - edge.start_pos.x;
                    let dy = (pos.y - edge.start_pos.y).abs();
                    // Lateral inward threshold: 35px horizontal travel, horizontally dominant
                    if edge.from_left && dx > 35.0 && dx > dy * 1.1 {
                        edge.triggered = true;
                    } else if !edge.from_left && dx < -35.0 && (-dx) > dy * 1.1 {
                        edge.triggered = true;
                    }
                    return;
                }
            }

            let focus = match input.touch.as_ref() {
                Some(contact) if contact.slot == slot => contact.focus.clone(),
                _ => return,
            };
            send_motion(server, input, pos, focus);
        }
        InputEvent::TouchUp { event } => {
            if server.screen_blanked {
                return;
            }
            let slot: i32 = event.slot().into();
            if let Some(nav) = input.nav_tracker.take() {
                if nav.slot == slot {
                    let vx = nav.tracker.velocity_x();
                    let vy = nav.tracker.velocity_y();
                    let (last_x, last_y) = nav.tracker.last_position();
                    let dx = last_x - nav.start_pos.x;
                    let dy = last_y - nav.start_pos.y;

                    if nav.start_mode == PhoneMode::Overview {
                        // 1. If pulling down to return to fullscreen
                        if server.nav_gesture_active && server.nav_gesture_dy > 0.0 {
                            server.nav_gesture_active = false;
                            let t = server.nav_gesture_progress;
                            let to_fullscreen = vy > 350.0 || (vy > -200.0 && t < 0.70);
                            let target = if to_fullscreen {
                                penumbra_gestures::NavigationTarget::Fullscreen
                            } else {
                                penumbra_gestures::NavigationTarget::Overview
                            };
                            let target_pos = if to_fullscreen { 0.0 } else { 1.0 };
                            let mut spring = penumbra_gestures::SpringSolver::overview();
                            spring.reset(t);
                            spring.set_target(target_pos);
                            let denom = (PHONE_H as f64) * 0.45;
                            let v_norm = if denom > 0.0 { (-vy / denom).clamp(-15.0, 15.0) } else { 0.0 };
                            spring.set_velocity(v_norm);
                            server.nav_spring = Some(spring);
                            server.nav_target = Some(target);
                            server.last_nav_spring_tick = Some(std::time::Instant::now());
                            return;
                        }

                        // 2. If dragging carousel or swiping up to dismiss
                        if server.shell_state.is_dragging {
                            server.shell_state.is_dragging = false;
                            let dismissed = if let Some(card_idx) = nav.grabbed_card {
                                if server.shell_state.active_card_dy < -70.0 || vy < -250.0 || dy < -70.0 {
                                    if let Some(task) = server.phone_tasks.get(card_idx) {
                                        let id = task.id;
                                        server.close_task_by_id(id);
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            };

                            if !dismissed {
                                // Horizontal settle with velocity-based momentum fling
                                let max_idx = server.phone_tasks.len().saturating_sub(1) as f64;
                                let fling_cards = vx / 550.0;
                                let raw_target = nav.start_scroll + (dx / CARD_STEP as f64) + fling_cards;
                                let target_idx = raw_target.round().clamp(0.0, max_idx);
                                server.shell_state.overview_index = target_idx as usize;
                                server.shell_state.overview_scroll = target_idx;
                                // advance_overview_scroll() smoothly lerps server.overview_scroll to target_idx
                            }
                            server.shell_state.active_card_dy = 0.0;
                            let _ = write_atomic(
                                &runtime_path(phone::SHELL_STATE_FILE),
                                &server.shell_state.serialize(),
                            );
                            return;
                        }

                        // Bottom nav bar swipe up in Overview mode -> dismiss to Home
                        if nav.grabbed_card.is_none() && nav.start_pos.y >= (NAV_BAR_Y as f64 - 48.0) && (dy < -35.0 || vy < -300.0) {
                            tracing::info!("Bottom nav bar swiped up in Overview -> dismiss to Home");
                            server.set_phone_home();
                            server.nav_gesture_active = false;
                            server.nav_gesture_progress = 0.0;
                            server.shell_state.is_dragging = false;
                            server.shell_state.active_card_dy = 0.0;
                            let _ = write_atomic(
                                &runtime_path(phone::SHELL_STATE_FILE),
                                &server.shell_state.serialize(),
                            );
                            return;
                        }

                        // 3. Tap detection in Overview mode (using a 24px threshold for touchscreen tolerances)
                        if dx.abs() < 24.0 && dy.abs() < 24.0 {
                            let mut tapped_card = None;
                            for (i, task) in server.phone_tasks.iter().enumerate() {
                                let (cx, cy, cw, ch, _) = card_transform(
                                    i,
                                    server.shell_state.overview_scroll,
                                    server.shell_state.overview_index,
                                    0.0,
                                );
                                if last_x >= cx as f64
                                    && last_x <= (cx + cw) as f64
                                    && last_y >= cy as f64
                                    && last_y <= (cy + ch) as f64
                                {
                                    tapped_card = Some((i, task.id));
                                    break;
                                }
                            }

                            if let Some((idx, id)) = tapped_card {
                                tracing::info!(id, idx, "Overview card tapped -> restore fullscreen");
                                server.shell_state.overview_index = idx;
                                server.shell_state.overview_scroll = idx as f64;
                                server.overview_scroll = idx as f64;
                                let mut spring = penumbra_gestures::SpringSolver::overview();
                                spring.reset(1.0);
                                spring.set_target(0.0);
                                spring.set_velocity(-6.0);
                                server.nav_spring = Some(spring);
                                server.nav_target = Some(penumbra_gestures::NavigationTarget::Fullscreen);
                                server.nav_gesture_progress = 1.0;
                                server.nav_gesture_dy = 1.0;
                                server.last_nav_spring_tick = Some(std::time::Instant::now());
                            } else {
                                tracing::info!("Overview background tapped -> return Home");
                                server.set_phone_home();
                            }
                            return;
                        }

                        // Reset overview state
                        server.shell_state.active_card_dy = 0.0;
                        server.shell_state.overview_scroll = server.shell_state.overview_index as f64;
                        server.overview_scroll = server.shell_state.overview_scroll;
                        let _ = write_atomic(
                            &runtime_path(phone::SHELL_STATE_FILE),
                            &server.shell_state.serialize(),
                        );
                        return;
                    }

                    // Home screen mode:
                    if nav.start_mode == PhoneMode::Home {
                        server.nav_gesture_active = false;
                        if dy < -40.0 || vy < -350.0 {
                            tracing::info!("TouchUp from Home -> open Overview");
                            server.shell_state.mode = PhoneMode::Overview;
                            server.shell_state.overview_index = 0;
                            server.shell_state.overview_scroll = 0.0;
                            server.overview_scroll = 0.0;
                            server.nav_gesture_progress = 0.0;
                            let _ = write_atomic(
                                &runtime_path(phone::SHELL_STATE_FILE),
                                &server.shell_state.serialize(),
                            );
                        } else {
                            server.nav_gesture_progress = 0.0;
                        }
                        return;
                    }

                    // Active App mode:
                    // If tap occurred in bottom-right keyboard button area without dragging:
                    if nav.start_pos.y >= 880.0 && nav.start_pos.x >= 355.0 && dx.abs() < 15.0 && dy.abs() < 15.0 {
                        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
                        let cmd_file = std::path::PathBuf::from(runtime_dir).join("penumbra-keyboard-cmd");
                        let _ = std::fs::write(cmd_file, "toggle");
                        tracing::info!("Bottom-right keyboard button tapped -> toggling virtual keyboard");
                        server.nav_gesture_active = false;
                        server.nav_gesture_progress = 0.0;
                        return;
                    }

                    let switch_dx = server.nav_switch_dx;
                    if server.nav_switch_active || (switch_dx.abs() > 20.0 && switch_dx.abs() > server.nav_gesture_dy.abs() * 1.1) {
                        server.nav_switch_active = false;
                        let target = penumbra_gestures::arbitrate_horizontal_release(switch_dx, vx, PHONE_W as f64);
                        let target_dx = match target {
                            penumbra_gestures::NavigationTarget::NextApp => -(PHONE_W as f64),
                            penumbra_gestures::NavigationTarget::PrevApp => PHONE_W as f64,
                            _ => 0.0,
                        };
                        let mut spring = penumbra_gestures::SpringSolver::horizontal_switch();
                        spring.reset(switch_dx);
                        spring.set_target(target_dx);
                        spring.set_velocity(vx.clamp(-3000.0, 3000.0));
                        server.nav_switch_spring = Some(spring);
                        server.nav_switch_target = Some(target);
                        server.last_nav_spring_tick = Some(std::time::Instant::now());
                        return;
                    }

                    server.nav_gesture_active = false;
                    let target = penumbra_gestures::arbitrate_release(
                        server.nav_gesture_progress,
                        vy,
                    );
                    let target_pos = match target {
                        penumbra_gestures::NavigationTarget::Overview | penumbra_gestures::NavigationTarget::Home => 1.0,
                        _ => 0.0,
                    };
                    let mut spring = match target {
                        penumbra_gestures::NavigationTarget::Home => penumbra_gestures::SpringSolver::home_dismiss(),
                        _ => penumbra_gestures::SpringSolver::overview(),
                    };
                    spring.reset(server.nav_gesture_progress);
                    spring.set_target(target_pos);
                    let denom = (PHONE_H as f64) * 0.45;
                    let v_norm = if denom > 0.0 { (-vy / denom).clamp(-15.0, 15.0) } else { 0.0 };
                    spring.set_velocity(v_norm);
                    server.nav_spring = Some(spring);
                    server.nav_target = Some(target);
                    server.last_nav_spring_tick = Some(std::time::Instant::now());
                    return;
                }
            }

            if let Some(edge) = input.edge_gesture.take() {
                if edge.slot == slot {
                    if edge.triggered {
                        tracing::info!(
                            from_left = edge.from_left,
                            "Android lateral back gesture recognized -> trigger Back"
                        );
                        server.trigger_back();
                    }
                    return;
                }
            }
            end_touch(server, input, slot);
        }
        InputEvent::TouchCancel { event } => {
            if server.screen_blanked {
                return;
            }
            let slot: i32 = event.slot().into();
            if let Some(nav) = input.nav_tracker.take() {
                if nav.slot == slot {
                    server.nav_gesture_active = false;
                    server.nav_switch_active = false;
                    server.nav_gesture_progress = 0.0;
                    server.nav_switch_dx = 0.0;
                    server.nav_spring = None;
                    server.nav_switch_spring = None;
                    server.shell_state.is_dragging = false;
                    server.shell_state.active_card_dy = 0.0;
                    return;
                }
            }
            input.edge_gesture = None;
            end_touch(server, input, slot);
        }
        InputEvent::Keyboard { event } => {
            let key = event.key_code();
            let state = event.state();
            handle_keyboard(server, input, key, state);
        }
        _ => {}
    }
}
