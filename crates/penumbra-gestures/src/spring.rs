//! Second-order damped harmonic oscillator physics engine.
//!
//! Governed by the differential equation:
//!   m * x''(t) + c * x'(t) + k * (x(t) - target) = 0
//! or equivalently:
//!   F = -k * (x - target) - c * v
//!
//! Supports exact analytical stepping (immune to numerical instability or dt jitter)
//! and provides Material 3 Expressive motion presets.

/// Second-order damped harmonic oscillator spring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    /// Spring stiffness constant k (N/m or px/s^2 equivalent).
    pub stiffness: f64,
    /// Damping ratio zeta (dimensionless).
    /// - zeta < 1.0: Underdamped (oscillatory decay)
    /// - zeta == 1.0: Critically damped (fastest return without overshoot)
    /// - zeta > 1.0: Overdamped (sluggish monotonic return)
    pub damping_ratio: f64,
    /// Mass m of the object. Default is 1.0.
    pub mass: f64,
    /// Target equilibrium position.
    pub target: f64,
    /// Current position.
    pub position: f64,
    /// Current velocity.
    pub velocity: f64,
}

impl Default for Spring {
    fn default() -> Self {
        Self::expressive()
    }
}

impl Spring {
    /// Creates a new spring with explicit stiffness, damping ratio, and mass.
    pub fn new(stiffness: f64, damping_ratio: f64, mass: f64) -> Self {
        assert!(stiffness > 0.0, "Stiffness must be positive");
        assert!(damping_ratio >= 0.0, "Damping ratio cannot be negative");
        assert!(mass > 0.0, "Mass must be positive");

        Self {
            stiffness,
            damping_ratio,
            mass,
            target: 0.0,
            position: 0.0,
            velocity: 0.0,
        }
    }

    /// Creates a spring from raw physical parameters: stiffness k, damping coefficient c, and mass m.
    ///
    /// Damping ratio zeta is calculated as:
    ///   zeta = c / (2 * sqrt(k * m))
    pub fn from_physics(stiffness: f64, damping_coeff: f64, mass: f64) -> Self {
        assert!(stiffness > 0.0, "Stiffness must be positive");
        assert!(damping_coeff >= 0.0, "Damping coefficient cannot be negative");
        assert!(mass > 0.0, "Mass must be positive");
        let damping_ratio = damping_coeff / (2.0 * (stiffness * mass).sqrt());
        Self::new(stiffness, damping_ratio, mass)
    }

    /// Material 3 Expressive preset:
    /// Snappy, responsive with a subtle, natural bounce (stiffness 320.0, damping 0.82).
    pub fn expressive() -> Self {
        Self::new(320.0, 0.82, 1.0)
    }

    /// Critically damped preset:
    /// Returns to rest as quickly as possible with 0 overshoot (damping 1.0).
    pub fn critically_damped() -> Self {
        Self::new(300.0, 1.0, 1.0)
    }

    /// Bouncy preset for playful UI elements.
    pub fn bouncy() -> Self {
        Self::new(280.0, 0.65, 1.0)
    }

    /// Gentle, smooth glide preset for large drawer transitions.
    pub fn gentle() -> Self {
        Self::new(160.0, 0.95, 1.0)
    }

    /// Builder: sets target position.
    pub fn with_target(mut self, target: f64) -> Self {
        self.target = target;
        self
    }

    /// Builder: sets current position.
    pub fn with_position(mut self, position: f64) -> Self {
        self.position = position;
        self
    }

    /// Builder: sets current velocity.
    pub fn with_velocity(mut self, velocity: f64) -> Self {
        self.velocity = velocity;
        self
    }

    /// Sets the target equilibrium position.
    pub fn set_target(&mut self, target: f64) {
        self.target = target;
    }

    /// Sets the current position.
    pub fn set_position(&mut self, pos: f64) {
        self.position = pos;
    }

    /// Sets the current velocity.
    pub fn set_velocity(&mut self, vel: f64) {
        self.velocity = vel;
    }

