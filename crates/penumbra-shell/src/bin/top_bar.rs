//! Standalone Penumbra Top Status Bar binary.
//!
//! A dedicated system status bar for Penumbra Mobile DE, built with Slint and
//! `layer-shell-slint` on Wayland `zwlr_layer_shell_v1` (`Layer::Top`, `exclusive_zone: 38`, `height: 38px`).

use std::time::Duration;

use chrono::Local;
use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_services::{BatteryService, NetworkService};

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Initializing Penumbra Top Bar via layer-shell-slint...");

    // 1. Configure and initialize Wayland Layer Shell Platform backend
    let config = LayerConfig::default()
        .with_namespace("penumbra-top-bar")
        .with_layer(Layer::Top)
        .with_anchor(Anchor::Top | Anchor::Left | Anchor::Right)
        .with_size(0, 38)
        .with_exclusive_zone(38)
        .with_keyboard_interactivity(KeyboardInteractivity::None);

    layer_shell_slint::init(config)?;

    // 2. Instantiate Slint TopBar UI
    let bar = TopBarWindow::new()?;
    let ui_weak = bar.as_weak();

    bar.on_top_bar_clicked({
        let ui_weak = ui_weak.clone();
        move || {
            info!("Top Bar clicked / touched");
            if let Some(_ui) = ui_weak.upgrade() {
                // In future steps, this will trigger the Quick Settings shade
            }
        }
    });

    // 3. Telemetry Clock Timer (updates every second)
    let clock_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        let update_time = move || {
            let now = Local::now();
            let time_str = now.format("%H:%M").to_string();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_time_text(time_str.into());
            }
        };
        // Initial immediate push
        update_time();
        clock_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(1000),
            update_time,
        );
    }

    // 4. Initialize Hardware Services (Battery, Network) via Tokio
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let (battery_service, network_service) = rt.block_on(async {
        let bat = BatteryService::new().await;
        let net = NetworkService::new().await;
        (bat, net)
    });

    // Initial Telemetry push
    {
        let bat_info = battery_service.current();
        let net_info = network_service.current();
        bar.set_battery_pct(bat_info.percentage as f32);
        bar.set_is_charging(bat_info.is_charging);
        bar.set_wifi_connected(net_info.is_connected);
        bar.set_wifi_signal(net_info.signal_strength as i32);
        bar.set_carrier_text(net_info.ssid.into());
    }

    // 5. Spawn background watcher thread for D-Bus / mock service updates
    {
        let ui_weak = ui_weak.clone();
        let mut bat_rx = battery_service.subscribe();
        let mut net_rx = network_service.subscribe();
        let _battery_service = battery_service;
        let _network_service = network_service;

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
                let _bat = _battery_service;
                let _net = _network_service;

                loop {
                    tokio::select! {
                        res = bat_rx.changed() => {
                            if res.is_ok() {
                                let info = bat_rx.borrow().clone();
                                let ui_weak = ui_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_weak.upgrade() {
                                        ui.set_battery_pct(info.percentage as f32);
                                        ui.set_is_charging(info.is_charging);
                                    }
                                });
                            } else {
                                break;
                            }
                        }
                        res = net_rx.changed() => {
                            if res.is_ok() {
                                let info = net_rx.borrow().clone();
                                let ui_weak = ui_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_weak.upgrade() {
                                        ui.set_wifi_connected(info.is_connected);
                                        ui.set_wifi_signal(info.signal_strength as i32);
                                        ui.set_carrier_text(info.ssid.into());
                                    }
                                });
                            } else {
                                break;
                            }
                        }
                    }
                }
            });
        });
    }

    info!("Penumbra Top Bar running event loop.");
    // 6. Run the layer-shell Slint event loop
    bar.run()?;

    Ok(())
}
