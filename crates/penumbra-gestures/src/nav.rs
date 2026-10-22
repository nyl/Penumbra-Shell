//! Bottom navigation bar gesture recognizer.
//!
//! Disambiguates the two vertical gestures performed from the bottom gesture bar:
//!
//! * **Fast flick up** (release velocity above threshold) -> [`NavGesture::GoHome`]
//! * **Slow drag up & hold** (distance > 80px, finger stationary for a short
//!   period) -> [`NavGesture::OpenOverview`]
//!
//! Hold detection must work even when the finger is completely still (no motion
//! events are delivered), so callers should invoke [`NavGestureRecognizer::poll`]
//! periodically (e.g. from a 16ms timer) while the pointer is pressed.

use std::time::Duration;

/// Gesture produced by the bottom navigation bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavGesture {
    /// Fast upward flick -> return to homescreen.
    GoHome,
    /// Drag up and hold -> open task switcher.
    OpenOverview,
    /// Swipe left on bottom bar -> switch to next running app.
    SwitchNextApp,
    /// Swipe right on bottom bar -> switch to previous running app.
    SwitchPrevApp,
}

/// Tunable thresholds for the navigation gesture recognizer.
#[derive(Debug, Clone, Copy)]
pub struct NavGestureConfig {
    /// Upward travel (px) required before a hold can open the overview.
    pub overview_distance: f64,
    /// Time (ms) the finger must stay (nearly) still to count as a hold.
    pub hold_duration: Duration,
    /// Radius (px) of movement tolerated while holding.
    pub hold_slop: f64,
    /// Upward release velocity (px/s) that classifies the gesture as a flick.
    pub flick_velocity: f64,
    /// Minimum upward travel (px) for a flick to count.
    pub flick_min_distance: f64,
    /// Minimum horizontal travel (px) to switch apps.
    pub switch_app_min_distance: f64,
    /// Window over which release velocity is estimated.
    pub velocity_window: Duration,
}

