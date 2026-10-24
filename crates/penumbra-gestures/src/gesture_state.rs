//! Gesture state machine, normalized transform computations, and commit arbitration.
//!
//! Models Android 17 (Pixel) navigation dynamics, translating interactive finger
//! displacements into normalized progress, scale, corner radius, and spring targets.

/// Target destination when a navigation gesture is completed or released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NavigationTarget {
    /// Return active application to fullscreen ($scale = 1.0$, offset $0$).
    #[default]
    Fullscreen,
    /// Transition into the overview/recents task carousel ($scale = 0.68$).
    Overview,
    /// Fast flick dismiss to launcher homescreen.
    Home,
    /// Fast horizontal swipe to next running application.
    NextApp,
    /// Fast horizontal swipe to previous running application.
    PrevApp,
}

/// Dynamic phase of an interactive navigation gesture.
#[derive(Debug, Clone, PartialEq)]
pub enum GesturePhase {
    /// No gesture is currently active; system is at rest.
    Idle,
    /// User finger is touching the screen, driving window transforms 1:1.
    Dragging {
        start_x: f64,
        start_y: f64,
        current_x: f64,
        current_y: f64,
        delta_x: f64,
        delta_y: f64,
        progress: f64,
        velocity_x: f64,
        velocity_y: f64,
    },
    /// Finger released; spring physics solver is animating window to equilibrium.
    Settling {
        current_progress: f64,
        target_progress: f64,
        target: NavigationTarget,
        velocity: f64,
    },
}

impl Default for GesturePhase {
    fn default() -> Self {
        Self::Idle
    }
}

/// Computes the normalized gesture progress $t$ from vertical travel $-\Delta Y$.
///
/// # Mathematical Formulation
/// $$t = \mathrm{clamp}\left(\frac{-\Delta Y}{\text{screen\_height} \times 0.45}, 0.0, 1.05\right)$$
///
/// Moving the finger upward produces negative $\Delta Y$, leading to positive progress $t$.
/// Dragging back down toward the bottom of the screen continuously reverses $t \to 0.0$.
///
/// # Arguments
/// * `delta_y` - Vertical displacement in pixels ($y_{\text{current}} - y_{\text{start}}$).
/// * `screen_height` - Total vertical display canvas height in pixels (e.g. 915.0).
///
/// # Returns
/// Normalized progress $t \in [0.0, 1.05]$.
#[inline]
pub fn compute_normalized_progress(delta_y: f64, screen_height: f64) -> f64 {
    let max_travel = (screen_height * 0.45).max(10.0);
    (-delta_y / max_travel).clamp(0.0, 1.05)
}

/// Computes the active application window scale factor $s(t)$ from normalized progress $t$.
///
/// # Mathematical Formulation
/// $$s(t) = 1.0 - t \cdot (1.0 - 0.68) = 1.0 - 0.32 \cdot t$$
///
/// When $t = 0.0$, $s = 1.0$ (fullscreen). When $t = 1.0$, $s = 0.68$ (overview card slot).
///
/// # Arguments
/// * `progress` - Normalized gesture progress $t$.
///
/// # Returns
/// Window scaling factor in range `[0.664, 1.0]`.
#[inline]
pub fn compute_window_scale(progress: f64) -> f64 {
    1.0 - (progress.clamp(0.0, 1.05) * (1.0 - 0.78))
}

/// Computes the active application window corner radius $r(t)$ in pixels.
///
/// # Mathematical Formulation
/// $$r(t) = t \cdot 28.0\text{ px}$$
///
/// Transitions smoothly from $0.0\text{px}$ (sharp rectangular fullscreen) to
/// $28.0\text{px}$ (Material 3 Expressive rounded card).
///
/// # Arguments
/// * `progress` - Normalized gesture progress $t$.
///
/// # Returns
/// Corner radius in pixels in range `[0.0, 29.4]`.
#[inline]
pub fn compute_corner_radius(progress: f64) -> f64 {
    (progress.clamp(0.0, 1.05) * 28.0).clamp(0.0, 28.0)
}

/// Computes the dimming scrim alpha overlay opacity $\alpha(t)$.
///
/// # Mathematical Formulation
/// Authentic Android 17 Material You scrim: subtle background dark tint, capped at 28% opacity
/// so wallpapers remain vibrant and legible under active gesture cards.
/// $$\alpha(t) = \mathrm{clamp}(t, 0.0, 1.0) \cdot 0.28$$
///
/// # Arguments
/// * `progress` - Normalized gesture progress $t$.
///
/// # Returns
/// Opacity in range `[0.0, 0.28]`.
#[inline]
pub fn compute_scrim_alpha(progress: f64) -> f64 {
    progress.clamp(0.0, 1.0) * 0.28
}

