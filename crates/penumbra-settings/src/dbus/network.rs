//! # NetworkManager D-Bus Integration
//!
//! Provides an asynchronous hardware client to monitor, scan, and configure system network
//! interfaces using the standard `org.freedesktop.NetworkManager` D-Bus API via `zbus`.
//!
//! ## System Contracts & Architecture
//! * **Wireless Device Discovery:** Locates Wi-Fi network interfaces via `org.freedesktop.NetworkManager.GetDevices()`
//!   matching device type `2` (`NM_DEVICE_TYPE_WIFI`).
//! * **Scan & AP Enumeration:** Invokes `RequestScan()` on `org.freedesktop.NetworkManager.Device.Wireless`
//!   and enumerates child `org.freedesktop.NetworkManager.AccessPoint` objects.
//! * **Security Classification:** Parses 802.11 RSN/WPA/Privacy flags into human-readable protocols
//!   (`WPA3`, `WPA2`, `WPA`, `WEP`, `Open`).
//! * **Reactive UI Dispatch:** Emits updates through a `tokio::sync::watch` channel consumed by the Slint UI thread.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tracing::{info, warn};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};

/// Represents an enumerated 802.11 Wi-Fi Access Point scanned from the wireless environment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WifiAccessPoint {
    /// Decoded human-readable Service Set Identifier (network name).
    pub ssid: String,
    /// Normalized signal strength indicator from `0` (no signal) to `100` (optimal).
    pub signal_strength: u8,
    /// Security encryption protocol string (`"WPA3"`, `"WPA2"`, `"WPA"`, `"WEP"`, or `"Open"`).
    pub security: String,
    /// Whether the device is currently associated and authenticated with this access point.
    pub is_connected: bool,
    /// D-Bus object path of the AccessPoint instance (e.g., `/org/freedesktop/NetworkManager/AccessPoint/42`).
    pub path: String,
}