impl Default for NavGestureConfig {
    fn default() -> Self {
        Self {
            overview_distance: 80.0,
            hold_duration: Duration::from_millis(220),
            hold_slop: 6.0,
            flick_velocity: 650.0,
            flick_min_distance: 24.0,
            switch_app_min_distance: 35.0,
            velocity_window: Duration::from_millis(90),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    y: f64,
    t: Duration,
}

/// Stateful recognizer for a single bottom-bar touch sequence.
#[derive(Debug, Clone)]
pub struct NavGestureRecognizer {
    pub config: NavGestureConfig,
    active: bool,
    start_x: f64,
    current_x: f64,
    start_y: f64,
    current_y: f64,
    /// Anchor used for hold detection; reset whenever the finger leaves `hold_slop`.
    anchor_y: f64,
    anchor_t: Duration,
    samples: Vec<Sample>,
    overview_fired: bool,
}

impl Default for NavGestureRecognizer {
    fn default() -> Self {
        Self::new(NavGestureConfig::default())
    }
}

impl NavGestureRecognizer {
    pub fn new(config: NavGestureConfig) -> Self {
        Self {
            config,
            active: false,
            start_x: 0.0,
            current_x: 0.0,
            start_y: 0.0,
            current_y: 0.0,
            anchor_y: 0.0,
            anchor_t: Duration::ZERO,
            samples: Vec::with_capacity(32),
            overview_fired: false,
        }
    }

    /// Whether a touch sequence is currently being tracked.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Upward travel in px since touch-down (positive = moved up).
    pub fn upward_distance(&self) -> f64 {
        (self.start_y - self.current_y).max(0.0)
    }

    /// Horizontal travel in px since touch-down (positive = moved right, negative = moved left).
    pub fn horizontal_distance(&self) -> f64 {
        self.current_x - self.start_x
    }

    pub fn on_down(&mut self, x: f64, y: f64, t: Duration) {
        self.active = true;
        self.start_x = x;
        self.current_x = x;
        self.start_y = y;
        self.current_y = y;
        self.anchor_y = y;
        self.anchor_t = t;
        self.samples.clear();
        self.samples.push(Sample { y, t });
        self.overview_fired = false;
    }

    /// Records motion. May return [`NavGesture::OpenOverview`] once a hold is detected.
    pub fn on_move(&mut self, x: f64, y: f64, t: Duration) -> Option<NavGesture> {
        if !self.active {
            return None;
        }
        self.current_x = x;
        self.current_y = y;
        self.samples.push(Sample { y, t });
        let window = self.config.velocity_window * 3;
        self.samples.retain(|s| t.saturating_sub(s.t) <= window);

        if (y - self.anchor_y).abs() > self.config.hold_slop {
            self.anchor_y = y;
            self.anchor_t = t;
        }
        self.poll(t)
    }

    /// Checks for a stationary hold. Call periodically while pressed.
    pub fn poll(&mut self, t: Duration) -> Option<NavGesture> {
        if !self.active || self.overview_fired {
            return None;
        }
        let held = t.saturating_sub(self.anchor_t) >= self.config.hold_duration;
        if held && self.upward_distance() >= self.config.overview_distance {
            self.overview_fired = true;
            return Some(NavGesture::OpenOverview);
        }
        None
    }

    /// Upward velocity (px/s, positive = up) estimated over the recent window.
    pub fn upward_velocity(&self, now: Duration) -> f64 {
        let recent: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|s| now.saturating_sub(s.t) <= self.config.velocity_window)
            .collect();
        match (recent.first(), recent.last()) {
            (Some(a), Some(b)) if b.t > a.t => {
                let dt = (b.t - a.t).as_secs_f64();
                (a.y - b.y) / dt
            }
            _ => 0.0,
        }
    }

    /// Finishes the sequence and classifies it.
    pub fn on_up(&mut self, x: f64, y: f64, t: Duration) -> Option<NavGesture> {
        if !self.active {
            return None;
        }
        let _ = self.on_move(x, y, t);
        self.active = false;

        if self.overview_fired {
            // Overview already opened during the hold; releasing just commits it.
            return None;
        }

        let distance_y = self.upward_distance();
        let distance_x = self.horizontal_distance();
        let abs_dx = distance_x.abs();

        // 1. Prioritize horizontal edge bar swipe for quick app switching
        if abs_dx >= self.config.switch_app_min_distance && abs_dx > distance_y * 1.2 {
            return if distance_x < 0.0 {
                Some(NavGesture::SwitchNextApp)
            } else {
                Some(NavGesture::SwitchPrevApp)
            };
        }

        // 2. Vertical gestures: Flick to Home or Drag & Hold to Overview
        let velocity = self.upward_velocity(t);

        if velocity >= self.config.flick_velocity && distance_y >= self.config.flick_min_distance {
            Some(NavGesture::GoHome)
        } else if distance_y >= self.config.overview_distance {
            // Slow drag past the threshold released without a fling.
            Some(NavGesture::OpenOverview)
        } else if distance_y >= self.config.flick_min_distance {
            Some(NavGesture::GoHome)
        } else {
            None
        }
    }

    pub fn cancel(&mut self) {
        self.active = false;
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn fast_flick_goes_home() {
        let mut r = NavGestureRecognizer::default();
        r.on_down(200.0, 900.0, ms(0));
        assert_eq!(r.on_move(200.0, 870.0, ms(20)), None);
        assert_eq!(r.on_move(200.0, 820.0, ms(40)), None);
        assert_eq!(r.on_up(200.0, 760.0, ms(60)), Some(NavGesture::GoHome));
    }

    #[test]
    fn drag_and_hold_opens_overview_while_pressed() {
        let mut r = NavGestureRecognizer::default();
        r.on_down(200.0, 900.0, ms(0));
        // Slow drag up 120px over 600ms
        for i in 1..=12 {
            assert_eq!(r.on_move(200.0, 900.0 - i as f64 * 10.0, ms(i * 50)), None);
        }
        // Finger stays still: no motion events, only polling
        assert_eq!(r.poll(ms(700)), None);
        assert_eq!(r.poll(ms(830)), Some(NavGesture::OpenOverview));
        // Release does not fire a second gesture
        assert_eq!(r.on_up(200.0, 780.0, ms(900)), None);
    }

    #[test]
    fn hold_below_threshold_does_not_open_overview() {
        let mut r = NavGestureRecognizer::default();
        r.on_down(200.0, 900.0, ms(0));
        r.on_move(200.0, 860.0, ms(200));
        assert_eq!(r.poll(ms(1000)), None);
        // Slow short drag released -> home
        assert_eq!(r.on_up(200.0, 860.0, ms(1100)), Some(NavGesture::GoHome));
    }

    #[test]
    fn tap_is_ignored() {
        let mut r = NavGestureRecognizer::default();
        r.on_down(200.0, 900.0, ms(0));
        assert_eq!(r.on_up(201.0, 898.0, ms(80)), None);
    }

    #[test]
    fn switch_next_app_on_swipe_left() {
        let mut r = NavGestureRecognizer::default();
        r.on_down(200.0, 900.0, ms(0));
        assert_eq!(r.on_move(160.0, 898.0, ms(30)), None);
        assert_eq!(r.on_move(120.0, 896.0, ms(60)), None);
        assert_eq!(
            r.on_up(110.0, 895.0, ms(90)),
            Some(NavGesture::SwitchNextApp)
        );
    }

    #[test]
    fn switch_prev_app_on_swipe_right() {
        let mut r = NavGestureRecognizer::default();
        r.on_down(200.0, 900.0, ms(0));
        assert_eq!(r.on_move(240.0, 898.0, ms(30)), None);
        assert_eq!(r.on_move(280.0, 896.0, ms(60)), None);
        assert_eq!(
            r.on_up(290.0, 895.0, ms(90)),
            Some(NavGesture::SwitchPrevApp)
        );
    }
}
