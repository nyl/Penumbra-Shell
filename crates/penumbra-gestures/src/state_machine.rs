//! Touch State Machine for mobile gestures and drawer interactions.

use crate::spring::Spring;
use crate::touch::{calculate_snap_target, FlickDirection, TouchTracker, Vector2D};
use std::time::Duration;

/// Navigation gesture action detected by the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationGesture {
    /// Flick up from bottom edge: Return to Home screen / minimize current surface.
    ReturnHome,
    /// Drag up from bottom edge and hold: Open Overview / App Switcher.
    OverviewRecents,
    /// Horizontal swipe on bottom bar: Fast switch between adjacent tasks.
    FastSwitchAdjacent(FlickDirection),
    /// Pull down from top edge: Reveal Quick Settings drawer.
    OpenQuickSettings,
    /// Swipe up from open Quick Settings: Dismiss drawer.
    DismissQuickSettings,
}

/// Operational state of the touch gesture tracker.
#[derive(Debug, Clone, PartialEq)]
pub enum TouchState {
    /// No pointer is currently in contact with the display.
    Idle,
    /// Contact initiated, waiting to exceed slop threshold (tap or gesture candidate).
    ContactDown {
        start_x: f64,
        start_y: f64,
        current_x: f64,
        current_y: f64,
        start_time: Duration,
    },
    /// Pointer has moved beyond touch slop; active interactive drag.
    Dragging {
        start_x: f64,
        start_y: f64,
        start_time: Duration,
        current_x: f64,
        current_y: f64,
        delta_x: f64,
        delta_y: f64,
        elapsed: Duration,
    },
    /// Pointer released; motion animated by damped spring oscillator until settled.
    Flinging { spring_x: Spring, spring_y: Spring },
    /// Animation has reached equilibrium rest state.
    Settled,
}

/// Gesture state machine handling touch sequences, slop detection, hold timers, and spring handoff.
#[derive(Debug, Clone)]
pub struct GestureStateMachine {
    pub state: TouchState,
    pub touch_tracker: TouchTracker,
    /// Slop in pixels required before transitioning from ContactDown to Dragging.
    pub touch_slop: f64,
    /// Duration threshold to trigger a "hold" action (e.g. 400ms for Overview).
    pub hold_duration: Duration,
    /// Minimum velocity (px/s) required for fling gestures.
    pub min_fling_velocity: f64,
}

impl Default for GestureStateMachine {
    fn default() -> Self {
        Self {
            state: TouchState::Idle,
            touch_tracker: TouchTracker::default(),
            touch_slop: 8.0,
            hold_duration: Duration::from_millis(400),
            min_fling_velocity: 450.0,
        }
    }
}

impl GestureStateMachine {
    pub fn new(touch_slop: f64, hold_duration: Duration, min_fling_velocity: f64) -> Self {
        Self {
            state: TouchState::Idle,
            touch_tracker: TouchTracker::default(),
            touch_slop,
            hold_duration,
            min_fling_velocity,
        }
    }

    /// Invoked on pointer down event.
    pub fn on_down(&mut self, x: f64, y: f64, timestamp: Duration) {
        self.touch_tracker.reset();
        self.touch_tracker.record_sample(x, y, timestamp);
        self.state = TouchState::ContactDown {
            start_x: x,
            start_y: y,
            current_x: x,
            current_y: y,
            start_time: timestamp,
        };
    }

    /// Invoked on pointer move event.
    pub fn on_move(&mut self, x: f64, y: f64, timestamp: Duration) {
        self.touch_tracker.record_sample(x, y, timestamp);

        match self.state {
            TouchState::ContactDown {
                start_x,
                start_y,
                start_time,
                ..
            } => {
                let dx = x - start_x;
                let dy = y - start_y;
                let dist = (dx * dx + dy * dy).sqrt();

                if dist >= self.touch_slop {
                    let elapsed = timestamp.saturating_sub(start_time);
                    self.state = TouchState::Dragging {
                        start_x,
                        start_y,
                        start_time,
                        current_x: x,
                        current_y: y,
                        delta_x: dx,
                        delta_y: dy,
                        elapsed,
                    };
                } else {
                    self.state = TouchState::ContactDown {
                        start_x,
                        start_y,
                        current_x: x,
                        current_y: y,
                        start_time,
                    };
                }
            }
            TouchState::Dragging {
                start_x,
                start_y,
                start_time,
                ..
            } => {
                let dx = x - start_x;
                let dy = y - start_y;
                let elapsed = timestamp.saturating_sub(start_time);

                self.state = TouchState::Dragging {
                    start_x,
                    start_y,
                    start_time,
                    current_x: x,
                    current_y: y,
                    delta_x: dx,
                    delta_y: dy,
                    elapsed,
                };
            }
            _ => {}
        }
    }

