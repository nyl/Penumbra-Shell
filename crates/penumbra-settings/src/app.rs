//! # Application Controller & Reactive UI Coordinator
//!
//! Synchronizes asynchronous D-Bus hardware backends with the declarative Slint UI.
//!
//! ## Threading & Asynchronous Architecture
//! * **UI Thread Separation:** Slint runs synchronously inside its own rendering and windowing
//!   event loop on the main thread.
//! * **Tokio Background Runtime:** All network queries, BlueZ ObjectManager scans, and UPower
//!   telemetry run asynchronously on a multi-threaded Tokio runtime.
//! * **Event Handover:** Data is bridged from Tokio worker threads into the Slint event loop
//!   using [`slint::invoke_from_event_loop`].
//! * **Lifecycle Management:** Navigation events trigger background hardware actions (e.g.,
//!   entering the Bluetooth subpage starts discovery scanning, leaving halts discovery).

use std::rc::Rc;
use std::sync::Arc;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio::runtime::Handle;
use tracing::info;

use penumbra_services::{
    apply_backlight_ratio, apply_volume_ratio, read_backlight_ratio, read_volume_ratio,
};

use crate::dbus::{BluetoothBackend, NetworkBackend, PowerBackend};
use crate::{BtDeviceData, PhysicalKeyboardData, SettingsWindow, VirtualKeyboardData, WifiApData};