    /// Natural undamped angular frequency omega_0 = sqrt(k / m).
    pub fn natural_frequency(&self) -> f64 {
        (self.stiffness / self.mass).sqrt()
    }

    /// Damping coefficient c = 2 * zeta * sqrt(k * m).
    pub fn damping_coefficient(&self) -> f64 {
        2.0 * self.damping_ratio * (self.stiffness * self.mass).sqrt()
    }

    /// Instantaneous net force F = -k * (x - target) - c * v.
    pub fn force(&self) -> f64 {
        let displacement = self.position - self.target;
        -self.stiffness * displacement - self.damping_coefficient() * self.velocity
    }

    /// Advances the spring by time delta `dt` (in seconds) using the exact analytical solution.
    ///
    /// This method is unconditionally stable regardless of `dt` size.
    pub fn step(&mut self, dt: f64) -> f64 {
        if dt <= 0.0 {
            return self.position;
        }

        let w0 = self.natural_frequency();
        let zeta = self.damping_ratio;
        let x0 = self.position - self.target;
        let v0 = self.velocity;

        let (new_x, new_v) = if (zeta - 1.0).abs() < 1e-5 {
            // Critically damped (zeta == 1.0)
            let decay = (-w0 * dt).exp();
            let c1 = x0;
            let c2 = v0 + w0 * x0;
            let x = decay * (c1 + c2 * dt);
            let v = decay * (v0 - w0 * c2 * dt);
            (x, v)
        } else if zeta < 1.0 {
            // Underdamped (0 <= zeta < 1.0)
            let wd = w0 * (1.0 - zeta * zeta).sqrt();
            let decay = (-zeta * w0 * dt).exp();
            let sin_term = (wd * dt).sin();
            let cos_term = (wd * dt).cos();

            let c1 = x0;
            let c2 = (v0 + zeta * w0 * x0) / wd;

            let x = decay * (c1 * cos_term + c2 * sin_term);
            let v = decay
                * ((-zeta * w0 * (c1 * cos_term + c2 * sin_term))
                    + (-wd * c1 * sin_term + wd * c2 * cos_term));
            (x, v)
        } else {
            // Overdamped (zeta > 1.0)
            let wd = w0 * (zeta * zeta - 1.0).sqrt();
            let r1 = -zeta * w0 + wd;
            let r2 = -zeta * w0 - wd;

            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;

            let exp1 = (r1 * dt).exp();
            let exp2 = (r2 * dt).exp();

            let x = c1 * exp1 + c2 * exp2;
            let v = c1 * r1 * exp1 + c2 * r2 * exp2;
            (x, v)
        };

        self.position = new_x + self.target;
        self.velocity = new_v;

        self.position
    }

    /// Checks if the spring has settled within specified position and velocity thresholds.
    pub fn is_settled(&self, pos_threshold: f64, vel_threshold: f64) -> bool {
        (self.position - self.target).abs() <= pos_threshold && self.velocity.abs() <= vel_threshold
    }

    /// Snaps position to target and zeroes velocity if settled within given thresholds.
    pub fn snap_if_settled(&mut self, pos_threshold: f64, vel_threshold: f64) -> bool {
        if self.is_settled(pos_threshold, vel_threshold) {
            self.position = self.target;
            self.velocity = 0.0;
            true
        } else {
            false
        }
    }

    /// Returns estimated settling time (in seconds) to reach within `threshold` of target.
    pub fn estimated_settling_time(&self, threshold: f64) -> f64 {
        let w0 = self.natural_frequency();
        let zeta = self.damping_ratio.max(0.01);
        let displacement = (self.position - self.target).abs().max(threshold * 1.01);
        // Exponential decay envelope is exp(-zeta * w0 * t) <= threshold / displacement
        // -zeta * w0 * t <= ln(threshold / displacement)
        let ratio = threshold / displacement;
        if ratio >= 1.0 {
            0.0
        } else {
            -ratio.ln() / (zeta * w0)
        }
    }
}
