//! Input handling and event translation from Wayland pointer/touch to Slint WindowEvents.
//!
//! Translates Wayland pointer and touch protocols into Slint's unified [`WindowEvent`] system.
//! Single-touch interactions are normalized to mouse clicks/drags so standard Slint widgets
//! (such as `TouchArea` and `Button`) respond identically to both mouse and finger taps.

use slint::platform::{PointerEventButton, WindowEvent};
use slint::LogicalPosition;
use smithay_client_toolkit::seat::pointer::{AxisScroll, PointerEvent, PointerEventKind};

/// Linux input event codes for standard mouse buttons.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// Maps a Linux event code (from wl_pointer.button) to Slint's [`PointerEventButton`].
pub fn linux_button_to_slint(button: u32) -> PointerEventButton {
    match button {
        BTN_LEFT => PointerEventButton::Left,
        BTN_RIGHT => PointerEventButton::Right,
        BTN_MIDDLE => PointerEventButton::Middle,
        _ => PointerEventButton::Other,
    }
}

/// State tracker for normalizing Wayland input devices to Slint window events.
#[derive(Debug, Default)]
pub struct InputProcessor {
    /// Last known pointer position in surface-local coordinates.
    current_position: (f64, f64),
    /// ID of the currently active touch contact (for single-touch emulation).
    active_touch_id: Option<i32>,
    /// Last position of the active touch contact.
    last_touch_position: (f64, f64),
}

impl InputProcessor {
    /// Creates a new `InputProcessor`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Processes a batch of Wayland pointer events and produces Slint [`WindowEvent`]s.
    pub fn process_pointer_event(
        &mut self,
        event: &PointerEvent,
        scale_factor: f32,
    ) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        self.current_position = event.position;
        let pos = LogicalPosition::new(
            (event.position.0 as f32) / scale_factor,
            (event.position.1 as f32) / scale_factor,
        );

        match &event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                out.push(WindowEvent::PointerMoved { position: pos });
            }
            PointerEventKind::Leave { .. } => {
                out.push(WindowEvent::PointerExited);
            }
            PointerEventKind::Press { button, .. } => {
                let slint_btn = linux_button_to_slint(*button);
                out.push(WindowEvent::PointerPressed {
                    position: pos,
                    button: slint_btn,
                });
            }
            PointerEventKind::Release { button, .. } => {
                let slint_btn = linux_button_to_slint(*button);
                out.push(WindowEvent::PointerReleased {
                    position: pos,
                    button: slint_btn,
                });
            }
            PointerEventKind::Axis {
                horizontal,
                vertical,
                ..
            } => {
                let delta_x = extract_axis_scroll(horizontal) / scale_factor;
                let delta_y = extract_axis_scroll(vertical) / scale_factor;
                if delta_x != 0.0 || delta_y != 0.0 {
                    out.push(WindowEvent::PointerScrolled {
                        position: pos,
                        delta_x: -delta_x,
                        delta_y: -delta_y,
                    });
                }
            }
        }

        out
    }

    /// Handles a touch down event. If no touch contact is active, claims this ID and emits
    /// a move followed by a press event.
    pub fn touch_down(&mut self, id: i32, pos: (f64, f64), scale_factor: f32) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        if self.active_touch_id.is_none() {
            self.active_touch_id = Some(id);
            self.last_touch_position = pos;
            let logical_pos =
                LogicalPosition::new((pos.0 as f32) / scale_factor, (pos.1 as f32) / scale_factor);
            out.push(WindowEvent::PointerMoved {
                position: logical_pos,
            });
            out.push(WindowEvent::PointerPressed {
                position: logical_pos,
                button: PointerEventButton::Left,
            });
        }
        out
    }

    /// Handles a touch motion event for the active touch ID.
    pub fn touch_motion(
        &mut self,
        id: i32,
        pos: (f64, f64),
        scale_factor: f32,
    ) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        if self.active_touch_id == Some(id) {
            self.last_touch_position = pos;
            let logical_pos =
                LogicalPosition::new((pos.0 as f32) / scale_factor, (pos.1 as f32) / scale_factor);
            out.push(WindowEvent::PointerMoved {
                position: logical_pos,
            });
        }
        out
    }

    /// Handles a touch up event, emitting a release and exit event when the active touch lifts.
    pub fn touch_up(&mut self, id: i32, scale_factor: f32) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        if self.active_touch_id == Some(id) {
            let logical_pos = LogicalPosition::new(
                (self.last_touch_position.0 as f32) / scale_factor,
                (self.last_touch_position.1 as f32) / scale_factor,
            );
            out.push(WindowEvent::PointerReleased {
                position: logical_pos,
                button: PointerEventButton::Left,
            });
            out.push(WindowEvent::PointerExited);
            self.active_touch_id = None;
        }
        out
    }

    /// Handles a touch cancel event, aborting any active touch interaction.
    pub fn touch_cancel(&mut self, scale_factor: f32) -> Vec<WindowEvent> {
        let mut out = Vec::new();
        if self.active_touch_id.is_some() {
            let logical_pos = LogicalPosition::new(
                (self.last_touch_position.0 as f32) / scale_factor,
                (self.last_touch_position.1 as f32) / scale_factor,
            );
            out.push(WindowEvent::PointerReleased {
                position: logical_pos,
                button: PointerEventButton::Left,
            });
            out.push(WindowEvent::PointerExited);
            self.active_touch_id = None;
        }
        out
    }
}

/// Helper to extract numeric pixel scroll delta from an SCTK `AxisScroll`.
fn extract_axis_scroll(scroll: &AxisScroll) -> f32 {
    let mut delta = 0.0;
    if scroll.absolute != 0.0 {
        delta += scroll.absolute as f32;
    } else if scroll.discrete != 0 {
        // Fallback for discrete wheel notches (typical mouse notch is ~15-20 pixels)
        delta += (scroll.discrete as f32) * 20.0;
    }
    delta
}