/// Instantaneous snapshot of system networking and wireless connectivity.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkState {
    /// Global Wi-Fi radio power state (hardware/software enabled).
    pub wifi_enabled: bool,
    /// SSID of the active wireless link, or empty if disconnected.
    pub connected_ssid: String,
    /// Signal quality percentage of the active connection (`0..=100`).
    pub signal_strength: u8,
    /// Deduplicated list of discovered Wi-Fi access points sorted by signal strength.
    pub access_points: Vec<WifiAccessPoint>,
    /// Indicates whether a valid NetworkManager service and Wi-Fi adapter are available.
    pub is_available: bool,
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManagerProxy {
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_wireless_enabled(&self, value: bool) -> zbus::Result<()>;

    fn activate_connection(
        &self,
        connection: &ObjectPath<'_>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;

    fn add_and_activate_connection(
        &self,
        connection: &HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait DeviceProxy {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait WirelessDeviceProxy {
    #[zbus(property)]
    fn access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;

    fn request_scan(&self, options: &HashMap<String, Value<'_>>) -> zbus::Result<()>;

    #[zbus(signal)]
    fn access_point_added(&self, access_point: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    fn access_point_removed(&self, access_point: OwnedObjectPath) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPointProxy {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;

    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn flags(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait SettingsProxy {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SettingsConnectionProxy {
    fn get_settings(
        &self,
    ) -> zbus::Result<HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>>;
}

/// Asynchronous controller managing interaction with NetworkManager system services.
pub struct NetworkBackend {
    conn: Option<zbus::connection::Connection>,
    wireless_device_path: Arc<Mutex<Option<OwnedObjectPath>>>,
    state_tx: watch::Sender<NetworkState>,
    state_rx: watch::Receiver<NetworkState>,
}

impl NetworkBackend {
    /// Initializes the NetworkManager client on the system D-Bus bus.
    ///
    /// Discovers the active wireless adapter and initiates background scanning tasks.
    /// Does not panic if D-Bus is unavailable, safely marking `is_available = false`.
    pub async fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(NetworkState::default());
        let wireless_device_path = Arc::new(Mutex::new(None));

        let conn = match zbus::connection::Connection::system().await {
            Ok(c) => Some(c),
            Err(err) => {
                warn!("NetworkBackend: System D-Bus unavailable: {err}");
                None
            }
        };

        let backend = Self {
            conn: conn.clone(),
            wireless_device_path: wireless_device_path.clone(),
            state_tx: state_tx.clone(),
            state_rx,
        };

        if let Some(ref connection) = conn {
            // Locate primary wireless device and populate initial access points
            backend.discover_and_update(connection).await;

            // Spawn background task for periodic AP polling and beacon refresh
            let conn_clone = connection.clone();
            let wd_path_clone = wireless_device_path.clone();
            let state_tx_clone = state_tx.clone();

            tokio::spawn(async move {
                Self::run_listener_and_poll(conn_clone, wd_path_clone, state_tx_clone).await;
            });
        }

        backend
    }

    /// Subscribes to reactive network state modifications.
    ///
    /// # Returns
    /// A `tokio::sync::watch::Receiver` receiving updated [`NetworkState`] instances.
    pub fn subscribe(&self) -> watch::Receiver<NetworkState> {
        self.state_rx.clone()
    }

    /// Returns the most recently cached snapshot of network state.
    pub fn current(&self) -> NetworkState {
        self.state_rx.borrow().clone()
    }

    /// Enumerates NetworkManager devices to find the primary Wi-Fi interface.
    async fn discover_and_update(&self, conn: &zbus::connection::Connection) {
        let nm = match NetworkManagerProxyProxy::new(conn).await {
            Ok(p) => p,
            Err(err) => {
                warn!("NetworkBackend: Failed to create NetworkManager proxy: {err}");
                return;
            }
        };

        let wifi_enabled = nm.wireless_enabled().await.unwrap_or(false);

        // Discover Wi-Fi devices matching NM_DEVICE_TYPE_WIFI == 2
        let devices = nm.get_devices().await.unwrap_or_default();
        let mut found_path = None;

        for dev_path in devices {
            if let Ok(dev_proxy) = DeviceProxyProxy::builder(conn).path(&dev_path) {
                if let Ok(dev) = dev_proxy.build().await {
                    if dev.device_type().await.unwrap_or(0) == 2 {
                        found_path = Some(dev_path);
                        break;
                    }
                }
            }
        }

        {
            let mut guard = self.wireless_device_path.lock().await;
            *guard = found_path.clone();
        }

        if let Some(ref w_path) = found_path {
            Self::refresh_access_points(conn, w_path, wifi_enabled, &self.state_tx).await;
        } else {
            let mut state = self.state_rx.borrow().clone();
            state.wifi_enabled = wifi_enabled;
            state.is_available = false;
            state.connected_ssid = String::new();
            state.access_points.clear();
            let _ = self.state_tx.send(state);
        }
    }

    /// Queries access point properties from NetworkManager and publishes them to the watch channel.
    async fn refresh_access_points(
        conn: &zbus::connection::Connection,
        w_path: &OwnedObjectPath,
        wifi_enabled: bool,
        state_tx: &watch::Sender<NetworkState>,
    ) {
        let w_proxy = match WirelessDeviceProxyProxy::builder(conn).path(w_path) {
            Ok(builder) => match builder.build().await {
                Ok(p) => p,
                Err(err) => {
                    warn!("NetworkBackend: Failed to bind WirelessDevice proxy: {err}");
                    return;
                }
            },
            Err(err) => {
                warn!("NetworkBackend: Failed to build WirelessDevice proxy: {err}");
                return;
            }
        };

        let active_ap_path = w_proxy
            .active_access_point()
            .await
            .unwrap_or_else(|_| OwnedObjectPath::try_from("/").unwrap());
        let ap_paths = w_proxy.access_points().await.unwrap_or_default();

        let mut points = Vec::new();
        let mut active_ssid = String::new();
        let mut active_strength = 0u8;

        for ap_path in ap_paths {
            let ap_proxy = match AccessPointProxyProxy::builder(conn).path(&ap_path) {
                Ok(b) => match b.build().await {
                    Ok(p) => p,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };

            let ssid_raw = ap_proxy.ssid().await.unwrap_or_default();
            if ssid_raw.is_empty() {
                continue; // Omit hidden SSIDs without broadcast beacon names
            }
            let ssid = String::from_utf8_lossy(&ssid_raw).trim().to_string();
            if ssid.is_empty() {
                continue;
            }

            let strength = ap_proxy.strength().await.unwrap_or(0);
            let rsn_flags = ap_proxy.rsn_flags().await.unwrap_or(0);
            let wpa_flags = ap_proxy.wpa_flags().await.unwrap_or(0);
            let flags = ap_proxy.flags().await.unwrap_or(0);

            // Decode 802.11 security flags
            let security = if (rsn_flags & 0x200) != 0 {
                "WPA3".to_string()
            } else if rsn_flags != 0 {
                "WPA2".to_string()
            } else if wpa_flags != 0 {
                "WPA".to_string()
            } else if (flags & 0x1) != 0 {
                "WEP".to_string()
            } else {
                "Open".to_string()
            };

            let is_connected = ap_path.as_str() != "/" && ap_path == active_ap_path;
            if is_connected {
                active_ssid = ssid.clone();
                active_strength = strength;
            }

            points.push(WifiAccessPoint {
                ssid,
                signal_strength: strength,
                security,
                is_connected,
                path: ap_path.as_str().to_string(),
            });
        }

        // Deduplicate mesh nodes and multi-band BSSIDs sharing the same SSID
        let mut deduplicated: HashMap<String, WifiAccessPoint> = HashMap::new();
        for ap in points {
            match deduplicated.get_mut(&ap.ssid) {
                Some(existing) => {
                    if ap.is_connected {
                        existing.is_connected = true;
                    }
                    if ap.signal_strength > existing.signal_strength {
                        existing.signal_strength = ap.signal_strength;
                        existing.security = ap.security;
                        existing.path = ap.path;
                    }
                }
                None => {
                    deduplicated.insert(ap.ssid.clone(), ap);
                }
            }
        }

        let mut final_list: Vec<WifiAccessPoint> = deduplicated.into_values().collect();

        // Sort: active link first, then descending by signal strength
        final_list.sort_by(|a, b| {
            b.is_connected
                .cmp(&a.is_connected)
                .then_with(|| b.signal_strength.cmp(&a.signal_strength))
                .then_with(|| a.ssid.cmp(&b.ssid))
        });

        let updated = NetworkState {
            wifi_enabled,
            connected_ssid: active_ssid,
            signal_strength: active_strength,
            access_points: final_list,
            is_available: true,
        };

        let _ = state_tx.send(updated);
    }

    /// Background worker periodically triggering active Wi-Fi scans and publishing results.
    async fn run_listener_and_poll(
        conn: zbus::connection::Connection,
        wd_path_lock: Arc<Mutex<Option<OwnedObjectPath>>>,
        state_tx: watch::Sender<NetworkState>,
    ) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));

        loop {
            interval.tick().await;

            let path_opt = {
                let guard = wd_path_lock.lock().await;
                guard.clone()
            };

            if let Some(ref w_path) = path_opt {
                if let Ok(nm) = NetworkManagerProxyProxy::new(&conn).await {
                    let enabled = nm.wireless_enabled().await.unwrap_or(false);
                    if enabled {
                        if let Ok(builder) = WirelessDeviceProxyProxy::builder(&conn).path(w_path) {
                            if let Ok(w_proxy) = builder.build().await {
                                let empty_opts = HashMap::new();
                                let _ = w_proxy.request_scan(&empty_opts).await;
                            }
                        }
                    }
                    Self::refresh_access_points(&conn, w_path, enabled, &state_tx).await;
                }
            }
        }
    }

    /// Enables or disables the wireless networking radio.
    ///
    /// # Arguments
    /// * `enabled` - `true` to power on Wi-Fi radio and begin beacon reception; `false` to disable.
    pub async fn set_wifi_enabled(&self, enabled: bool) {
        if let Some(ref conn) = self.conn {
            if let Ok(nm) = NetworkManagerProxyProxy::new(conn).await {
                if let Err(err) = nm.set_wireless_enabled(enabled).await {
                    warn!("NetworkBackend: Failed to set wireless enabled: {err}");
                }
            }
            self.discover_and_update(conn).await;
        }
    }

    /// Explicitly triggers an out-of-band active wireless scan.
    pub async fn request_scan(&self) {
        if let Some(ref conn) = self.conn {
            let path_opt = {
                let guard = self.wireless_device_path.lock().await;
                guard.clone()
            };
            if let Some(ref w_path) = path_opt {
                if let Ok(builder) = WirelessDeviceProxyProxy::builder(conn).path(w_path) {
                    if let Ok(w_proxy) = builder.build().await {
                        let empty_opts = HashMap::new();
                        let _ = w_proxy.request_scan(&empty_opts).await;
                        info!("NetworkBackend: Triggered wireless scan on {}", w_path.as_str());
                    }
                }
                let enabled = self.state_rx.borrow().wifi_enabled;
                Self::refresh_access_points(conn, w_path, enabled, &self.state_tx).await;
            }
        }
    }

    /// Initiates association and authentication with the target network SSID.
    ///
    /// Reuses existing saved connection profiles when available, or provisions a new
    /// connection via `AddAndActivateConnection`.
    ///
    /// # Arguments
    /// * `ssid` - The target network SSID string.
    /// * `password` - Optional WPA/WPA2 pre-shared key for password-protected networks.
    pub async fn connect_to_network(&self, ssid: &str, password: Option<&str>) {
        let conn = match self.conn {
            Some(ref c) => c,
            None => return,
        };

        let w_path = {
            let guard = self.wireless_device_path.lock().await;
            match guard.clone() {
                Some(p) => p,
                None => return,
            }
        };

        let nm = match NetworkManagerProxyProxy::new(conn).await {
            Ok(p) => p,
            Err(_) => return,
        };

        let ap_target = {
            let state = self.state_rx.borrow();
            state
                .access_points
                .iter()
                .find(|a| a.ssid == ssid)
                .map(|a| a.path.clone())
        };

        let ap_path = match ap_target {
            Some(p) => match OwnedObjectPath::try_from(p) {
                Ok(path) => path,
                Err(_) => OwnedObjectPath::try_from("/").unwrap(),
            },
            None => OwnedObjectPath::try_from("/").unwrap(),
        };

        // Check if an existing connection configuration matches this SSID
        let mut existing_conn_path = None;
        if let Ok(settings_proxy) = SettingsProxyProxy::new(conn).await {
            if let Ok(connections) = settings_proxy.list_connections().await {
                for c_path in connections {
                    if let Ok(builder) = SettingsConnectionProxyProxy::builder(conn).path(&c_path) {
                        if let Ok(c_proxy) = builder.build().await {
                            if let Ok(settings) = c_proxy.get_settings().await {
                                if let Some(wireless) = settings.get("802-11-wireless") {
                                    if let Some(s_val) = wireless.get("ssid") {
                                        let bytes_opt = match &**s_val {
                                            Value::Array(arr) => {
                                                let mut b = Vec::new();
                                                for item in arr.iter() {
                                                    if let Value::U8(u) = item {
                                                        b.push(*u);
                                                    }
                                                }
                                                Some(b)
                                            }
                                            Value::Str(s) => Some(s.as_bytes().to_vec()),
                                            _ => None,
                                        };
                                        if let Some(bytes) = bytes_opt {
                                            if String::from_utf8_lossy(&bytes).trim() == ssid {
                                                existing_conn_path = Some(c_path);
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(c_path) = existing_conn_path {
            info!("NetworkBackend: Activating existing connection for SSID '{ssid}'");
            let _ = nm
                .activate_connection(&c_path.as_ref(), &w_path.as_ref(), &ap_path.as_ref())
                .await;
        } else {
            info!("NetworkBackend: Creating and activating new connection for SSID '{ssid}'");
            let mut connection_setting: HashMap<&str, Value<'_>> = HashMap::new();
            connection_setting.insert("type", Value::from("802-11-wireless"));
            connection_setting.insert("id", Value::from(ssid));

            let mut wireless_setting: HashMap<&str, Value<'_>> = HashMap::new();
            let ssid_bytes = ssid.as_bytes().to_vec();
            wireless_setting.insert("ssid", Value::from(ssid_bytes));
            wireless_setting.insert("mode", Value::from("infrastructure"));

            let mut full_settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
            full_settings.insert("connection", connection_setting);
            full_settings.insert("802-11-wireless", wireless_setting);

            let mut sec_setting: HashMap<&str, Value<'_>> = HashMap::new();
            if let Some(pass) = password {
                sec_setting.insert("key-mgmt", Value::from("wpa-psk"));
                sec_setting.insert("psk", Value::from(pass));
                full_settings.insert("802-11-wireless-security", sec_setting);
            }

            let _ = nm
                .add_and_activate_connection(&full_settings, &w_path.as_ref(), &ap_path.as_ref())
                .await;
        }

        let enabled = self.state_rx.borrow().wifi_enabled;
        Self::refresh_access_points(conn, &w_path, enabled, &self.state_tx).await;
    }
}
