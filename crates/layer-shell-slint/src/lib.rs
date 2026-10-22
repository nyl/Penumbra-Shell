//! Standalone `layer-shell-slint` Platform Backend
//!
//! A zero-winit Wayland layer shell provider for the Slint UI toolkit using
//! `smithay-client-toolkit` (SCTK) and `wayland-protocols-wlr`.
//!
//! # Overview
//! This crate enables any Slint component to be instantiated as a native Wayland layer surface
//! (`zwlr_layer_shell_v1`) without pulling in or depending on Winit or X11 libraries:
//! - **Configurable Layer Targeting**: Place surfaces on `Background`, `Bottom`, `Top`, or `Overlay`.
//! - **Edge Anchoring**: Anchor surfaces to screen edges (Top, Bottom, Left, Right).
//! - **Exclusive Zone Calculation**: Push or reserve desktop workspace space deterministically.
//! - **Zero-Copy Software Rendering**: Leverages Slint's [`SoftwareRenderer`](slint::platform::software_renderer::SoftwareRenderer)
//!   rendering directly into Wayland shared memory (`wl_shm::SlotPool`) using native `ARGB8888`.
//! - **Wayland-to-Slint Dispatch Bridge**: Translates pointer movements, clicks, scrolls, and touch
//!   gestures into Slint [`WindowEvent`](slint::platform::WindowEvent)s.
//!
//! # Disclaimer
//! This is an independent community-driven platform backend and is **not** an official
//! implementation by or affiliated with SixtyFPS GmbH or the official Slint development team.

pub mod error;
pub mod input;
pub mod platform;
pub mod shm;
pub mod state;
pub mod window;

pub use error::LayerShellError;
pub use platform::LayerShellPlatform;
pub use shm::Argb8888Pixel;
pub use window::LayerWindow;

pub use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
pub use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Anchor, KeyboardInteractivity,
};

