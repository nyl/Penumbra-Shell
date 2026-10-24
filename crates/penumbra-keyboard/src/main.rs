//! # Penumbra On-Screen Virtual Keyboard (`penumbra-keyboard`)
//!
//! A standalone Wayland layer-shell on-screen virtual keyboard for Penumbra Mobile DE,
//! designed in Rust and Slint according to Android 17 / Pixel Material You specifications.
//!
//! ## Architectural Overview
//! 1. **Wayland Layer-Shell Placement:**
//!    - Renders as a dedicated overlay surface anchored to `Bottom | Left | Right`.
//!    - Uses `KeyboardInteractivity::None` so that tapping keys on the virtual keyboard
//!      does NOT steal input focus from the active client (e.g. terminal, browser, search bar).
//! 2. **Hardware Kernel Integration (`/dev/uinput`):**
//!    - Generates genuine Linux kernel key events (`EV_KEY`, `EV_SYN`) via `/dev/uinput`.
//!    - Emits standard scancodes with hardware shift and repeat semantics.
//! 3. **Physical Keyboard Coexistence:**
//!    - Actively detects connected physical keyboards from `/proc/bus/input/devices`.
//!    - Automatically hides and suspends the on-screen keyboard when a physical typing
//!      keyboard is detected.
//! 4. **CLI and IPC Remote Control:**
//!    - Running `penumbra-keyboard --toggle`, `--show`, or `--hide` sends commands to
//!      the active session daemon via `$XDG_RUNTIME_DIR/penumbra-keyboard-cmd`.

pub mod ipc;
pub mod physical_detector;
pub mod uinput;

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use physical_detector::detect_active_physical_keyboard;
use uinput::{UInputKeyboard, KEY_BACKSPACE, KEY_ENTER, KEY_SPACE};

slint::include_modules!();

fn print_usage() {
    println!(
        "Usage: penumbra-keyboard [OPTIONS]\n\n\
        Options:\n  \
          --show        Show the on-screen keyboard\n  \
          --hide        Hide the on-screen keyboard\n  \
          --toggle      Toggle visibility of the on-screen keyboard\n  \
          -h, --help    Display this help message\n"
    );
}

