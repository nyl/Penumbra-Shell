//! SCTK AppState handling seat, shm, layer-shell, and compositor delegates.

use std::rc::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use log::{debug, info, trace};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_subcompositor, delegate_touch,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerHandler},
        touch::TouchHandler,
        Capability, SeatHandler, SeatState,
    },
    shell::wlr_layer::{LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::protocol::{
    wl_output::{self, WlOutput},
    wl_pointer::WlPointer,
    wl_seat::WlSeat,
    wl_surface::WlSurface,
    wl_touch::WlTouch,
};
use wayland_client::{Connection, QueueHandle};

use crate::input::InputProcessor;
use crate::window::LayerWindow;

/// Application state carrying SCTK protocol delegates and managing the Wayland connection.
pub struct AppState {
    /// Wayland global registry state tracker.
    pub registry_state: RegistryState,
    /// Wayland compositor manager state.
    pub compositor_state: CompositorState,
    /// Connected outputs state tracker.
    pub output_state: OutputState,
    /// Seat and capability tracking state.
    pub seat_state: SeatState,
    /// Shared memory manager state.
    pub shm: Shm,
    /// Shared memory buffer allocation pool.
    pub pool: SlotPool,
    /// wlr-layer-shell protocol manager.
    pub layer_shell: LayerShell,

    /// Currently bound mouse pointer device, if available on seat.
    pub pointer: Option<WlPointer>,
    /// Currently bound touchscreen device, if available on seat.
    pub touch: Option<WlTouch>,
    /// Input event normalizer translating Wayland events to Slint window events.
    pub input_processor: InputProcessor,

    /// Weak reference to the active Slint layer window adapter.
    pub window: Option<Weak<LayerWindow>>,

    /// Flag set to true when the window surface is closed or the event loop should terminate.
    pub exit_flag: Arc<AtomicBool>,
}

impl AppState {
    /// Dispatches an event closure across the active layer window, if alive.
    fn with_window<F: FnOnce(&LayerWindow)>(&self, f: F) {
        if let Some(ref weak) = self.window {
            if let Some(window) = weak.upgrade() {
                f(&window);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Compositor Handler
// ---------------------------------------------------------------------------
impl CompositorHandler for AppState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        new_factor: i32,
    ) {
        debug!("Wayland surface scale factor changed: {}", new_factor);
        self.with_window(|w| {
            w.set_scale_factor(new_factor as f32);
        });
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        // Transformations are handled transparently by the compositor.
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _time: u32,
    ) {
        trace!("Wayland frame callback received; ready for next frame presentation");
        self.with_window(|w| {
            w.on_frame_rendered();
        });
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }
}

// ---------------------------------------------------------------------------
// Output Handler
// ---------------------------------------------------------------------------
impl OutputHandler for AppState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}
    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {}
    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: WlOutput) {
    }
}

// ---------------------------------------------------------------------------
// Shared Memory Handler
// ---------------------------------------------------------------------------
impl ShmHandler for AppState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

// ---------------------------------------------------------------------------
// Seat Handler
// ---------------------------------------------------------------------------
impl SeatHandler for AppState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer if self.pointer.is_none() => {
                debug!("Acquiring wl_pointer capability on seat");
                let pointer = self
                    .seat_state
                    .get_pointer(qh, &seat)
                    .expect("Failed to get pointer");
                self.pointer = Some(pointer);
            }
            Capability::Touch if self.touch.is_none() => {
                debug!("Acquiring wl_touch capability on seat");
                let touch = self
                    .seat_state
                    .get_touch(qh, &seat)
                    .expect("Failed to get touch");
                self.touch = Some(touch);
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer => {
                debug!("Releasing wl_pointer capability");
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            }
            Capability::Touch => {
                debug!("Releasing wl_touch capability");
                if let Some(touch) = self.touch.take() {
                    touch.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: WlSeat) {}
}

// ---------------------------------------------------------------------------
// Pointer Handler
// ---------------------------------------------------------------------------
impl PointerHandler for AppState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let window = self.window.as_ref().and_then(|w| w.upgrade());
        if let Some(w) = window {
            let scale = w.scale_factor();
            for event in events {
                let slint_events = self.input_processor.process_pointer_event(event, scale);
                for slint_event in slint_events {
                    w.dispatch_event(slint_event);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Touch Handler
// ---------------------------------------------------------------------------
impl TouchHandler for AppState {
    fn down(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        _surface: WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        let window = self.window.as_ref().and_then(|w| w.upgrade());
        if let Some(w) = window {
            let scale = w.scale_factor();
            let events = self.input_processor.touch_down(id, position, scale);
            for event in events {
                w.dispatch_event(event);
            }
        }
    }

    fn up(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        id: i32,
    ) {
        let window = self.window.as_ref().and_then(|w| w.upgrade());
        if let Some(w) = window {
            let scale = w.scale_factor();
            let events = self.input_processor.touch_up(id, scale);
            for event in events {
                w.dispatch_event(event);
            }
        }
    }

    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &WlTouch,
        _time: u32,
        id: i32,
        position: (f64, f64),
    ) {
        let window = self.window.as_ref().and_then(|w| w.upgrade());
        if let Some(w) = window {
            let scale = w.scale_factor();
            let events = self.input_processor.touch_motion(id, position, scale);
            for event in events {
                w.dispatch_event(event);
            }
        }
    }

    fn cancel(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _touch: &WlTouch) {
        let window = self.window.as_ref().and_then(|w| w.upgrade());
        if let Some(w) = window {
            let scale = w.scale_factor();
            let events = self.input_processor.touch_cancel(scale);
            for event in events {
                w.dispatch_event(event);
            }
        }
    }

    fn shape(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _major: f64,
        _minor: f64,
    ) {
    }

    fn orientation(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _orientation: f64,
    ) {
    }
}

// ---------------------------------------------------------------------------
// Layer Shell Handler
// ---------------------------------------------------------------------------
impl LayerShellHandler for AppState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        info!("Wayland layer surface closed by compositor; terminating event loop");
        self.exit_flag.store(true, Ordering::SeqCst);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        serial: u32,
    ) {
        debug!(
            "Wayland layer surface configure event: size={:?}, serial={}",
            configure.new_size, serial
        );

        let window = self.window.as_ref().and_then(|w| w.upgrade());
        if let Some(w) = window {
            let (w_width, w_height) = configure.new_size;
            w.on_configure(w_width, w_height);
        }
    }
}

// Registry handling
impl ProvidesRegistryState for AppState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(AppState);
delegate_compositor!(AppState);
delegate_subcompositor!(AppState);
delegate_output!(AppState);
delegate_shm!(AppState);
delegate_seat!(AppState);
delegate_pointer!(AppState);
delegate_touch!(AppState);
delegate_layer!(AppState);