/// Returns a configured logical mobile shell size and the output scale derived
/// from the real panel dimensions when available.
pub fn mobile_output_geometry() -> (u32, u32, f32) {
    let output_w = std::env::var("PENUMBRA_OUTPUT_WIDTH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1080);
    let output_h = std::env::var("PENUMBRA_OUTPUT_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(2340);

    // The logical canvas is the 412x915 mobile design size. The compositor scales
    // it uniformly onto the physical panel (letterboxed), so shells never lay out
    // at panel pixels.
    let logical_w = std::env::var("PENUMBRA_LOGICAL_WIDTH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(412);
    let logical_h = std::env::var("PENUMBRA_LOGICAL_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(915);

    let scale = std::env::var("PENUMBRA_SCALE_FACTOR")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or_else(|| {
            let scale_w = output_w as f32 / logical_w.max(1) as f32;
            let scale_h = output_h as f32 / logical_h.max(1) as f32;
            scale_w.min(scale_h)
        });

    (logical_w, logical_h, scale)
}

#[cfg(test)]
mod tests {
    use super::mobile_output_geometry;

    #[test]
    fn native_output_geometry_defaults_to_412x915_logical_canvas() {
        std::env::set_var("PENUMBRA_OUTPUT_WIDTH", "1080");
        std::env::set_var("PENUMBRA_OUTPUT_HEIGHT", "2340");
        std::env::remove_var("PENUMBRA_LOGICAL_WIDTH");
        std::env::remove_var("PENUMBRA_LOGICAL_HEIGHT");
        std::env::remove_var("PENUMBRA_SCALE_FACTOR");

        let (w, h, scale) = mobile_output_geometry();
        assert_eq!(w, 412);
        assert_eq!(h, 915);
        assert!((scale - 2.557).abs() < 0.01, "scale={scale}");
    }
}

/// Declarative configuration for creating a Wayland layer shell surface.
#[derive(Debug, Clone)]
pub struct LayerConfig {
    /// Namespace identifier for compositor rules (e.g., "panel", "dock", "osk").
    pub namespace: String,
    /// Target compositor layer where this surface will be rendered.
    pub layer: Layer,
    /// Edges to which the surface should anchor.
    pub anchor: Anchor,
    /// Requested initial width in surface coordinates (0 implies stretch between left/right anchors).
    pub width: u32,
    /// Requested initial height in surface coordinates (0 implies stretch between top/bottom anchors).
    pub height: u32,
    /// Surface margins: (Top, Right, Bottom, Left).
    pub margin: (i32, i32, i32, i32),
    /// Exclusive zone size in pixels (pushes regular windows; -1 disables).
    pub exclusive_zone: i32,
    /// Level of keyboard focus grabbed by this layer surface.
    pub keyboard_interactivity: KeyboardInteractivity,
}

impl Default for LayerConfig {
    fn default() -> Self {
        Self {
            namespace: "layer-shell-slint".to_string(),
            layer: Layer::Top,
            anchor: Anchor::Top | Anchor::Left | Anchor::Right,
            width: 0,
            height: 48,
            margin: (0, 0, 0, 0),
            exclusive_zone: 48,
            keyboard_interactivity: KeyboardInteractivity::None,
        }
    }
}

impl LayerConfig {
    /// Preset configuration for a persistent top status bar or header panel.
    ///
    /// Anchors to Top | Left | Right with exclusive zone equal to the panel height.
    pub fn top_bar(height: u32) -> Self {
        Self {
            namespace: "slint-top-bar".to_string(),
            layer: Layer::Top,
            anchor: Anchor::Top | Anchor::Left | Anchor::Right,
            width: 0,
            height,
            margin: (0, 0, 0, 0),
            exclusive_zone: height as i32,
            keyboard_interactivity: KeyboardInteractivity::None,
        }
    }

    /// Preset configuration for a persistent bottom dock or taskbar.
    ///
    /// Anchors to Bottom | Left | Right with exclusive zone equal to the panel height.
    pub fn bottom_panel(height: u32) -> Self {
        Self {
            namespace: "slint-bottom-panel".to_string(),
            layer: Layer::Top,
            anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
            width: 0,
            height,
            margin: (0, 0, 0, 0),
            exclusive_zone: height as i32,
            keyboard_interactivity: KeyboardInteractivity::None,
        }
    }

    /// Preset configuration for a full-screen modal, launcher, or lock overlay.
    ///
    /// Anchors to all four edges on the `Overlay` layer with exclusive keyboard interaction.
    pub fn overlay() -> Self {
        Self {
            namespace: "slint-overlay".to_string(),
            layer: Layer::Overlay,
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            width: 0,
            height: 0,
            margin: (0, 0, 0, 0),
            exclusive_zone: -1,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
        }
    }

    /// Sets the namespace string for the layer surface.
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Sets the target layer (`Background`, `Bottom`, `Top`, or `Overlay`).
    pub fn with_layer(mut self, layer: Layer) -> Self {
        self.layer = layer;
        self
    }

    /// Sets the edge anchor flags.
    pub fn with_anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Sets the explicit width and height dimensions.
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Sets the margins around the surface: (top, right, bottom, left).
    pub fn with_margin(mut self, top: i32, right: i32, bottom: i32, left: i32) -> Self {
        self.margin = (top, right, bottom, left);
        self
    }

    /// Sets the exclusive zone in pixels (insets for application workspace).
    pub fn with_exclusive_zone(mut self, exclusive_zone: i32) -> Self {
        self.exclusive_zone = exclusive_zone;
        self
    }

    /// Sets the keyboard interactivity mode (`None`, `Exclusive`, or `OnDemand`).
    pub fn with_keyboard_interactivity(mut self, interactivity: KeyboardInteractivity) -> Self {
        self.keyboard_interactivity = interactivity;
        self
    }
}

/// Initializes the Slint layer-shell platform backend with the specified configuration.
///
/// Registers the platform driver globally via [`slint::platform::set_platform`].
/// Must be called before instantiating any Slint UI components.
///
/// # Errors
/// Returns [`LayerShellError`] if connecting to the Wayland display or binding
/// the required protocols fails.
pub fn init(config: LayerConfig) -> Result<(), LayerShellError> {
    let platform = LayerShellPlatform::new(config)?;
    slint::platform::set_platform(Box::new(platform))?;
    Ok(())
}
