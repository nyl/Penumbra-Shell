// Standalone Penumbra Navigation Bar binary.
//
// A dedicated gesture navigation bar for Penumbra Mobile DE, built with Slint and
// `layer-shell-slint` on Wayland `zwlr_layer_shell_v1` (`Layer::Top`, `exclusive_zone: 32`, `height: 32px`).
//
// Provides Android/iOS-style pill gestures:
// - Tap or flick up: Return to Homescreen
// - Swipe up & hold: Open Task Switcher / Overview
// - Swipe left/right: Cycle running tasks

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_gestures::{NavGesture, NavGestureConfig, NavGestureRecognizer};
use penumbra_shell::ipc::{read_windows, send_command, PhoneMode, ShellState};

slint::include_modules!();

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
    // The surface geometry is controlled by the layer-shell config created above.
    let bar = NavBarWindow::new()?;
    let ui_weak = bar.as_weak();

    // 3. Navigation Gesture Recognizer
    let mut gesture_config = NavGestureConfig::default();
    gesture_config.overview_distance = 90.0; // Per spec: "Drag and Pause (y > 90px, v ≈ 0)"
    gesture_config.hold_duration = Duration::from_millis(150);
    gesture_config.flick_velocity = 700.0; // Per spec: "Quick Flick Up (v > 700px/s, displacement < 80px)"
    gesture_config.flick_min_distance = 15.0;
    gesture_config.switch_app_min_distance = 35.0;

    let nav_rec = Rc::new(RefCell::new(NavGestureRecognizer::new(gesture_config)));
    let nav_start = Rc::new(Cell::new(Instant::now()));
    let poll_timer = Rc::new(Timer::default());

    let handle_gesture = Rc::new(|gesture: NavGesture| match gesture {
        NavGesture::GoHome => {
            info!("Navigation: Home requested (Fast flick up or tap)");
            let mut state = ShellState::load();
            state.mode = PhoneMode::Home;
            state.drawer_open = false;
            let _ = state.save();
        }
        NavGesture::OpenOverview => {
            info!("Navigation: Overview / Recents requested (Slow pull up & hold)");
            let mut state = ShellState::load();
            state.mode = PhoneMode::Overview;
            state.drawer_open = false;
            state.overview_index = 0;
            let _ = state.save();
        }
        NavGesture::SwitchNextApp => {
            let windows = read_windows();
            if windows.len() > 1 {
                let target = windows[1].id;
                info!("Navigation: Switching to next app ({})", target);
                let _ = send_command(&format!("focus {}", target));
                let mut state = ShellState::load();
                state.mode = PhoneMode::App;
                let _ = state.save();
            }
        }
        NavGesture::SwitchPrevApp => {
            let windows = read_windows();
            if let Some(last) = windows.last() {
                info!("Navigation: Switching to previous app ({})", last.id);
                let _ = send_command(&format!("focus {}", last.id));
                let mut state = ShellState::load();
                state.mode = PhoneMode::App;
                let _ = state.save();
            }
        }
    });

    {
        let nav_rec = nav_rec.clone();
        let nav_start = nav_start.clone();
        let poll_timer = poll_timer.clone();
        let handle_gesture = handle_gesture.clone();

        bar.on_touch_down(move |x, y| {
            let now = Instant::now();
            nav_start.set(now);
            {
                let mut rec = nav_rec.borrow_mut();
                rec.on_down(x as f64, y as f64, Duration::ZERO);
            }

            let nav_rec = nav_rec.clone();
            let nav_start = nav_start.clone();
            let handle_gesture = handle_gesture.clone();
            poll_timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
                let elapsed = nav_start.get().elapsed();
                let gesture = {
                    let mut rec = nav_rec.borrow_mut();
                    rec.poll(elapsed)
                };
                if let Some(gesture) = gesture {
                    handle_gesture(gesture);
                }
            });
        });
    }

    {
        let nav_rec = nav_rec.clone();
        let nav_start = nav_start.clone();
        let handle_gesture = handle_gesture.clone();

        bar.on_touch_move(move |x, y| {
            let elapsed = nav_start.get().elapsed();
            let gesture = {
                let mut rec = nav_rec.borrow_mut();
                rec.on_move(x as f64, y as f64, elapsed)
            };
            if let Some(gesture) = gesture {
                handle_gesture(gesture);
            }
        });
    }

    {
        let nav_rec = nav_rec.clone();
        let nav_start = nav_start.clone();
        let poll_timer = poll_timer.clone();
        let handle_gesture = handle_gesture.clone();

        bar.on_touch_up(move |x, y| {
            poll_timer.stop();
            let elapsed = nav_start.get().elapsed();
            let (gesture, is_tap) = {
                let mut rec = nav_rec.borrow_mut();
                let g = rec.on_up(x as f64, y as f64, elapsed);
                let tap = g.is_none()
                    && rec.upward_distance() < 20.0
                    && rec.horizontal_distance().abs() < 25.0;
                (g, tap)
            };

            if let Some(gesture) = gesture {
                handle_gesture(gesture);
            } else if is_tap {
                // If it was a quick tap (< 20px drag), trigger Home
                handle_gesture(NavGesture::GoHome);
            }
        });
    }

    // Direct callbacks as fallback
    {
        let handle_gesture = handle_gesture.clone();
        bar.on_nav_home(move || {
            handle_gesture(NavGesture::GoHome);
        });
    }

    {
        let handle_gesture = handle_gesture.clone();
        bar.on_nav_recents(move || {
            handle_gesture(NavGesture::OpenOverview);
        });
    }

    {
        let handle_gesture = handle_gesture.clone();
        bar.on_nav_next_app(move || {
            handle_gesture(NavGesture::SwitchNextApp);
        });
    }

    {
        let handle_gesture = handle_gesture.clone();
        bar.on_nav_prev_app(move || {
            handle_gesture(NavGesture::SwitchPrevApp);
        });
    }

    // 4. State Poller: set solid background in App mode, transparent in Home mode
    let state_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        state_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
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
