//! Standalone Penumbra Navigation Bar binary.
//!
//! Provides Android 17 (Pixel) navigation gesture dynamics for Penumbra Mobile DE,
//! built with Slint and `layer-shell-slint` on Wayland `zwlr_layer_shell_v1`
//! (`Layer::Top`, `exclusive_zone: 32`, `height: 32px`).
//!
//! # Architecture & Pipeline
//! - Eliminates all artificial hold timers. Touch tracking is continuous from the very first pixel.
//! - Uses [`penumbra_gestures::VelocityTracker`] with Exponential Moving Average (EMA, $\alpha = 0.8$)
//!   for smooth, real-time velocity calculation.
//! - Bidirectional drag: swiping upward drives progress $t \in [0.0, 1.05]$ into Overview,
//!   and dragging back down reverses $t \to 0.0$ smoothly.
//! - On touch release, dispatches velocity to [`penumbra_gestures::arbitrate_release`] and
//!   transitions window state into `Overview` or returns to `Fullscreen`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_gestures::{compute_normalized_progress, VelocityTracker};
use penumbra_shell::ipc::{read_windows, send_command, PhoneMode, ShellState};

slint::include_modules!();

/// Coordinates and timestamps for active navigation touch interactions.
#[derive(Debug, Clone, Copy)]
struct TouchSession {
    start_x: f64,
    start_y: f64,
    start_time: Instant,
}

