//! Penumbra Gestures: Standalone Damped Spring Physics & Touch State Machine.
//!
//! Provides mathematically exact damped harmonic oscillator simulation (F = -k*x - c*v),
//! touch momentum tracking, gesture classification, and boundary snapping for the
//! Penumbra Mobile Desktop Environment.

pub mod nav;
pub mod spring;
pub mod state_machine;
pub mod touch;

#[cfg(test)]
mod tests;

pub use nav::{NavGesture, NavGestureConfig, NavGestureRecognizer};
pub use spring::Spring;
pub use state_machine::{GestureStateMachine, NavigationGesture, TouchState};
pub use touch::{calculate_snap_target, FlickDirection, TouchSample, TouchTracker, Vector2D};
