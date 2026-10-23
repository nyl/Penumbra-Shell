//! # Penumbra Settings Binary
//!
//! A standalone Material 3 Expressive settings application for the Penumbra Mobile Desktop Environment.
//!
//! ## Architecture Overview
//! * **UI Framework:** Slint declarative UI compiled via `slint_build` and executed on the main thread
//!   via `i-slint-backend-winit`.
//! * **Single-Instance Enforcement:** Claims the session D-Bus bus name `org.penumbra.Settings`.
//!   If an instance is already running, secondary launches route the target page via D-Bus IPC
//!   and terminate immediately without spawning redundant windows.
//! * **Hardware Daemons:** Communicates with standard Linux system services (NetworkManager, BlueZ,
//!   UPower) via asynchronous `zbus` proxies.
//!
//! ## Command-Line Usage
//! ```bash
//! # Launch root settings directory
//! penumbra-settings
//!
//! # Direct deep-link navigation to the Wi-Fi management subpage
//! penumbra-settings --page wifi
//!
//! # Direct deep-link navigation to Bluetooth accessories
//! penumbra-settings --page bluetooth
//! ```

mod app;
mod dbus;
mod routes;
mod search;
mod system_info;

use std::str::FromStr;
use std::sync::Arc;
use clap::Parser;
use slint::ComponentHandle;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use crate::dbus::{BluetoothBackend, NetworkBackend, PowerBackend};
use crate::routes::Page;

slint::include_modules!();

/// Command-line arguments for the `penumbra-settings` application.
#[derive(Parser, Debug)]
#[command(name = "penumbra-settings", about = "Penumbra Mobile Settings")]
struct Args {
    /// Target subpage to open directly upon launch.
    ///
    /// Available options: `"main"`, `"wifi"`, `"bluetooth"`, `"display"`, `"battery"`, `"sound"`, `"about"`.
    /// Also accepts common aliases such as `"internet"`, `"bt"`, `"screen"`, `"power"`.
    #[arg(short, long, default_value = "main")]
    page: String,
}

/// Application entry point.
///
/// # Errors
/// Returns an error if Tokio runtime construction fails or the Slint event loop terminates abnormally.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let args = Args::parse();
    let initial_page = Page::from_str(&args.page).unwrap_or(Page::Main);
    info!("Starting penumbra-settings with target page: {initial_page}");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // 1. Single-Instance Check via D-Bus:
    // If an instance is already running, route the requested page to it and exit cleanly.
    let already_running = rt.block_on(async {
        dbus::try_activate_existing(initial_page.as_str()).await
    });

    if already_running {
        info!("Successfully routed page to running Settings instance. Exiting secondary process.");
        return Ok(());
    }

    // 2. Primary Instance Setup:
    // Claim org.penumbra.Settings on the user session bus and listen for incoming runtime activations.
    let (page_tx, mut page_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let _dbus_conn = rt.block_on(async { dbus::start_service(page_tx).await });

    // 3. Initialize Hardware Backends
    let (net_backend, bt_backend, power_backend) = rt.block_on(async {
        (
            Arc::new(NetworkBackend::new().await),
            Arc::new(BluetoothBackend::new().await),
            Arc::new(PowerBackend::new().await),
        )
    });

    // 4. Instantiate Slint SettingsWindow
    let window = SettingsWindow::new()?;
    let ui_weak = window.as_weak();

    // Set initial page from CLI argument
    window.set_active_page(initial_page.as_str().into());

    // Connect App Controller & Hardware Reactivity
    app::setup_app(
        &window,
        net_backend,
        bt_backend,
        power_backend,
        rt.handle().clone(),
    );

    // 5. Handle runtime D-Bus activation requests from subsequent CLI launches or quick settings
    let weak = ui_weak.clone();
    rt.spawn(async move {
        while let Some(requested_page_str) = page_rx.recv().await {
            info!("Received runtime activation to navigate to '{requested_page_str}'");
            let page = Page::from_str(&requested_page_str).unwrap_or(Page::Main);
            let w = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = w.upgrade() {
                    ui.set_active_page(page.as_str().into());
                    let _ = ui.window().show();
                }
            });
        }
    });

    info!("Entering Penumbra Settings UI event loop");
    window.run()?;

    Ok(())
}