    /// Invoked on pointer up event. Returns an optional recognized NavigationGesture.
    pub fn on_up(
        &mut self,
        x: f64,
        y: f64,
        timestamp: Duration,
        viewport_height: f64,
    ) -> Option<NavigationGesture> {
        self.touch_tracker.record_sample(x, y, timestamp);
        let velocity = self.touch_tracker.compute_velocity();

        let gesture = match self.state {
            TouchState::Dragging {
                start_x: _,
                start_y,
                delta_x,
                delta_y,
                elapsed,
                ..
            } => {
                let is_bottom_bar = start_y >= viewport_height - 60.0;
                let is_top_bar = start_y <= 60.0;

                if is_bottom_bar {
                    // Check for horizontal fast-switch
                    if delta_x.abs() > delta_y.abs() && delta_x.abs() > 40.0 {
                        if delta_x > 0.0 {
                            Some(NavigationGesture::FastSwitchAdjacent(FlickDirection::Right))
                        } else {
                            Some(NavigationGesture::FastSwitchAdjacent(FlickDirection::Left))
                        }
                    } else if delta_y < -40.0 || velocity.y < -self.min_fling_velocity {
                        // Check if held long enough before release
                        if elapsed >= self.hold_duration {
                            Some(NavigationGesture::OverviewRecents)
                        } else {
                            Some(NavigationGesture::ReturnHome)
                        }
                    } else {
                        None
                    }
                } else if is_top_bar && (delta_y > 40.0 || velocity.y > self.min_fling_velocity) {
                    Some(NavigationGesture::OpenQuickSettings)
                } else if delta_y < -40.0 || velocity.y < -self.min_fling_velocity {
                    Some(NavigationGesture::DismissQuickSettings)
                } else {
                    None
                }
            }
            _ => None,
        };

        // Transition to flinging or settled
        let release_x = x;
        let release_y = y;
        let mut spring_x = Spring::expressive()
            .with_position(release_x)
            .with_velocity(velocity.x);
        let mut spring_y = Spring::expressive()
            .with_position(release_y)
            .with_velocity(velocity.y);

        // Snap target y to either top (0.0) or settled position
        let target_y = calculate_snap_target(
            release_y,
            velocity.y,
            &[0.0, viewport_height],
            self.min_fling_velocity,
        );
        spring_y.set_target(target_y);
        spring_x.set_target(release_x);

        self.state = TouchState::Flinging { spring_x, spring_y };

        gesture
    }

    /// Invoked on gesture cancel (interrupted touch).
    pub fn on_cancel(&mut self) {
        self.state = TouchState::Idle;
        self.touch_tracker.reset();
    }

    /// Advances the flinging physics by delta time dt.
    /// Returns the current (x, y) spring positions.
    pub fn step_physics(&mut self, dt: f64) -> Option<Vector2D> {
        if let TouchState::Flinging {
            ref mut spring_x,
            ref mut spring_y,
        } = self.state
        {
            let px = spring_x.step(dt);
            let py = spring_y.step(dt);

            let settled_x = spring_x.is_settled(0.5, 5.0);
            let settled_y = spring_y.is_settled(0.5, 5.0);

            if settled_x && settled_y {
                let target_x = spring_x.target;
                let target_y = spring_y.target;
                self.state = TouchState::Settled;
                Some(Vector2D::new(target_x, target_y))
            } else {
                Some(Vector2D::new(px, py))
            }
        } else {
            None
        }
    }
}
