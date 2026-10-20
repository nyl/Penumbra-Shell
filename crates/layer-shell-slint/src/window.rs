//! Implements [`slint::platform::WindowAdapter`] for native Wayland layer-shell surfaces.

use std::cell::Cell;
use std::rc::{Rc, Weak};
use log::{debug, error, trace};
use slint::platform::software_renderer::SoftwareRenderer;
use slint::platform::{Renderer, WindowAdapter, WindowEvent};
use slint::{PhysicalSize, Window};
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use wayland_client::protocol::wl_shm::Format;
use wayland_client::QueueHandle;

use crate::shm::{calculate_stride, Argb8888Pixel};
use crate::state::AppState;

/// The primary window adapter linking a Slint [`Window`] to a Wayland [`LayerSurface`].
pub struct LayerWindow {
    /// Slint window handle owned by this adapter.
    window: Window,
    /// Software renderer generating ARGB8888 pixels.
    renderer: SoftwareRenderer,
    /// SCTK layer surface representing the native Wayland layer shell entity.
    layer_surface: LayerSurface,
    /// Current window dimensions in physical pixels.
    size: Cell<PhysicalSize>,
    /// Display output scaling factor (DPI multiplier).
    scale_factor: Cell<f32>,
    /// Flag indicating whether the window has received its initial configure sequence.
    configured: Cell<bool>,
    /// Flag set when Slint requests a new frame to be drawn.
    needs_redraw: Cell<bool>,
    /// Flag indicating a Wayland frame callback (`wl_surface.frame()`) is in flight.
    frame_callback_pending: Cell<bool>,
    /// Wayland queue handle for scheduling frame callbacks.
    qh: QueueHandle<AppState>,
    /// Ping signal to wake up the Calloop event loop when redraws are requested asynchronously.
    ping: calloop::ping::Ping,
}

impl LayerWindow {
    /// Creates a new `LayerWindow` bound to a Wayland `LayerSurface`.
    pub fn new(
        layer_surface: LayerSurface,
        initial_width: u32,
        initial_height: u32,
        qh: QueueHandle<AppState>,
        ping: calloop::ping::Ping,
    ) -> Rc<Self> {
        let renderer = SoftwareRenderer::new();
        let initial_size = PhysicalSize::new(initial_width, initial_height);

        Rc::new_cyclic(|weak_adapter: &Weak<Self>| {
            // Slint window requires a Weak<dyn WindowAdapter>
            let weak_dyn: Weak<dyn WindowAdapter> = weak_adapter.clone();
            let window = Window::new(weak_dyn);

            Self {
                window,
                renderer,
                layer_surface,
                size: Cell::new(initial_size),
                scale_factor: Cell::new(1.0),
                configured: Cell::new(false),
                needs_redraw: Cell::new(true),
                frame_callback_pending: Cell::new(false),
                qh,
                ping,
            }
        })
    }

    /// Returns the underlying SCTK [`LayerSurface`].
    pub fn layer_surface(&self) -> &LayerSurface {
        &self.layer_surface
    }

    /// Returns the current scale factor.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor.get()
    }

    /// Updates the display scale factor and notifies Slint of the change.
    pub fn set_scale_factor(&self, factor: f32) {
        if (self.scale_factor.get() - factor).abs() > f32::EPSILON {
            self.scale_factor.set(factor);
            self.dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: factor,
            });
            self.request_redraw();
        }
    }

    /// Handles a Wayland configure event from the compositor.
    pub fn on_configure(&self, width: u32, height: u32) {
        let (current_w, current_h) = (self.size.get().width, self.size.get().height);
        let new_w = if width > 0 { width } else { current_w };
        let new_h = if height > 0 { height } else { current_h };

        let new_size = PhysicalSize::new(new_w, new_h);
        if self.size.get() != new_size || !self.configured.get() {
            debug!(
                "Configuring LayerWindow surface geometry: {}x{}",
                new_w, new_h
            );
            self.size.set(new_size);
            self.configured.set(true);
            self.dispatch_event(WindowEvent::Resized {
                size: new_size.to_logical(self.scale_factor.get()),
            });
        }

        self.needs_redraw.set(true);
        self.ping.ping();
    }

    /// Invoked when the compositor completes a frame callback.
    pub fn on_frame_rendered(&self) {
        self.frame_callback_pending.set(false);
        if self.needs_redraw.get() {
            self.ping.ping();
        }
    }

    /// Checks if a frame can currently be submitted to the compositor.
    pub fn can_render(&self) -> bool {
        self.configured.get() && self.needs_redraw.get() && !self.frame_callback_pending.get()
    }

    /// Executes the Slint software render pass and commits the resulting buffer to Wayland.
    pub fn render(&self, pool: &mut SlotPool) {
        let width = self.size.get().width;
        let height = self.size.get().height;

        if width == 0 || height == 0 || !self.configured.get() {
            trace!(
                "Skipping render pass: invalid size ({}x{}) or not configured",
                width,
                height
            );
            return;
        }

        // Throttle to compositor refresh rate if a frame callback is already pending
        if self.frame_callback_pending.get() {
            trace!("Skipping render pass: waiting for active frame callback");
            return;
        }

        self.needs_redraw.set(false);
        let stride = calculate_stride(width);

        // Allocate or reuse a buffer slot from the shared memory pool
        let (buffer, canvas) = match pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            Format::Argb8888,
        ) {
            Ok(res) => res,
            Err(e) => {
                error!("Failed to allocate SHM buffer from SlotPool: {:?}", e);
                return;
            }
        };

        // Safety: Argb8888Pixel is #[repr(C)] with 4 u8 bytes, identical to 32-bit ARGB8888.
        let pixel_count = (width as usize) * (height as usize);
        let pixels: &mut [Argb8888Pixel] = unsafe {
            std::slice::from_raw_parts_mut(
                canvas.as_mut_ptr() as *mut Argb8888Pixel,
                pixel_count.min(canvas.len() / 4),
            )
        };

        // Render Slint UI elements into the Wayland shared memory buffer
        self.renderer.render(pixels, width as usize);

        let wl_surface = self.layer_surface.wl_surface();

        // Attach the rendered SHM buffer
        if let Err(e) = buffer.attach_to(wl_surface) {
            error!("Failed to attach SHM buffer to wl_surface: {:?}", e);
            return;
        }

        // Damage the entire surface region to notify the compositor of updated pixels
        wl_surface.damage_buffer(0, 0, width as i32, height as i32);

        // Register a frame callback to sync with the display's VBlank / refresh rate
        wl_surface.frame(&self.qh, wl_surface.clone());
        self.frame_callback_pending.set(true);

        // Commit pending state changes atomically
        wl_surface.commit();
        trace!("Committed Wayland surface frame ({}x{})", width, height);
    }

    /// Dispatches a window event to the Slint internal component tree.
    pub fn dispatch_event(&self, event: WindowEvent) {
        self.window.dispatch_event(event);
    }
}

// ---------------------------------------------------------------------------
// WindowAdapter Trait Implementation
// ---------------------------------------------------------------------------
impl WindowAdapter for LayerWindow {
    fn window(&self) -> &Window {
        &self.window
    }

    fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    fn renderer(&self) -> &dyn Renderer {
        &self.renderer
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
        self.ping.ping();
    }

    fn set_visible(&self, _visible: bool) -> Result<(), slint::PlatformError> {
        // Layer surfaces are mapped automatically once configured and committed
        Ok(())
    }
}
