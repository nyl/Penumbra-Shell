//! Input routing for the native DRM backend.
//!
//! libinput reports absolute touch and pointer positions in physical panel
//! pixels. They are mapped onto the logical phone canvas and delivered through
//! the seat's pointer. Touch is emulated with a single pointer: the first finger
//! drives motion and `BTN_LEFT`, which is what layer-shell and xdg clients expect.

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, ButtonState, InputEvent, PointerAxisEvent,
            PointerButtonEvent, PointerMotionEvent, TouchEvent,
        },
        libinput::LibinputInputBackend,
    },
    input::pointer::{AxisFrame, ButtonEvent, MotionEvent, PointerHandle},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Serial},
};

use crate::{
    native::Canvas,
    phone::{PHONE_H, PHONE_W},
    server::PenumbraServerState,
};

/// Linux `BTN_LEFT` key code.
const BTN_LEFT: u32 = 0x110;

/// Focus target and origin for a pointer position.
type Focus = Option<(WlSurface, Point<f64, Logical>)>;

/// The finger currently driving the emulated pointer.
struct TouchContact {
    slot: i32,
    /// Surface that received the touch-down; motion stays with it until release.
    focus: Focus,
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
        }
    }

    fn next_serial(&mut self) -> Serial {
        self.serial = self.serial.wrapping_add(1);
        self.serial.into()
    }

    fn time(&self) -> u32 {
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
            // Only the first finger drives the emulated pointer.
            if input.touch.is_some() {
                return;
            }
            let slot: i32 = event.slot().into();
            let (width, height) = input.panel;
            let pos = input.panel_point(
                event.x_transformed(width),
                event.y_transformed(height),
            );
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
            let focus = match input.touch.as_ref() {
                Some(contact) if contact.slot == slot => contact.focus.clone(),
                _ => return,
            };
            let (width, height) = input.panel;
            let pos = input.panel_point(
                event.x_transformed(width),
                event.y_transformed(height),
            );
            send_motion(server, input, pos, focus);
        }
        InputEvent::TouchUp { event } => {
            let slot: i32 = event.slot().into();
            end_touch(server, input, slot);
        }
        InputEvent::TouchCancel { event } => {
            let slot: i32 = event.slot().into();
            end_touch(server, input, slot);
        }
        // Keyboard, gestures, and touch frame markers are not routed on the native path.
        _ => {}
    }
}
