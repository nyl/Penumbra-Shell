//! Touch tracking and momentum calculations for mobile gesture handling.

use std::collections::VecDeque;
use std::time::Duration;

/// 2D Vector representation for velocity, position, and momentum.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector2D {
    pub x: f64,
    pub y: f64,
}

impl Vector2D {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn magnitude(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    pub fn dot(&self, other: &Self) -> f64 {
        self.x * other.x + self.y * other.y
    }
}

/// Recorded historical touch sample.
#[derive(Debug, Clone, Copy)]
pub struct TouchSample {
    pub x: f64,
    pub y: f64,
    pub timestamp: Duration,
}

/// Directional flick categorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlickDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Gesture touch tracker maintaining a sliding window of historical coordinates and timestamps.
/// Computes release velocity and momentum vectors for swipe-to-dismiss and snap-to-edge.
#[derive(Debug, Clone)]
pub struct TouchTracker {
    /// History buffer of recent samples.
    samples: VecDeque<TouchSample>,
    /// Maximum time window (e.g., 150ms) to consider for release velocity.
    window_duration: Duration,
    /// Maximum capacity of samples.
    max_samples: usize,
}

impl Default for TouchTracker {
    fn default() -> Self {
        Self::new(Duration::from_millis(150), 20)
    }
}

impl TouchTracker {
    pub fn new(window_duration: Duration, max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples),
            window_duration,
            max_samples,
        }
    }

    /// Clears all recorded touch samples.
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Adds a new touch sample at coordinates (x, y) with a timestamp.
    pub fn record_sample(&mut self, x: f64, y: f64, timestamp: Duration) {
        // Evict samples outside the time window relative to the new timestamp
        while let Some(front) = self.samples.front() {
            if timestamp.saturating_sub(front.timestamp) > self.window_duration {
                self.samples.pop_front();
            } else {
                break;
            }
        }

        if self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }

        self.samples.push_back(TouchSample { x, y, timestamp });
    }

    /// Number of samples currently held.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Returns the most recent sample recorded.
    pub fn latest_sample(&self) -> Option<&TouchSample> {
        self.samples.back()
    }

    /// Returns the initial sample in the current tracking stroke.
    pub fn initial_sample(&self) -> Option<&TouchSample> {
        self.samples.front()
    }

    /// Computes instantaneous release velocity (pixels per second) across the sliding window.
    ///
    /// Uses linear regression (least squares) across time if sufficient samples exist,
    /// falling back to finite difference between initial and final window samples.
    pub fn compute_velocity(&self) -> Vector2D {
        if self.samples.len() < 2 {
            return Vector2D::ZERO;
        }

        let newest = self.samples.back().unwrap();
        let oldest = self.samples.front().unwrap();

        let dt_total = (newest.timestamp.saturating_sub(oldest.timestamp)).as_secs_f64();
        if dt_total < 1e-4 {
            return Vector2D::ZERO;
        }

        // Linear regression for optimal noise reduction and responsiveness:
        // slope = (N * sum(t*p) - sum(t)*sum(p)) / (N * sum(t^2) - (sum(t))^2)
        let n = self.samples.len() as f64;
        let t0 = oldest.timestamp.as_secs_f64();

        let mut sum_t = 0.0;
        let mut sum_t2 = 0.0;
        let mut sum_x = 0.0;
        let mut sum_tx = 0.0;
        let mut sum_y = 0.0;
        let mut sum_ty = 0.0;

        for s in &self.samples {
            let t = s.timestamp.as_secs_f64() - t0;
            sum_t += t;
            sum_t2 += t * t;
            sum_x += s.x;
            sum_tx += t * s.x;
            sum_y += s.y;
            sum_ty += t * s.y;
        }

        let denom = n * sum_t2 - sum_t * sum_t;
        if denom.abs() < 1e-6 {
            // Fall back to simple delta
            let vx = (newest.x - oldest.x) / dt_total;
            let vy = (newest.y - oldest.y) / dt_total;
            return Vector2D::new(vx, vy);
        }

        let vx = (n * sum_tx - sum_t * sum_x) / denom;
        let vy = (n * sum_ty - sum_t * sum_y) / denom;

        Vector2D::new(vx, vy)
    }

    /// Calculates the release momentum vector.
    pub fn release_momentum(&self) -> Vector2D {
        self.compute_velocity()
    }

    /// Detects if the current movement constitutes a directional flick
    /// based on minimum velocity (px/s) and minimum stroke displacement.
    pub fn detect_flick(&self, min_velocity: f64, min_distance: f64) -> Option<FlickDirection> {
        if self.samples.len() < 2 {
            return None;
        }

        let oldest = self.samples.front().unwrap();
        let newest = self.samples.back().unwrap();

        let dx = newest.x - oldest.x;
        let dy = newest.y - oldest.y;
        let dist = (dx * dx + dy * dy).sqrt();

        if dist < min_distance {
            return None;
        }

        let velocity = self.compute_velocity();
        let speed = velocity.magnitude();

        if speed < min_velocity {
            return None;
        }

        if dx.abs() > dy.abs() {
            if dx > 0.0 && velocity.x > 0.0 {
                Some(FlickDirection::Right)
            } else if dx < 0.0 && velocity.x < 0.0 {
                Some(FlickDirection::Left)
            } else {
                None
            }
        } else {
            if dy > 0.0 && velocity.y > 0.0 {
                Some(FlickDirection::Down)
            } else if dy < 0.0 && velocity.y < 0.0 {
                Some(FlickDirection::Up)
            } else {
                None
            }
        }
    }
}

/// Computes optimal snap-to-edge target for a 1D position given its release momentum.
///
/// * `current_pos`: The current position (e.g., drawer offset in px).
/// * `velocity`: The release velocity in px/s.
/// * `targets`: Array of allowed snap equilibrium targets (e.g. `[0.0, 915.0]`).
/// * `fling_velocity_threshold`: Minimum velocity magnitude to consider a fling rather than closest snap.
pub fn calculate_snap_target(
    current_pos: f64,
    velocity: f64,
    targets: &[f64],
    fling_velocity_threshold: f64,
) -> f64 {
    if targets.is_empty() {
        return current_pos;
    }

    if targets.len() == 1 {
        return targets[0];
    }

    // If strong fling velocity exists, prioritize directional snapping
    if velocity.abs() >= fling_velocity_threshold {
        if velocity > 0.0 {
            // Find target greater than current_pos
            if let Some(&t) = targets.iter().find(|&&t| t > current_pos) {
                return t;
            }
        } else {
            // Find target less than current_pos
            if let Some(&t) = targets.iter().rev().find(|&&t| t < current_pos) {
                return t;
            }
        }
    }

    // Otherwise, project position with minor momentum decay and snap to nearest target
    let projected_pos = current_pos + velocity * 0.12;
    *targets
        .iter()
        .min_by(|&&a, &&b| {
            (a - projected_pos)
                .abs()
                .partial_cmp(&(b - projected_pos).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(&targets[0])
}
