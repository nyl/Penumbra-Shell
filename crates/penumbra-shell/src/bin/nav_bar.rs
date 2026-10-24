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

use std::time::Duration;

use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
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
    let bar = NavBarWindow::new()?;
    let ui_weak = bar.as_weak();

    // Wire keyboard toggle button on navigation bar
    bar.on_keyboard_toggle_clicked(|| {
        let path = penumbra_shell::ipc::runtime_path("penumbra-keyboard-cmd");
        let _ = std::fs::write(path, "toggle");
        info!("Toggled virtual keyboard via navigation bar button");
    });

    // 3. Navigation gesture tracking is handled natively by penumbra-comp with 120Hz libinput physics.
    bar.on_touch_down(|_, _| {});
    bar.on_touch_move(|_, _| {});
    bar.on_touch_up(|_, _| {});

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