pub(crate) fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Initializing Penumbra Navigation Bar via layer-shell-slint...");

    // 1. Configure Wayland Layer Shell Platform backend.
    let (logical_w, _logical_h, _scale) = layer_shell_slint::mobile_output_geometry();
    let config = LayerConfig::default()
        .with_namespace("penumbra-nav-bar")
        .with_layer(Layer::Top)
        .with_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right)
        .with_size(logical_w, 32)
        .with_exclusive_zone(32)
        .with_keyboard_interactivity(KeyboardInteractivity::None);

    layer_shell_slint::init(config)?;

    // 2. Instantiate Slint NavBarWindow component.
    let bar = NavBarWindow::new()?;
    let ui_weak = bar.as_weak();

    // Wire keyboard toggle button on navigation bar
    bar.on_keyboard_toggle_clicked(|| {
        let path = penumbra_shell::ipc::runtime_path("penumbra-keyboard-cmd");
        let _ = std::fs::write(path, "toggle");
        info!("Toggled virtual keyboard via navigation bar button");
    });

    // 3. Continuous Navigation Gesture Tracker (Zero hold timers, EMA alpha = 0.8)
    let tracker = Rc::new(RefCell::new(VelocityTracker::new(0.8)));
    let active_touch = Rc::new(RefCell::new(Option::<TouchSession>::None));

    {
        let tracker = Rc::clone(&tracker);
        let active_touch = Rc::clone(&active_touch);

        bar.on_touch_down(move |x, y| {
            let now = Instant::now();
            *active_touch.borrow_mut() = Some(TouchSession {
                start_x: x as f64,
                start_y: y as f64,
                start_time: now,
            });

            let mut trk = tracker.borrow_mut();
            trk.reset();
            trk.start(x as f64, y as f64, Duration::ZERO);

            let mut state = ShellState::load();
            state.nav_gesture_active = true;
            state.nav_gesture_dy = 0.0;
            state.nav_gesture_progress = 0.0;
            state.nav_gesture_vy = 0.0;
            let _ = state.save();
        });
    }

    {
        let tracker = Rc::clone(&tracker);
        let active_touch = Rc::clone(&active_touch);

        bar.on_touch_move(move |x, y| {
            let touch = *active_touch.borrow();
            let Some(session) = touch else {
                return;
            };

            let elapsed = session.start_time.elapsed();
            let mut trk = tracker.borrow_mut();
            let (vx, vy) = trk.update(x as f64, y as f64, elapsed);

            let delta_x = x as f64 - session.start_x;
            let delta_y = y as f64 - session.start_y;

            let mut state = ShellState::load();
            state.nav_gesture_active = true;

            // Check if gesture is horizontally dominant (Android Quick Switch, 24px deadzone)
            if delta_x.abs() > 24.0 && delta_x.abs() > delta_y.abs() * 1.2 {
                state.nav_switch_active = true;
                state.nav_gesture_dx = delta_x;
                state.nav_gesture_vx = vx;
                state.nav_gesture_dy = 0.0;
                state.nav_gesture_progress = 0.0;
                state.nav_gesture_vy = 0.0;
            } else {
                // Vertically dominant (Overview / Home swipe)
                let progress = compute_normalized_progress(delta_y, 915.0);
                state.nav_switch_active = false;
                state.nav_gesture_dx = 0.0;
                state.nav_gesture_vx = 0.0;
                state.nav_gesture_dy = delta_y;
                state.nav_gesture_progress = progress;
                state.nav_gesture_vy = vy;
            }
            let _ = state.save();
        });
    }

    {
        let tracker = Rc::clone(&tracker);
        let active_touch = Rc::clone(&active_touch);

        bar.on_touch_up(move |x, y| {
            let touch = active_touch.borrow_mut().take();
            let Some(session) = touch else {
                return;
            };

            let elapsed = session.start_time.elapsed();
            let mut trk = tracker.borrow_mut();
            let (vx, vy) = trk.update(x as f64, y as f64, elapsed);

            let delta_x = x as f64 - session.start_x;
            let delta_y = y as f64 - session.start_y;

            // 1. Quick tap (< 15px travel, < 250ms duration) -> Go Home
            if delta_x.abs() < 15.0 && delta_y.abs() < 15.0 && elapsed < Duration::from_millis(250) {
                info!("Navigation: Quick tap on nav pill -> Return Home");
                let mut state = ShellState::load();
                state.mode = PhoneMode::Home;
                state.drawer_open = false;
                state.nav_gesture_active = false;
                state.nav_gesture_progress = 0.0;
                state.nav_switch_active = false;
                state.nav_gesture_dx = 0.0;
                let _ = state.save();
                return;
            }

            // 2. Horizontal Quick Switch Arbitration
            if delta_x.abs() > 15.0 && delta_x.abs() > delta_y.abs() * 1.1 {
                info!(
                    "Navigation: Horizontal Quick Switch release dx={:.1}, vx={:.1}px/s",
                    delta_x, vx
                );
                let mut state = ShellState::load();
                state.nav_gesture_active = false;
                state.nav_switch_active = true;
                state.nav_gesture_dx = delta_x;
                state.nav_gesture_vx = vx;
                let _ = state.save();
                return;
            }

            // 3. Vertical Navigation Swipe Arbitration
            let progress = compute_normalized_progress(delta_y, 915.0);
            info!(
                "Navigation: Vertical release progress={:.2}, vy={:.1}px/s",
                progress, vy
            );

            let mut state = ShellState::load();
            state.nav_gesture_active = false;
            state.nav_switch_active = false;
            state.nav_gesture_progress = progress;
            state.nav_gesture_dy = delta_y;
            state.nav_gesture_vy = vy;
            let _ = state.save();
        });
    }

    // Direct callbacks
    bar.on_nav_home(|| {
        info!("Navigation: Home button invoked");
        let mut state = ShellState::load();
        state.mode = PhoneMode::Home;
        state.drawer_open = false;
        let _ = state.save();
    });

    bar.on_nav_recents(|| {
        info!("Navigation: Recents button invoked");
        let mut state = ShellState::load();
        state.mode = PhoneMode::Overview;
        state.drawer_open = false;
        state.overview_index = 0;
        let _ = state.save();
    });

    bar.on_nav_next_app(|| {
        let windows = read_windows();
        if windows.len() > 1 {
            let target = windows[1].id;
            let _ = send_command(&format!("focus {}", target));
            let mut state = ShellState::load();
            state.mode = PhoneMode::App;
            let _ = state.save();
        }
    });

    bar.on_nav_prev_app(|| {
        let windows = read_windows();
        if let Some(last) = windows.last() {
            let _ = send_command(&format!("focus {}", last.id));
            let mut state = ShellState::load();
            state.mode = PhoneMode::App;
            let _ = state.save();
        }
    });

    // 4. Solid container background sync
    let state_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        state_timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
            let state = ShellState::load();
            if let Some(ui) = ui_weak.upgrade() {
                let should_be_solid = state.mode == PhoneMode::App;
                if ui.get_solid() != should_be_solid {
                    ui.set_solid(should_be_solid);
                }
            }
        });
    }

    info!("Penumbra Navigation Bar running event loop.");
    bar.run()?;

    Ok(())
}