/// Sets up reactive state bindings and user interaction callbacks for `SettingsWindow`.
///
/// # Arguments
/// * `window` - Reference to the instantiated Slint root window component.
/// * `net_backend` - Shared controller for NetworkManager wireless integration.
/// * `bt_backend` - Shared controller for BlueZ adapter and peripheral integration.
/// * `power_backend` - Shared controller for UPower battery telemetry.
/// * `rt_handle` - Handle to the Tokio runtime executing background tasks.
pub fn setup_app(
    window: &SettingsWindow,
    net_backend: Arc<NetworkBackend>,
    bt_backend: Arc<BluetoothBackend>,
    power_backend: Arc<PowerBackend>,
    rt_handle: Handle,
) {
    let ui_weak = window.as_weak();

    // 1. Initial State Population from real D-Bus backends
    {
        let net = net_backend.current();
        window.set_wifi_enabled(net.wifi_enabled);
        window.set_connected_ssid(net.connected_ssid.into());
        window.set_signal_strength(net.signal_strength as i32);
        let ap_models: Vec<WifiApData> = net
            .access_points
            .iter()
            .map(|ap| WifiApData {
                ssid: ap.ssid.clone().into(),
                signal: ap.signal_strength as i32,
                security: ap.security.clone().into(),
                connected: ap.is_connected,
            })
            .collect();
        window.set_wifi_access_points(ModelRc::from(Rc::new(VecModel::from(ap_models))));

        let bt = bt_backend.current();
        window.set_bt_enabled(bt.powered);
        window.set_adapter_name(bt.adapter_name.into());
        window.set_is_discovering(bt.discovering);
        let bt_models: Vec<BtDeviceData> = bt
            .devices
            .iter()
            .map(|dev| BtDeviceData {
                name: dev.name.clone().into(),
                address: dev.address.clone().into(),
                connected: dev.is_connected,
                paired: dev.is_paired,
                battery: dev.battery_level,
            })
            .collect();
        window.set_bt_devices(ModelRc::from(Rc::new(VecModel::from(bt_models))));

        let power = power_backend.current();
        window.set_battery_pct(power.percentage as i32);
        window.set_is_charging(power.is_charging);
        window.set_battery_state_text(power.state_text.into());
        window.set_battery_saver(power.power_saver);

        // Populate initial hardware brightness and media volume
        let init_b = read_backlight_ratio().unwrap_or(0.75);
        window.set_brightness(init_b as f32);
        let init_v = read_volume_ratio();
        window.set_media_volume(init_v as f32);

        // Pre-populate search catalog
        window.set_search_results(crate::search::filter_search(""));

        // Populate genuine hardware and OS telemetry
        let sys = crate::system_info::SystemInfo::read_current();
        window.set_device_name(sys.device_name.into());
        window.set_device_model(sys.device_model.into());
        window.set_os_version(sys.os_version.into());
        window.set_kernel_version(sys.kernel_version.into());
        window.set_cpu_model(sys.cpu_model.into());
        window.set_memory_info(sys.memory_info.into());
        window.set_storage_info(sys.storage_info.into());
        window.set_uptime_info(sys.uptime_info.into());
        window.set_compositor_info(sys.compositor_info.into());
        window.set_wifi_mac(sys.wifi_mac.into());
        window.set_bluetooth_mac(sys.bluetooth_mac.into());
        window.set_display_resolution(sys.display_resolution.into());
        window.set_battery_health(sys.battery_health.into());
        window.set_battery_model(sys.battery_model.into());

        // Populate virtual and physical keyboard telemetry
        reload_keyboards(window);
    }

    // Search Query & Virtual Keyboard IPC Callbacks
    {
        let weak = ui_weak.clone();
        window.on_search_query_changed(move |query| {
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
            let cmd_file = std::path::PathBuf::from(runtime_dir).join("penumbra-keyboard-cmd");
            let _ = std::fs::write(cmd_file, "show");
            if let Some(ui) = weak.upgrade() {
                ui.set_search_results(crate::search::filter_search(query.as_str()));
            }
        });
    }

    window.on_search_focused(|| {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        let cmd_file = std::path::PathBuf::from(runtime_dir).join("penumbra-keyboard-cmd");
        let _ = std::fs::write(cmd_file, "show");
        info!("Search focused in Settings -> showing on-screen virtual keyboard");
    });

    window.on_search_unfocused(|| {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        let cmd_file = std::path::PathBuf::from(runtime_dir).join("penumbra-keyboard-cmd");
        let _ = std::fs::write(cmd_file, "hide");
        info!("Search dismissed in Settings -> hiding on-screen virtual keyboard");
    });

    // 2. Reactive Network Subscription (dynamic scans from NetworkManager)
    {
        let mut rx = net_backend.subscribe();
        let weak = ui_weak.clone();
        rt_handle.spawn(async move {
            while rx.changed().await.is_ok() {
                let net = rx.borrow().clone();
                let w = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_wifi_enabled(net.wifi_enabled);
                        ui.set_connected_ssid(net.connected_ssid.into());
                        ui.set_signal_strength(net.signal_strength as i32);
                        let ap_models: Vec<WifiApData> = net
                            .access_points
                            .iter()
                            .map(|ap| WifiApData {
                                ssid: ap.ssid.clone().into(),
                                signal: ap.signal_strength as i32,
                                security: ap.security.clone().into(),
                                connected: ap.is_connected,
                            })
                            .collect();
                        ui.set_wifi_access_points(ModelRc::from(Rc::new(VecModel::from(ap_models))));
                    }
                });
            }
        });
    }

    // 3. Reactive Bluetooth Subscription (dynamic ObjectManager updates from BlueZ)
    {
        let mut rx = bt_backend.subscribe();
        let weak = ui_weak.clone();
        rt_handle.spawn(async move {
            while rx.changed().await.is_ok() {
                let bt = rx.borrow().clone();
                let w = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_bt_enabled(bt.powered);
                        ui.set_adapter_name(bt.adapter_name.into());
                        ui.set_is_discovering(bt.discovering);
                        let bt_models: Vec<BtDeviceData> = bt
                            .devices
                            .iter()
                            .map(|dev| BtDeviceData {
                                name: dev.name.clone().into(),
                                address: dev.address.clone().into(),
                                connected: dev.is_connected,
                                paired: dev.is_paired,
                                battery: dev.battery_level,
                            })
                            .collect();
                        ui.set_bt_devices(ModelRc::from(Rc::new(VecModel::from(bt_models))));
                    }
                });
            }
        });
    }

    // 4. Reactive Power Subscription (battery telemetry from UPower)
    {
        let mut rx = power_backend.subscribe();
        let weak = ui_weak.clone();
        rt_handle.spawn(async move {
            while rx.changed().await.is_ok() {
                let power = rx.borrow().clone();
                let w = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_battery_pct(power.percentage as i32);
                        ui.set_is_charging(power.is_charging);
                        ui.set_battery_state_text(power.state_text.into());
                        ui.set_battery_saver(power.power_saver);
                    }
                });
            }
        });
    }

    // 5. Connect User Action Callbacks to Background Tasks
    {
        let net = net_backend.clone();
        let rt = rt_handle.clone();
        window.on_toggle_wifi(move |val| {
            info!("Settings: Toggle Wi-Fi -> {val}");
            let net = net.clone();
            rt.spawn(async move {
                net.set_wifi_enabled(val).await;
            });
        });
    }

    {
        let net = net_backend.clone();
        let rt = rt_handle.clone();
        window.on_connect_ap(move |ssid| {
            info!("Settings: Connect to Wi-Fi SSID -> {ssid}");
            let net = net.clone();
            let ssid = ssid.to_string();
            rt.spawn(async move {
                net.connect_to_network(&ssid, None).await;
            });
        });
    }

    {
        let net = net_backend.clone();
        let rt = rt_handle.clone();
        window.on_refresh_wifi_scan(move || {
            info!("Settings: User requested Wi-Fi rescan");
            let net = net.clone();
            rt.spawn(async move {
                net.request_scan().await;
            });
        });
    }

    {
        let bt = bt_backend.clone();
        let rt = rt_handle.clone();
        window.on_toggle_bt(move |val| {
            info!("Settings: Toggle Bluetooth -> {val}");
            let bt = bt.clone();
            rt.spawn(async move {
                if !val {
                    let _ = bt.stop_discovery().await;
                }
                bt.set_powered(val).await;
            });
        });
    }

    {
        let bt = bt_backend.clone();
        let rt = rt_handle.clone();
        window.on_toggle_bt_device(move |addr| {
            info!("Settings: Toggle Bluetooth device -> {addr}");
            let bt = bt.clone();
            let addr = addr.to_string();
            rt.spawn(async move {
                bt.toggle_device(&addr).await;
            });
        });
    }

    {
        let bt = bt_backend.clone();
        let rt = rt_handle.clone();
        window.on_start_bt_discovery(move || {
            info!("Settings: User started Bluetooth discovery");
            let bt = bt.clone();
            rt.spawn(async move {
                bt.start_discovery().await;
            });
        });
    }

    {
        let power = power_backend.clone();
        let rt = rt_handle.clone();
        window.on_toggle_battery_saver(move |val| {
            info!("Settings: Toggle battery saver -> {val}");
            let power = power.clone();
            rt.spawn(async move {
                power.set_power_saver(val).await;
            });
        });
    }

    window.on_brightness_changed(|v| {
        info!("Settings: Brightness changed -> {:.2}", v);
        if let Err(error) = apply_backlight_ratio(v as f64) {
            tracing::warn!(?error, "Failed to apply system backlight in settings");
        }
    });

    window.on_media_volume_changed(|v| {
        info!("Settings: Media volume changed -> {:.2}", v);
        apply_volume_ratio(v as f64);
    });

    // Keyboard Action Callbacks
    window.on_toggle_hide_on_physical(|val| {
        info!("Settings: Hide keyboard on physical connection set to {val}");
    });

    {
        let weak = ui_weak.clone();
        window.on_select_virtual_keyboard(move |id| {
            info!("Settings: User selected virtual keyboard: {id}");
            crate::keyboard_backend::switch_active_virtual_keyboard(&id);
            if let Some(ui) = weak.upgrade() {
                let model = ui.get_virtual_keyboards();
                let mut list: Vec<VirtualKeyboardData> = model.iter().collect();
                for item in &mut list {
                    item.is_active = item.desktop_id == id;
                }
                ui.set_virtual_keyboards(ModelRc::from(Rc::new(VecModel::from(list))));
            }
        });
    }

    {
        let weak = ui_weak.clone();
        window.on_refresh_keyboard_scan(move || {
            info!("Settings: Refreshing keyboard hardware and virtual entries");
            if let Some(ui) = weak.upgrade() {
                reload_keyboards(&ui);
            }
        });
    }

    // Subpage routing hooks for discovery & scanning lifecycle
    {
        let bt = bt_backend.clone();
        let net = net_backend.clone();
        let rt = rt_handle.clone();
        let weak = ui_weak.clone();
        window.on_navigate_to(move |page| {
            info!("Settings: Navigated to subpage '{page}'");
            let bt = bt.clone();
            let net = net.clone();
            let page_str = page.to_string();
            let w = weak.clone();
            rt.spawn(async move {
                if page_str == "bluetooth" {
                    bt.start_discovery().await;
                } else if page_str == "wifi" {
                    net.request_scan().await;
                } else if page_str == "keyboard" {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = w.upgrade() {
                            reload_keyboards(&ui);
                        }
                    });
                } else {
                    bt.stop_discovery().await;
                }
            });
        });
    }

    {
        let bt = bt_backend.clone();
        let rt = rt_handle.clone();
        window.on_navigate_back(move || {
            info!("Settings: Navigated back to main catalog");
            let bt = bt.clone();
            rt.spawn(async move {
                bt.stop_discovery().await;
            });
        });
    }
}

/// Discovers installed KDE-compliant virtual keyboards and connected physical keyboards,
/// populating the Slint UI models.
fn reload_keyboards(window: &SettingsWindow) {
    let vks = crate::keyboard_backend::detect_virtual_keyboards();
    let vk_models: Vec<VirtualKeyboardData> = vks
        .into_iter()
        .map(|v| VirtualKeyboardData {
            name: v.name.into(),
            comment: v.comment.into(),
            desktop_id: v.desktop_id.into(),
            is_active: v.is_active,
        })
        .collect();
    window.set_virtual_keyboards(ModelRc::from(Rc::new(VecModel::from(vk_models))));

    let pks = crate::keyboard_backend::detect_physical_keyboards();
    let pk_models: Vec<PhysicalKeyboardData> = pks
        .into_iter()
        .map(|p| PhysicalKeyboardData {
            name: p.name.into(),
            bus: p.bus.into(),
        })
        .collect();
    window.set_physical_keyboards(ModelRc::from(Rc::new(VecModel::from(pk_models))));
}
