//! Second-order damped harmonic oscillator physics engine.
//!
//! Governed by the differential equation:
//! $$m \ddot{x}(t) + c \dot{x}(t) + k (x(t) - x_{\text{target}}) = 0$$
//! or equivalently:
//! $$F = -k (x - x_{\text{target}}) - c v$$
//!
//! Provides exact closed-form analytical stepping (immune to numerical instability or $dt$ jitter)
//! as well as numerical Semi-Implicit Euler and Runge-Kutta 4th order (RK4) integration,
//! with specialized presets for Android 17 (Pixel) navigation dynamics.

/// Physical specification of a damped harmonic oscillator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DampedHarmonicOscillator {
    /// Spring stiffness constant $k$ (in $\text{N/m}$ or equivalent $\text{px/s}^2$).
    pub stiffness: f64,
    /// Viscous damping coefficient $c$ (in $\text{N}\cdot\text{s/m}$).
    pub damping_coefficient: f64,
    /// Moving mass $m$ (typically $1.0$).
    pub mass: f64,
}

impl DampedHarmonicOscillator {
    /// Constructs an oscillator from raw physical parameters: stiffness $k$, damping coefficient $c$, and mass $m$.
    ///
    /// # Arguments
    /// * `stiffness` - Spring constant $k > 0$.
    /// * `damping_coefficient` - Damping coefficient $c \ge 0$.
    /// * `mass` - Mass $m > 0$.
    pub fn new(stiffness: f64, damping_coefficient: f64, mass: f64) -> Self {
        assert!(stiffness > 0.0, "Stiffness must be positive");
        assert!(damping_coefficient >= 0.0, "Damping coefficient cannot be negative");
        assert!(mass > 0.0, "Mass must be positive");
        Self {
            stiffness,
            damping_coefficient,
            mass,
        }
    }

    /// Constructs an oscillator from stiffness $k$, damping ratio $\zeta$, and mass $m$.
    ///
    /// # Mathematical Formulation
    /// $$c = 2 \zeta \sqrt{k m}$$
    pub fn from_damping_ratio(stiffness: f64, damping_ratio: f64, mass: f64) -> Self {
        assert!(stiffness > 0.0, "Stiffness must be positive");
        assert!(damping_ratio >= 0.0, "Damping ratio cannot be negative");
        assert!(mass > 0.0, "Mass must be positive");
        let c = 2.0 * damping_ratio * (stiffness * mass).sqrt();
        Self::new(stiffness, c, mass)
    }

    /// Natural undamped angular frequency:
    /// $$\omega_0 = \sqrt{\frac{k}{m}}$$
    #[inline]
    pub fn natural_frequency(&self) -> f64 {
        (self.stiffness / self.mass).sqrt()
    }

    /// Dimensionless damping ratio:
    /// $$\zeta = \frac{c}{2 \sqrt{k m}}$$
    #[inline]
    pub fn damping_ratio(&self) -> f64 {
        self.damping_coefficient / (2.0 * (self.stiffness * self.mass).sqrt())
    }

    /// Damped angular frequency for underdamped systems:
    /// $$\omega_d = \omega_0 \sqrt{|1 - \zeta^2|}$$
    #[inline]
    pub fn damped_frequency(&self) -> f64 {
        let w0 = self.natural_frequency();
        let zeta = self.damping_ratio();
        w0 * (1.0 - zeta * zeta).abs().sqrt()
    }
}

/// Second-order damped harmonic spring solver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringSolver {
    /// Physical oscillator parameters.
    pub oscillator: DampedHarmonicOscillator,
    /// Target equilibrium position $x_{\text{target}}$.
    pub target: f64,
    /// Current position $x$.
    pub position: f64,
    /// Current velocity $v$.
    pub velocity: f64,
}

/// Backward compatibility alias for [`SpringSolver`].
pub type Spring = SpringSolver;

impl Default for SpringSolver {
    fn default() -> Self {
        Self::overview()
    }
}

impl SpringSolver {
    /// Creates a spring solver with explicit stiffness $k$, damping ratio $\zeta$, and mass $m$.
    ///
    /// # Arguments
    /// * `stiffness` - Spring stiffness $k$.
    /// * `damping_ratio` - Damping ratio $\zeta$.
    /// * `mass` - Mass $m$.
    pub fn new(stiffness: f64, damping_ratio: f64, mass: f64) -> Self {
        Self {
            oscillator: DampedHarmonicOscillator::from_damping_ratio(stiffness, damping_ratio, mass),
            target: 0.0,
            position: 0.0,
            velocity: 0.0,
        }
    }

