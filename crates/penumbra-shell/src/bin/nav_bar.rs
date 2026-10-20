//! Standalone Penumbra Navigation Bar binary.
//!
//! A dedicated gesture navigation bar for Penumbra Mobile DE, built with Slint and
//! `layer-shell-slint` on Wayland `zwlr_layer_shell_v1` (`Layer::Top`, `exclusive_zone: 32`, `height: 32px`).
//!
//! Provides Android/iOS-style pill gestures:
//! - Tap or flick up: Return to Homescreen
//! - Swipe up & hold: Open Task Switcher / Overview
//! - Swipe left/right: Cycle running tasks

use std::time::Duration;

use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_shell::ipc::{read_windows, send_command, PhoneMode, ShellState};

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Initializing Penumbra Navigation Bar via layer-shell-slint...");

    // 1. Configure Wayland Layer Shell Platform backend
    let config = LayerConfig::default()
        .with_namespace("penumbra-nav-bar")
        .with_layer(Layer::Top)
        .with_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right)
        .with_size(412, 32)
        .with_exclusive_zone(32)
        .with_keyboard_interactivity(KeyboardInteractivity::None);

    layer_shell_slint::init(config)?;

    // 2. Instantiate Slint NavBarWindow component
    let bar = NavBarWindow::new()?;
    let ui_weak = bar.as_weak();

    // 3. Navigation Callbacks
    bar.on_nav_home({
        move || {
            info!("Navigation: Home requested");
            let mut state = ShellState::load();
            state.mode = PhoneMode::Home;
            state.drawer_open = false;
            let _ = state.save();
        }
    });

    bar.on_nav_recents({
        move || {
            info!("Navigation: Overview / Recents requested");
            let mut state = ShellState::load();
            state.mode = PhoneMode::Overview;
            state.drawer_open = false;
            let _ = state.save();
        }
    });

    bar.on_nav_next_app({
        move || {
            let windows = read_windows();
            if windows.len() > 1 {
                // Focus the second window in the MRU list
                let target = windows[1].id;
                info!("Navigation: Switching to next app ({})", target);
                let _ = send_command(&format!("focus {}", target));
                let mut state = ShellState::load();
                state.mode = PhoneMode::App;
                let _ = state.save();
            }
        }
    });

    bar.on_nav_prev_app({
        move || {
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

    // 4. State Poller: set solid background in App mode, transparent in Home mode
    let state_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        state_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(100),
            move || {
                let state = ShellState::load();
                if let Some(ui) = ui_weak.upgrade() {
                    let should_be_solid = state.mode == PhoneMode::App;
                    if ui.get_solid() != should_be_solid {
                        ui.set_solid(should_be_solid);
                    }
                }
            },
        );
    }

    info!("Penumbra Navigation Bar running event loop.");
    bar.run()?;

    Ok(())
}
