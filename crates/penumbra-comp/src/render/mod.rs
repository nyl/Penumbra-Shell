//! Offscreen rendering passes and texture processing pipelines for Penumbra Compositor.
//!
//! # Architecture & Offscreen Buffer Lifecycle
//!
//! To achieve a fluid 60/120 FPS Android 17 progressive blur backdrop without GPU pipeline stalls:
//!
//! 1. **Scene Capture (`scene_fbo`):**
//!    When a QuickSettings pull-down gesture is active (`progress > 0.0`), the compositor captures
//!    the underlying desktop background layers (wallpaper, homescreen) and running application
//!    windows into an offscreen framebuffer texture (`scene_fbo`) matching the phone's logical resolution
//!    ($412 \times 915$).
//!
//! 2. **Downsample & Dual-Kawase Blur Ping-Ponging (`ping_fbo` & `blurred_fbo`):**
//!    Rather than attempting a multi-pass Gaussian blur at native resolution—which would incur severe
//!    fillrate penalties and GPU pipeline bubbles—the pipeline executes a 2-pass Dual-Kawase blur
//!    at a 1/4 downscaled resolution ($103 \times 228$).
//!    - **Pass 1 (Downsample + Diagonal Dispersion):** Reads `scene_fbo` at full resolution, applies
//!      a 4-tap diagonal Kawase kernel with offset $d = 1.5$, and renders into `ping_fbo`. Hardware
//!      bilinear filtering (`GL_LINEAR`) provides initial spatial anti-aliasing.
//!    - **Pass 2 (Diffuse Ping-Pong):** Reads `ping_fbo`, applies an expanded 4-tap diagonal Kawase
//!      kernel with offset $d = 2.0$, and renders into `blurred_fbo`. High-frequency window boundaries
//!      and text dissolve completely into an organic, smooth color field.
//!
//! 3. **Progressive Cross-Fade Pass (`quicksettings_backdrop.frag`):**
//!    The final compositing pass samples both `scene_fbo` (sharp) and `blurred_fbo` (diffuse).
//!    The gesture progress $u\_progress \in [0.0, 1.0]$ interpolates the two textures using Hermite
//!    interpolation (`smoothstep(0.05, 0.95, u_progress)`), while simultaneously applying a dynamic
//!    Material 3 dark scrim tint (`#0b0a0e` with alpha scaling up to $0.40 \cdot u\_progress$).
//!    The resulting backdrop is composited directly onto the primary screen before rendering the
//!    transparent QuickSettings drawer chrome.
//!
//! # Memory & Buffer Lifecycle Management
//!
//! Textures and framebuffers are lazily allocated on the first gesture contact and retained in
//! GPU memory across subsequent frames. When the screen dimensions change or the pipeline is
//! dropped, all OpenGL textures, framebuffers, and compiled shader programs are cleanly destroyed.

pub mod quicksettings;

pub use quicksettings::QuickSettingsBackdropPipeline;
