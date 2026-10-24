# Penumbra Gestures

Mathematical physics engine and touch gesture arbitration library for the Penumbra Mobile Desktop Environment.

## Architectural Decision: Pure Internal Library (`[lib]`)

### Problem Analysis
In previous iterations, gesture handling was prone to frame-pacing latency and jitter due to artificial hold timers and redundant IPC mediation hops between the bottom navigation bar and the compositor. When gesture events or touch streams are proxied through external brokers or asynchronous daemon channels, round-trip serialization and queueing introduce multiple frame drops (16ms to 33ms+).

### Selected Architecture: In-Process Zero-Copy Physics Library
`penumbra-gestures` is implemented strictly as a zero-dependency internal Rust library crate (`[lib]`), imported directly by:
1. **`penumbra-comp`** (Compositor): Executes real-time touch arbitration, normalized scale/corner-radius transforms, and second-order spring settling during frame rendering at display vblank frequency.
2. **`penumbra-shell` / `penumbra-nav-bar`** (Shell Navigation Bar): Tracks user touch interactions with 1:1 instantaneous response, filtering instantaneous velocity via Exponential Moving Average (EMA, $\alpha = 0.8$) on the very first pixel of movement.

### Core Mathematical Modules
- **`SpringSolver` & `DampedHarmonicOscillator`** (`spring.rs`): Exact analytical solution and numerical Euler/RK4 integration for the damped harmonic oscillator:
  $$m \ddot{x}(t) + c \dot{x}(t) + k (x(t) - x_{\text{target}}) = 0$$
  Includes Android 17 (Pixel) Overview navigation tuning: $k \approx 340.0$, $c \approx 29.0$, $m = 1.0$ (damping ratio $\zeta \approx 0.786$).
- **`VelocityTracker`** (`velocity.rs`): Real-time velocity estimator using Exponential Moving Average (EMA, $\alpha = 0.8$) to suppress touch noise while maintaining sub-millisecond reaction times.
- **`GestureState` & Arbitration** (`gesture_state.rs`): Normalized progress calculation ($t \in [0.0, 1.05]$) and threshold arbitration between `Fullscreen` and `Overview` based on instantaneous release velocity and gesture displacement.
