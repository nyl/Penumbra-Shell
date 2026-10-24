//! Real-time touch velocity estimation using Exponential Moving Average (EMA).
//!
//! Provides instantaneous and filtered velocity tracking for mobile gesture interactions,
//! filtering high-frequency digitizer noise while guaranteeing zero-latency phase response.

use std::time::Duration;

/// Real-time 2D touch velocity tracker using an Exponential Moving Average (EMA) filter.
///
/// # Mathematical Formulation
/// For each incoming touch sample at coordinate $(x_i, y_i)$ and time $t_i$:
///
/// 1. Compute instantaneous sample velocity:
///    $$v_{\text{inst}, x} = \frac{x_i - x_{i-1}}{t_i - t_{i-1}}, \quad v_{\text{inst}, y} = \frac{y_i - y_{i-1}}{t_i - t_{i-1}}$$
///
/// 2. Apply Exponential Moving Average (EMA) filter:
///    $$v_{x} = \alpha \cdot v_{\text{inst}, x} + (1 - \alpha) \cdot v_{x}$$
///    $$v_{y} = \alpha \cdot v_{\text{inst}, y} + (1 - \alpha) \cdot v_{y}$$
///
/// where $\alpha \in [0.0, 1.0]$ is the smoothing factor (default $0.8$). A higher $\alpha$
/// prioritizes responsiveness, while lower values offer increased noise suppression.
#[derive(Debug, Clone)]
pub struct VelocityTracker {
    /// Smoothing parameter for EMA filter. Default: 0.8.
    alpha: f64,
    /// Last recorded position coordinate (X).
    last_x: f64,
    /// Last recorded position coordinate (Y).
    last_y: f64,
    /// Last recorded sample timestamp.
    last_time: Option<Duration>,
    /// Filtered velocity in X axis (pixels per second).
    velocity_x: f64,
    /// Filtered velocity in Y axis (pixels per second).
    velocity_y: f64,
    /// Whether at least one movement delta has been incorporated.
    has_samples: bool,
}

impl Default for VelocityTracker {
    fn default() -> Self {
        Self::new(0.8)
    }
}

impl VelocityTracker {
    /// Constructs a new [`VelocityTracker`] with the given smoothing parameter $\alpha$.
    ///
    /// # Arguments
    /// * `alpha` - Smoothing factor in range `[0.0, 1.0]`. Typically `0.8` for touch tracking.
    ///
    /// # Returns
    /// An initialized velocity tracker with zero initial velocity.
    pub fn new(alpha: f64) -> Self {
        Self {
            alpha: alpha.clamp(0.01, 1.0),
            last_x: 0.0,
            last_y: 0.0,
            last_time: None,
            velocity_x: 0.0,
            velocity_y: 0.0,
            has_samples: false,
        }
    }

    /// Resets tracker state to idle with zero velocity.
    pub fn reset(&mut self) {
        self.last_x = 0.0;
        self.last_y = 0.0;
        self.last_time = None;
        self.velocity_x = 0.0;
        self.velocity_y = 0.0;
        self.has_samples = false;
    }

    /// Registers an initial touch contact point (`TouchDown`).
    ///
    /// # Arguments
    /// * `x` - Initial X coordinate in pixels.
    /// * `y` - Initial Y coordinate in pixels.
    /// * `time` - Timestamp of contact down.
    pub fn start(&mut self, x: f64, y: f64, time: Duration) {
        self.last_x = x;
        self.last_y = y;
        self.last_time = Some(time);
        self.velocity_x = 0.0;
        self.velocity_y = 0.0;
        self.has_samples = false;
    }

    /// Incorporates a touch movement update (`TouchMove`).
    ///
    /// # Arguments
    /// * `x` - Updated X coordinate in pixels.
    /// * `y` - Updated Y coordinate in pixels.
    /// * `time` - Timestamp of updated position.
    ///
    /// # Returns
    /// Current estimated instantaneous velocity $(v_x, v_y)$ in pixels per second.
    pub fn update(&mut self, x: f64, y: f64, time: Duration) -> (f64, f64) {
        if let Some(prev_time) = self.last_time {
            let dt = (time.saturating_sub(prev_time)).as_secs_f64();
            if dt > 0.0005 {
                let inst_vx = (x - self.last_x) / dt;
                let inst_vy = (y - self.last_y) / dt;

                if !self.has_samples {
                    self.velocity_x = inst_vx;
                    self.velocity_y = inst_vy;
                    self.has_samples = true;
                } else {
                    self.velocity_x = self.alpha * inst_vx + (1.0 - self.alpha) * self.velocity_x;
                    self.velocity_y = self.alpha * inst_vy + (1.0 - self.alpha) * self.velocity_y;
                }

                self.last_x = x;
                self.last_y = y;
                self.last_time = Some(time);
            }
        } else {
            self.start(x, y, time);
        }

        (self.velocity_x, self.velocity_y)
    }

    /// Returns the current filtered velocity in X and Y axes in pixels per second.
    ///
    /// # Returns
    /// Tuple `(v_x, v_y)` where positive $v_y$ denotes downward motion and negative $v_y$ denotes upward motion.
    pub fn velocity(&self) -> (f64, f64) {
        (self.velocity_x, self.velocity_y)
    }

    /// Returns the current vertical velocity $v_y$ in pixels per second.
    ///
    /// # Returns
    /// Velocity in pixels per second ($v_y < 0$ for upward swipe, $v_y > 0$ for downward swipe).
    pub fn velocity_y(&self) -> f64 {
        self.velocity_y
    }

    /// Returns the current horizontal velocity $v_x$ in pixels per second.
    ///
    /// # Returns
    /// Velocity in pixels per second.
    pub fn velocity_x(&self) -> f64 {
        self.velocity_x
    }
}