fn handle_cli_args() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if args.len() <= 1 {
        return false;
    }

    match args[1].as_str() {
        "--show" | "-s" => {
            let path = ipc::runtime_path(ipc::KEYBOARD_CMD_FILE);
            let _ = std::fs::write(path, "show");
            info!("Sent 'show' command to running penumbra-keyboard daemon");
            true
        }
        "--hide" | "-c" => {
            let path = ipc::runtime_path(ipc::KEYBOARD_CMD_FILE);
            let _ = std::fs::write(path, "hide");
            info!("Sent 'hide' command to running penumbra-keyboard daemon");
            true
        }
        "--toggle" | "-t" => {
            let path = ipc::runtime_path(ipc::KEYBOARD_CMD_FILE);
            let _ = std::fs::write(path, "toggle");
            info!("Sent 'toggle' command to running penumbra-keyboard daemon");
            true
        }
        "-h" | "--help" => {
            print_usage();
            true
        }
        _ => false,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    // If an external control flag is passed, dispatch and exit immediately
    if handle_cli_args() {
        return Ok(());
    }

    info!("Initializing Penumbra Keyboard via layer-shell-slint...");

    // 1. Configure Wayland Layer Shell Platform backend.
    // Anchored to screen bottom, Overlay layer, 280px height.
    let (logical_w, _logical_h, _scale) = layer_shell_slint::mobile_output_geometry();
    let config = LayerConfig::default()
        .with_namespace("penumbra-keyboard")
        .with_layer(Layer::Top)
        .with_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right)
        .with_size(logical_w, 280)
        .with_exclusive_zone(0)
        .with_keyboard_interactivity(KeyboardInteractivity::None);

    layer_shell_slint::init(config)?;

    // 2. Instantiate Slint KeyboardWindow UI
    let keyboard_window = KeyboardWindow::new()?;
    let ui_weak = keyboard_window.as_weak();

    // 3. Initialize Linux /dev/uinput Driver
    let uinput = Arc::new(UInputKeyboard::new());

    // 4. Register Event Callbacks
    {
        let uinput = Arc::clone(&uinput);
        keyboard_window.on_key_clicked(move |text| {
            uinput.send_text(text.as_str());
        });
    }

    {
        let uinput = Arc::clone(&uinput);
        keyboard_window.on_backspace_clicked(move || {
            uinput.click_key(KEY_BACKSPACE);
        });
    }

    {
        let uinput = Arc::clone(&uinput);
        keyboard_window.on_space_clicked(move || {
            uinput.click_key(KEY_SPACE);
        });
    }

    {
        let uinput = Arc::clone(&uinput);
        keyboard_window.on_enter_clicked(move || {
            uinput.click_key(KEY_ENTER);
        });
    }

    {
        let ui_weak = ui_weak.clone();
        keyboard_window.on_toggle_hide(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let eff_open = ui.get_is_open() && !ui.get_physical_keyboard_connected();
                ipc::write_keyboard_state(eff_open);
            }
            info!("On-screen keyboard visibility toggled by user");
        });
    }

    // 5. Initial Physical Keyboard Detection Check
    let initial_physical = detect_active_physical_keyboard();
    let initial_is_open = initial_physical.is_none();
    if let Some(ref name) = initial_physical {
        info!(
            "Physical keyboard detected on startup ('{}'); hiding on-screen keyboard",
            name
        );
        keyboard_window.set_physical_keyboard_connected(true);
        keyboard_window.set_physical_keyboard_name(name.clone().into());
        keyboard_window.set_is_open(false);
    } else {
        keyboard_window.set_physical_keyboard_connected(false);
        keyboard_window.set_physical_keyboard_name("".into());
        keyboard_window.set_is_open(true);
    }
    ipc::write_keyboard_state(initial_is_open);

    // 6. Background Telemetry and Command Polling Timer (every 200ms)
    let poll_timer = Timer::default();
    let last_known_physical = Rc::new(std::cell::RefCell::new(initial_physical));

    {
        let ui_weak = ui_weak.clone();
        let last_known = Rc::clone(&last_known_physical);

        poll_timer.start(TimerMode::Repeated, Duration::from_millis(200), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };

            // 6a. Auto-dismiss if Quick Settings drawer or power dialog is open
            if ipc::is_drawer_or_dialog_open() {
                if ui.get_is_open() {
                    ui.set_is_open(false);
                    ipc::write_keyboard_state(false);
                }
            }

            // 6b. Check for physical keyboard connection changes
            let current_physical = detect_active_physical_keyboard();
            let mut last = last_known.borrow_mut();

            if current_physical != *last {
                if let Some(ref name) = current_physical {
                    info!(
                        "Physical keyboard plugged in / detected ('{}') -> auto-hiding on-screen keyboard",
                        name
                    );
                    ui.set_physical_keyboard_connected(true);
                    ui.set_physical_keyboard_name(name.clone().into());
                    ui.set_is_open(false);
                } else {
                    info!("Physical keyboard disconnected -> enabling on-screen keyboard");
                    ui.set_physical_keyboard_connected(false);
                    ui.set_physical_keyboard_name("".into());
                    ui.set_is_open(true);
                }
                *last = current_physical;
            }

            // 6c. Check for external IPC commands
            if let Some(cmd) = ipc::poll_pending_command() {
                match cmd {
                    ipc::KeyboardCommand::Show => {
                        if !ui.get_physical_keyboard_connected() {
                            ui.set_is_open(true);
                        } else {
                            info!("Ignoring show command: physical keyboard is connected");
                        }
                    }
                    ipc::KeyboardCommand::Hide => {
                        ui.set_is_open(false);
                    }
                    ipc::KeyboardCommand::Toggle => {
                        if !ui.get_physical_keyboard_connected() {
                            let curr = ui.get_is_open();
                            ui.set_is_open(!curr);
                        } else {
                            info!("Ignoring toggle command: physical keyboard is connected");
                        }
                    }
                }
            }

            let eff_open = ui.get_is_open() && !ui.get_physical_keyboard_connected();
            ipc::write_keyboard_state(eff_open);
        });
    }

    // 7. Run Slint Event Loop
    keyboard_window.run()?;

    info!("Penumbra Keyboard exited cleanly");
    Ok(())
}