    /// Creates a spring solver from physical constants: stiffness $k$, damping coefficient $c$, and mass $m$.
    ///
    /// # Arguments
    /// * `stiffness` - Spring stiffness $k$.
    /// * `damping_coeff` - Damping coefficient $c$.
    /// * `mass` - Mass $m$.
    pub fn from_physics(stiffness: f64, damping_coeff: f64, mass: f64) -> Self {
        Self {
            oscillator: DampedHarmonicOscillator::new(stiffness, damping_coeff, mass),
            target: 0.0,
            position: 0.0,
            velocity: 0.0,
        }
    }

    /// Android 17 (Pixel) Overview Navigation Spring preset:
    ///
    /// # Mathematical Formulation
    /// - Stiffness: $k = 300.0$
    /// - Damping: $c = 28.0$
    /// - Mass: $m = 1.0$
    /// - Damping ratio: $\zeta = \frac{28.0}{2\sqrt{300.0}} \approx 0.8083$ (authentic Material underdamped decay).
    pub fn overview() -> Self {
        Self::from_physics(300.0, 28.0, 1.0)
    }

    /// Android 17 Quick Switch horizontal window cycling spring preset.
    ///
    /// Tuned for fluid lateral gliding between adjacent task cards.
    /// - Stiffness: $k = 280.0$
    /// - Damping: $c = 27.0$
    /// - Mass: $m = 1.0$
    pub fn horizontal_switch() -> Self {
        Self::from_physics(280.0, 27.0, 1.0)
    }

    /// Fast dismiss to Home screen spring preset ($k = 340.0, c = 29.0$).
    pub fn home_dismiss() -> Self {
        Self::from_physics(340.0, 29.0, 1.0)
    }

    /// Material 3 Expressive preset ($k = 320.0, \zeta = 0.82$).
    pub fn expressive() -> Self {
        Self::new(320.0, 0.82, 1.0)
    }

    /// Critically damped preset ($k = 300.0, \zeta = 1.0$, no overshoot).
    pub fn critically_damped() -> Self {
        Self::new(300.0, 1.0, 1.0)
    }

    /// Bouncy preset for playful interactive elements ($k = 280.0, \zeta = 0.65$).
    pub fn bouncy() -> Self {
        Self::new(280.0, 0.65, 1.0)
    }

    /// Gentle glide preset ($k = 160.0, \zeta = 0.95$).
    pub fn gentle() -> Self {
        Self::new(160.0, 0.95, 1.0)
    }

    /// Sets the target position (builder pattern).
    pub fn with_target(mut self, target: f64) -> Self {
        self.target = target;
        self
    }

    /// Sets the current position (builder pattern).
    pub fn with_position(mut self, position: f64) -> Self {
        self.position = position;
        self
    }

    /// Sets the current velocity (builder pattern).
    pub fn with_velocity(mut self, velocity: f64) -> Self {
        self.velocity = velocity;
        self
    }

    /// Accessor for stiffness constant $k$.
    #[inline]
    pub fn stiffness(&self) -> f64 {
        self.oscillator.stiffness
    }

    /// Accessor for damping ratio $\zeta$.
    #[inline]
    pub fn damping_ratio(&self) -> f64 {
        self.oscillator.damping_ratio()
    }

    /// Accessor for mass $m$.
    #[inline]
    pub fn mass(&self) -> f64 {
        self.oscillator.mass
    }

    /// Sets target position.
    #[inline]
    pub fn set_target(&mut self, target: f64) {
        self.target = target;
    }

    /// Sets current position.
    #[inline]
    pub fn set_position(&mut self, position: f64) {
        self.position = position;
    }

    /// Sets current velocity.
    #[inline]
    pub fn set_velocity(&mut self, velocity: f64) {
        self.velocity = velocity;
    }