/// Computes the active application window scale during App-to-Home dismiss.
///
/// # Mathematical Formulation
/// Follows non-linear deceleration:
/// $$s(t) = (1.0 - t)^{1.2} \times 0.88 + 0.12$$
/// At $t = 0.0 \implies 1.0$ (fullscreen).
/// At $t = 1.0 \implies 0.12$ (matches squircle app icon size $\sim 56\text{px}$).
#[inline]
pub fn compute_home_collapse_scale(progress: f64) -> f64 {
    let t = progress.clamp(0.0, 1.0);
    (1.0 - t).powf(1.2) * 0.88 + 0.12
}

/// Applies non-linear rubber-band resistance when gesture progress exceeds $1.0$.
///
/// # Mathematical Formulation
/// $$t_{\text{damped}} = 1.0 + (t - 1.0) \times 0.25$$
#[inline]
pub fn rubber_band_clamp(progress: f64) -> f64 {
    if progress <= 1.0 {
        progress.max(0.0)
    } else {
        1.0 + (progress - 1.0) * 0.25
    }
}

/// Arbitrates between [`NavigationTarget::Home`], [`NavigationTarget::Overview`],
/// and [`NavigationTarget::Fullscreen`] upon vertical touch release.
///
/// # Mathematical Formulation
/// 1. Fast upward flick:
///    $$v_y < -500.0\text{ px/s} \implies \text{Home}$$
///    Authentic Android navigation dynamics: a quick, energetic upward flick from the
///    bottom navigation pill immediately dismisses the active application to the home screen.
///
/// 2. Fast downward fling:
///    $$v_y > 500.0\text{ px/s} \implies \text{Fullscreen}$$
///    Pulling back down reverses the gesture and smoothly snaps the active app back to fullscreen.
///
/// 3. Positional threshold (drag and hold or deliberate release):
///    $$t \ge 0.35 \iff s(t) \le 0.888 \implies \text{Overview}$$
///    Swiping upward and pausing or releasing past 35% normalized travel transitions to Overview.
///    Otherwise $\implies \text{Fullscreen}$.
///
/// # Arguments
/// * `progress` - Normalized progress $t$ at the moment of touch release.
/// * `velocity_y` - Instantaneous vertical velocity in pixels per second ($v_y < 0$ is upwards).
///
/// # Returns
/// Committed [`NavigationTarget`].
pub fn arbitrate_release(progress: f64, velocity_y: f64) -> NavigationTarget {
    const FLICK_THRESHOLD: f64 = 500.0;
    if velocity_y < -FLICK_THRESHOLD {
        NavigationTarget::Home
    } else if velocity_y > FLICK_THRESHOLD {
        NavigationTarget::Fullscreen
    } else if progress >= 0.35 {
        NavigationTarget::Overview
    } else {
        NavigationTarget::Fullscreen
    }
}

/// Arbitrates between [`NavigationTarget::NextApp`], [`NavigationTarget::PrevApp`],
/// and [`NavigationTarget::Fullscreen`] upon horizontal navigation swipe release.
///
/// # Mathematical Formulation
/// * $v_x < -350.0\text{ px/s} \text{ or } \Delta X < -(\text{width} \times 0.25) \implies \text{NextApp}$
/// * $v_x > 350.0\text{ px/s} \text{ or } \Delta X > (\text{width} \times 0.25) \implies \text{PrevApp}$
/// * Otherwise $\implies \text{Fullscreen}$ (revert to current app)
///
/// # Arguments
/// * `delta_x` - Horizontal displacement $\Delta X$ in pixels.
/// * `velocity_x` - Instantaneous horizontal velocity in pixels per second.
/// * `screen_width` - Logical screen width in pixels.
///
/// # Returns
/// Committed [`NavigationTarget`].
pub fn arbitrate_horizontal_release(
    delta_x: f64,
    velocity_x: f64,
    screen_width: f64,
) -> NavigationTarget {
    const FLICK_THRESHOLD: f64 = 350.0;
    let dist_threshold = (screen_width * 0.25).max(40.0);

    if velocity_x < -FLICK_THRESHOLD || delta_x < -dist_threshold {
        NavigationTarget::NextApp
    } else if velocity_x > FLICK_THRESHOLD || delta_x > dist_threshold {
        NavigationTarget::PrevApp
    } else {
        NavigationTarget::Fullscreen
    }
}

