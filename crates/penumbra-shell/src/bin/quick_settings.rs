//! Standalone Penumbra Quick Settings & Notification Shade binary.
//!
//! A dedicated system control panel and notification center for Penumbra Mobile DE,
//! built with Slint and `layer-shell-slint` on Wayland `zwlr_layer_shell_v1`
//! (`Layer::Overlay`, `exclusive_zone: 0`, `size: 412x915`).

use std::time::Duration;

use chrono::Local;
use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_services::NetworkService;
use penumbra_shell::ipc::ShellState;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Initializing Penumbra Quick Settings via layer-shell-slint...");

    // 1. Configure Wayland Layer Shell Platform backend
    let config = LayerConfig::default()
        .with_namespace("penumbra-quick-settings")
        .with_layer(Layer::Overlay)
        .with_anchor(Anchor::Top | Anchor::Left | Anchor::Right)
        .with_size(412, 915)
        .with_exclusive_zone(0)
        .with_keyboard_interactivity(KeyboardInteractivity::None);

    layer_shell_slint::init(config)?;

    // 2. Instantiate Slint QuickSettingsWindow component
    let qs = QuickSettingsWindow::new()?;
    let ui_weak = qs.as_weak();

    // 3. Telemetry Clock (Time + Date)
    let clock_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        let update_time = move || {
            let now = Local::now();
            let time_str = now.format("%H:%M").to_string();
            let date_str = now.format("%A, %b %-d").to_string();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_time_text(time_str.into());
                ui.set_date_text(date_str.into());
            }
        };
        update_time();
        clock_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(1000),
            update_time,
        );
    }

    // 4. Network / Carrier Telemetry via Tokio
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let network_service = rt.block_on(async {
        NetworkService::new().await
    });

    {
        let net_info = network_service.current();
        qs.set_carrier_text(net_info.ssid.into());
        qs.set_tile_internet(net_info.is_connected);
    }

    // 5. User Interaction Callbacks
    qs.on_close_requested({
        move || {
            info!("Quick Settings: Close requested");
            let mut state = ShellState::load();
            state.drawer_open = false;
            let _ = state.save();
        }
    });

    qs.on_power_action({
        move || {
            info!("Quick Settings: Power button tapped");
        }
    });

    qs.on_settings_action({
        move || {
            info!("Quick Settings: Settings button tapped");
            let mut state = ShellState::load();
            state.drawer_open = false;
            let _ = state.save();
        }
    });

    qs.on_brightness_changed(|val| {
        info!("Quick Settings: Brightness set to {:.2}", val);
    });

    qs.on_volume_changed(|val| {
        info!("Quick Settings: Volume set to {:.2}", val);
    });

    info!("Penumbra Quick Settings running event loop.");
    qs.run()?;

    Ok(())
}
