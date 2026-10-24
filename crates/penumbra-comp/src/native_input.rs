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
    phone::{NAV_BAR_Y, PHONE_H, PHONE_W},
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
    pub nav_tracker: Option<(i32, Point<f64, Logical>, penumbra_gestures::VelocityTracker)>,
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

            // Bottom navigation gesture: immediate 1:1 tracking on first pixel from nav region
            if pos.y >= NAV_BAR_Y as f64 && server.phone_app_visible() && input.nav_tracker.is_none() {
                let mut tracker = penumbra_gestures::VelocityTracker::new(0.8);
                tracker.start(pos.x, pos.y, std::time::Duration::ZERO);
                input.nav_tracker = Some((slot, pos, tracker));
                server.nav_gesture_active = true;
                server.nav_gesture_progress = 0.0;
                server.nav_gesture_dy = 0.0;
                server.nav_gesture_vy = 0.0;
                server.nav_spring = None;
                return;
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
            send_motion(server, input, pos, focus);
            send_button(server, input, BTN_LEFT, ButtonState::Pressed);
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

            if let Some((tracked_slot, start_pos, ref mut tracker)) = input.nav_tracker {
                if tracked_slot == slot {
                    let elapsed = input.started.elapsed();
                    let (vx, vy) = tracker.update(pos.x, pos.y, elapsed);
                    let dx = pos.x - start_pos.x;
                    let dy = pos.y - start_pos.y;

                    let is_horizontal = dx.abs() > 15.0 && dx.abs() > dy.abs() * 1.1;
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
            if let Some((tracked_slot, _start_pos, tracker)) = input.nav_tracker.take() {
                if tracked_slot == slot {
                    let vx = tracker.velocity_x();
                    let vy = tracker.velocity_y();
                    let dx = server.nav_switch_dx;

                    if server.nav_switch_active || (dx.abs() > 20.0 && dx.abs() > server.nav_gesture_dy.abs() * 1.1) {
                        server.nav_switch_active = false;
                        let target = penumbra_gestures::arbitrate_horizontal_release(dx, vx, PHONE_W as f64);
                        let target_dx = match target {
                            penumbra_gestures::NavigationTarget::NextApp => -(PHONE_W as f64),
                            penumbra_gestures::NavigationTarget::PrevApp => PHONE_W as f64,
                            _ => 0.0,
                        };
                        let mut spring = penumbra_gestures::SpringSolver::horizontal_switch();
                        spring.reset(dx);
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
            if let Some((tracked_slot, _, _)) = input.nav_tracker.take() {
                if tracked_slot == slot {
                    server.nav_gesture_active = false;
                    server.nav_switch_active = false;
                    server.nav_gesture_progress = 0.0;
                    server.nav_switch_dx = 0.0;
                    server.nav_spring = None;
                    server.nav_switch_spring = None;
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
