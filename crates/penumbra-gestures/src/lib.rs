//! Penumbra Gestures: High-performance in-process mathematical physics and touch tracking.
//!
//! Provides:
//! - Exact second-order damped harmonic oscillator physics ([`SpringSolver`], [`DampedHarmonicOscillator`]).
//! - Exponential Moving Average touch velocity estimation ([`VelocityTracker`]).
//! - Normalized Android 17 gesture transformation, scaling, and arbitration ([`GesturePhase`], [`NavigationTarget`]).
//!
//! Designed as an internal zero-dependency library crate to eliminate IPC frame-pacing latency.

pub mod gesture_state;
pub mod nav;
pub mod spring;
pub mod state_machine;
pub mod touch;
pub mod velocity;

#[cfg(test)]
mod tests;

pub use gesture_state::{
    arbitrate_horizontal_release, arbitrate_release, compute_corner_radius,
    compute_home_collapse_scale, compute_normalized_progress, compute_scrim_alpha,
    compute_window_scale, rubber_band_clamp, GesturePhase, NavigationTarget,
};
pub use nav::{NavGesture, NavGestureConfig, NavGestureRecognizer};
pub use spring::{DampedHarmonicOscillator, Spring, SpringSolver};
pub use state_machine::{GestureStateMachine, NavigationGesture, TouchState};
pub use touch::{calculate_snap_target, FlickDirection, TouchSample, TouchTracker, Vector2D};
pub use velocity::VelocityTracker;
