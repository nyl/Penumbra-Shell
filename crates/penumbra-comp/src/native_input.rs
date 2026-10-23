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

/// Linux hardware key codes.
const KEY_POWER: u32 = 116;
const KEY_VOLUMEUP: u32 = 115;
const KEY_VOLUMEDOWN: u32 = 114;

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
    pub power_press_time: Option<std::time::Instant>,
    pub power_dialog_triggered: bool,
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
            power_press_time: None,
            power_dialog_triggered: false,
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
    match key {
        KEY_POWER => match state {
            KeyState::Pressed => {
                input.power_press_time = Some(std::time::Instant::now());
                input.power_dialog_triggered = false;
            }
            KeyState::Released => {
                if let Some(start) = input.power_press_time.take() {
                    let elapsed = start.elapsed();
                    if input.power_dialog_triggered {
                        // Already handled by hold detector
                    } else if elapsed >= std::time::Duration::from_millis(550) {
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
        KEY_VOLUMEUP => {
            if state == KeyState::Pressed {
                tracing::info!("Hardware Volume Up button pressed (+5%)");
                let _ = std::process::Command::new("pactl")
                    .args(["set-sink-volume", "@DEFAULT_SINK@", "+5%"])
                    .spawn();
            }
        }
        KEY_VOLUMEDOWN => {
            if state == KeyState::Pressed {
                tracing::info!("Hardware Volume Down button pressed (-5%)");
                let _ = std::process::Command::new("pactl")
                    .args(["set-sink-volume", "@DEFAULT_SINK@", "-5%"])
                    .spawn();
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
            let delta = event.delta();
            let pos = clamp_to_canvas(
                server.pointer_location.x + delta.x,
                server.pointer_location.y + delta.y,
            );
            hover(server, input, pos);
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let (width, height) = input.panel;
            let pos = input.panel_point(
                event.x_transformed(width),
                event.y_transformed(height),
            );
            hover(server, input, pos);
        }
        InputEvent::PointerButton { event } => {
            send_button(server, input, event.button_code(), event.state());
        }
        InputEvent::PointerAxis { event } => {
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
            let slot: i32 = event.slot().into();
            let (width, height) = input.panel;
            let pos = input.panel_point(
                event.x_transformed(width),
                event.y_transformed(height),
            );

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
            let slot: i32 = event.slot().into();
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
            let slot: i32 = event.slot().into();
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