    /// Advances the spring by time delta `dt` (seconds) using the exact analytical closed-form solution.
    ///
    /// # Mathematical Formulation
    /// Depending on $\zeta$:
    /// - **Critically Damped ($\zeta = 1.0$):**
    ///   $$x(t) = e^{-\omega_0 t} (c_1 + c_2 t) + x_{\text{target}}$$
    /// - **Underdamped ($\zeta < 1.0$):**
    ///   $$x(t) = e^{-\zeta \omega_0 t} (c_1 \cos(\omega_d t) + c_2 \sin(\omega_d t)) + x_{\text{target}}$$
    /// - **Overdamped ($\zeta > 1.0$):**
    ///   $$x(t) = c_1 e^{r_1 t} + c_2 e^{r_2 t} + x_{\text{target}}$$
    ///
    /// This solution is unconditionally stable for any $dt > 0$.
    ///
    /// # Arguments
    /// * `dt` - Time step in seconds.
    ///
    /// # Returns
    /// New simulated position $x$.
    pub fn step(&mut self, dt: f64) -> f64 {
        self.step_analytical(dt)
    }

    /// Advances the spring using the exact analytical solution.
    pub fn step_analytical(&mut self, dt: f64) -> f64 {
        if dt <= 0.0 {
            return self.position;
        }

        let w0 = self.oscillator.natural_frequency();
        let zeta = self.oscillator.damping_ratio();
        let x0 = self.position - self.target;
        let v0 = self.velocity;

        let (new_x, new_v) = if (zeta - 1.0).abs() < 1e-5 {
            // Critically damped
            let decay = (-w0 * dt).exp();
            let c1 = x0;
            let c2 = v0 + w0 * x0;
            let x = decay * (c1 + c2 * dt);
            let v = decay * (v0 - w0 * c2 * dt);
            (x, v)
        } else if zeta < 1.0 {
            // Underdamped
            let wd = self.oscillator.damped_frequency();
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
            // Overdamped
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

    /// Advances the spring using Semi-Implicit (Symplectic) Euler integration.
    ///
    /// # Mathematical Formulation
    /// $$a = \frac{-k(x - x_{\text{target}}) - c v}{m}$$
    /// $$v_{t+1} = v_t + a \cdot \Delta t$$
    /// $$x_{t+1} = x_t + v_{t+1} \cdot \Delta t$$
    pub fn step_euler(&mut self, dt: f64) -> f64 {
        if dt <= 0.0 {
            return self.position;
        }
        let k = self.oscillator.stiffness;
        let c = self.oscillator.damping_coefficient;
        let m = self.oscillator.mass;

        let displacement = self.position - self.target;
        let accel = (-k * displacement - c * self.velocity) / m;
        self.velocity += accel * dt;
        self.position += self.velocity * dt;
        self.position
    }

    /// Advances the spring using 4th-order Runge-Kutta (RK4) integration.
    pub fn step_rk4(&mut self, dt: f64) -> f64 {
        if dt <= 0.0 {
            return self.position;
        }
        let k = self.oscillator.stiffness;
        let c = self.oscillator.damping_coefficient;
        let m = self.oscillator.mass;
        let target = self.target;

        let deriv = |x: f64, v: f64| -> (f64, f64) {
            let a = (-k * (x - target) - c * v) / m;
            (v, a)
        };

        let (dx1, dv1) = deriv(self.position, self.velocity);
        let (dx2, dv2) = deriv(self.position + dx1 * 0.5 * dt, self.velocity + dv1 * 0.5 * dt);
        let (dx3, dv3) = deriv(self.position + dx2 * 0.5 * dt, self.velocity + dv2 * 0.5 * dt);
        let (dx4, dv4) = deriv(self.position + dx3 * dt, self.velocity + dv3 * dt);

        self.position += (dt / 6.0) * (dx1 + 2.0 * dx2 + 2.0 * dx3 + dx4);
        self.velocity += (dt / 6.0) * (dv1 + 2.0 * dv2 + 2.0 * dv3 + dv4);
        self.position
    }

    /// Resets the spring to a starting position with zero velocity.
    pub fn reset(&mut self, pos: f64) {
        self.position = pos;
        self.velocity = 0.0;
    }

    /// Checks if the spring has settled within specified position and velocity thresholds.
    #[inline]
    pub fn is_settled(&self, pos_threshold: f64, vel_threshold: f64) -> bool {
        (self.position - self.target).abs() <= pos_threshold && self.velocity.abs() <= vel_threshold
    }

    /// Checks if the spring has settled using default overview gesture thresholds (pos: 0.005, vel: 0.05).
    #[inline]
    pub fn is_settled_default(&self) -> bool {
        self.is_settled(0.005, 0.05)
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
}
