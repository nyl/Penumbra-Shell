//! # BlueZ D-Bus ObjectManager Integration
//!
//! Provides an asynchronous Bluetooth client interfacing with Linux BlueZ daemon (`bluetoothd`)
//! over the system D-Bus bus via `zbus`.
//!
//! ## System Contracts & Architecture
//! * **ObjectManager Topology:** Queries `org.freedesktop.DBus.ObjectManager.GetManagedObjects()` on path `/`
//!   to discover all adapters and devices under a single unified call.
//! * **Adapter Control:** Locates the primary adapter implementing `org.bluez.Adapter1` (e.g. `/org/bluez/hci0`)
//!   and toggles its `Powered` and `Discovering` properties.
//! * **Device Enumeration:** Discovers peripheral devices implementing `org.bluez.Device1` (paired, trusted,
//!   or currently advertising).
//! * **Battery Reporting:** Reads battery capacity from companion `org.bluez.Battery1` interfaces when available.
//! * **Signal Monitoring:** Continuously listens for `InterfacesAdded` and `InterfacesRemoved` signals
//!   to dynamically add/remove discovered accessories without requiring polling.

use std::collections::HashMap;
use std::sync::Arc;
use futures_util::StreamExt;
use tokio::sync::{watch, Mutex};
use tracing::{info, warn};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// Represents a remote Bluetooth device discovered or paired by the local adapter.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BluetoothDevice {
    /// User-friendly alias or broadcast advertising name.
    pub name: String,
    /// Physical Bluetooth MAC address in uppercase hexadecimal format (e.g. `AA:BB:CC:DD:EE:FF`).
    pub address: String,
    /// Whether an active ACL link or profile is established with the remote device.
    pub is_connected: bool,
    /// Whether the device has completed pairing and security bonding.
    pub is_paired: bool,
    /// Battery charge percentage (`0..=100`), or `-1` if the device does not expose a Battery service.
    pub battery_level: i32,
    /// Freedesktop icon hint indicating device category (e.g., `"audio-headset"`, `"input-keyboard"`).
    pub icon: String,
    /// Canonical D-Bus object path of the `org.bluez.Device1` instance.
    pub path: String,
}

/// Instantaneous snapshot of local Bluetooth subsystem state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BluetoothState {
    /// Power status of the primary Bluetooth adapter radio.
    pub powered: bool,
    /// User-configured friendly name or alias of the local adapter.
    pub adapter_name: String,
    /// Canonical D-Bus path of the active adapter (`/org/bluez/hci0`).
    pub adapter_path: String,
    /// Whether the adapter is actively in discovery mode scanning for nearby advertising beacons.
    pub discovering: bool,
    /// Sorted list of known and discovered Bluetooth devices.
    pub devices: Vec<BluetoothDevice>,
    /// Whether a functional Bluetooth daemon and hardware adapter were detected.
    pub is_available: bool,
}

#[zbus::proxy(
    interface = "org.freedesktop.DBus.ObjectManager",
    default_service = "org.bluez",
    default_path = "/"
)]
trait BluezObjectManagerProxy {
    fn get_managed_objects(
        &self,
    ) -> zbus::Result<
        HashMap<
            OwnedObjectPath,
            HashMap<String, HashMap<String, OwnedValue>>,
        >,
    >;

    #[zbus(signal)]
    fn interfaces_added(
        &self,
        object_path: OwnedObjectPath,
        interfaces_and_properties: HashMap<String, HashMap<String, OwnedValue>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn interfaces_removed(
        &self,
        object_path: OwnedObjectPath,
        interfaces: Vec<String>,
    ) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.bluez.Adapter1",
    default_service = "org.bluez"
)]
trait BluezAdapterProxy {
    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_powered(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn discovering(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn alias(&self) -> zbus::Result<String>;

    fn start_discovery(&self) -> zbus::Result<()>;
    fn stop_discovery(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.bluez.Device1",
    default_service = "org.bluez"
)]
trait BluezDeviceProxy {
    #[zbus(property)]
    fn name(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn alias(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn address(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn connected(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn paired(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn icon(&self) -> zbus::Result<String>;

    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
    fn pair(&self) -> zbus::Result<()>;
}

/// Extracts a string property from a D-Bus property dictionary.
fn extract_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props.get(key).and_then(|v| match &**v {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    })
}

/// Extracts a boolean property from a D-Bus property dictionary with safe fallback to `false`.
fn extract_bool(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .map(|v| match &**v {
            Value::Bool(b) => *b,
            _ => false,
        })
        .unwrap_or(false)
}

/// Extracts an integer property (handling byte, short, or word representations) from a D-Bus dictionary.
fn extract_i32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    props.get(key).and_then(|v| match &**v {
        Value::U8(n) => Some(*n as i32),
        Value::I16(n) => Some(*n as i32),
        Value::U16(n) => Some(*n as i32),
        Value::I32(n) => Some(*n),
        Value::U32(n) => Some(*n as i32),
        _ => None,
    })
}

/// Asynchronous controller managing interaction with Linux BlueZ D-Bus services.
pub struct BluetoothBackend {
    conn: Option<zbus::connection::Connection>,
    adapter_path: Arc<Mutex<Option<OwnedObjectPath>>>,
    state_tx: watch::Sender<BluetoothState>,
    state_rx: watch::Receiver<BluetoothState>,
}

impl BluetoothBackend {
    /// Initializes the BlueZ client on the system D-Bus bus.
    ///
    /// Discovers existing adapters and paired devices, and spawns real-time signal monitors.
    /// Safe against missing daemons or non-root environments, marking `is_available = false`.
    pub async fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(BluetoothState::default());
        let adapter_path = Arc::new(Mutex::new(None));

        let conn = match zbus::connection::Connection::system().await {
            Ok(c) => Some(c),
            Err(err) => {
                warn!("BluetoothBackend: System D-Bus unavailable: {err}");
                None
            }
        };

        let backend = Self {
            conn: conn.clone(),
            adapter_path: adapter_path.clone(),
            state_tx: state_tx.clone(),
            state_rx,
        };

        if let Some(ref connection) = conn {
            backend.refresh_all(connection).await;

            // Spawn background task listening to BlueZ ObjectManager signal streams
            let conn_clone = connection.clone();
            let adapter_path_clone = adapter_path.clone();
            let state_tx_clone = state_tx.clone();

            tokio::spawn(async move {
                Self::run_signal_monitor(conn_clone, adapter_path_clone, state_tx_clone).await;
            });
        }

        backend
    }

    /// Subscribes to reactive Bluetooth state modifications.
    ///
    /// # Returns
    /// A `tokio::sync::watch::Receiver` receiving updated [`BluetoothState`] snapshots.
    pub fn subscribe(&self) -> watch::Receiver<BluetoothState> {
        self.state_rx.clone()
    }

    /// Returns the most recently cached snapshot of local Bluetooth state.
    pub fn current(&self) -> BluetoothState {
        self.state_rx.borrow().clone()
    }

    /// Queries the full managed object tree from BlueZ and publishes an updated state.
    async fn refresh_all(&self, conn: &zbus::connection::Connection) {
        Self::query_managed_objects(conn, &self.adapter_path, &self.state_tx).await;
    }

    /// Interrogates BlueZ ObjectManager and parses adapter and device hierarchies.
    async fn query_managed_objects(
        conn: &zbus::connection::Connection,
        adapter_path_lock: &Arc<Mutex<Option<OwnedObjectPath>>>,
        state_tx: &watch::Sender<BluetoothState>,
    ) {
        let om = match BluezObjectManagerProxyProxy::new(conn).await {
            Ok(p) => p,
            Err(err) => {
                warn!("BluetoothBackend: Failed to bind ObjectManager: {err}");
                let mut state = BluetoothState::default();
                state.is_available = false;
                let _ = state_tx.send(state);
                return;
            }
        };

        let objects = match om.get_managed_objects().await {
            Ok(objs) => objs,
            Err(err) => {
                warn!("BluetoothBackend: Failed to query GetManagedObjects: {err}");
                let mut state = BluetoothState::default();
                state.is_available = false;
                let _ = state_tx.send(state);
                return;
            }
        };

        let mut primary_adapter_path = None;
        let mut adapter_name = String::new();
        let mut powered = false;
        let mut discovering = false;
        let mut devices = Vec::new();

        // Locate primary adapter implementing org.bluez.Adapter1
        for (path, ifaces) in &objects {
            if let Some(adapter_props) = ifaces.get("org.bluez.Adapter1") {
                primary_adapter_path = Some(path.clone());
                powered = extract_bool(adapter_props, "Powered");
                discovering = extract_bool(adapter_props, "Discovering");
                adapter_name = extract_str(adapter_props, "Alias")
                    .or_else(|| extract_str(adapter_props, "Name"))
                    .unwrap_or_else(|| path.as_str().to_string());
                break;
            }
        }

        {
            let mut guard = adapter_path_lock.lock().await;
            *guard = primary_adapter_path.clone();
        }

        // Locate and parse all devices implementing org.bluez.Device1
        for (path, ifaces) in &objects {
            if let Some(dev_props) = ifaces.get("org.bluez.Device1") {
                let address = extract_str(dev_props, "Address").unwrap_or_default();
                let name = extract_str(dev_props, "Alias")
                    .or_else(|| extract_str(dev_props, "Name"))
                    .unwrap_or_else(|| {
                        if !address.is_empty() {
                            format!("Device ({address})")
                        } else {
                            "Unknown Device".to_string()
                        }
                    });

                let is_connected = extract_bool(dev_props, "Connected");
                let is_paired = extract_bool(dev_props, "Paired");
                let icon = extract_str(dev_props, "Icon").unwrap_or_else(|| "devices".to_string());

                let battery_level = if let Some(battery_props) = ifaces.get("org.bluez.Battery1") {
                    extract_i32(battery_props, "Percentage").unwrap_or(-1)
                } else {
                    -1
                };

                devices.push(BluetoothDevice {
                    name,
                    address,
                    is_connected,
                    is_paired,
                    battery_level,
                    icon,
                    path: path.as_str().to_string(),
                });
            }
        }

        // Sort: active connected devices first, then paired/bonded, then alphabetical
        devices.sort_by(|a, b| {
            b.is_connected
                .cmp(&a.is_connected)
                .then_with(|| b.is_paired.cmp(&a.is_paired))
                .then_with(|| a.name.cmp(&b.name))
        });

        let updated = BluetoothState {
            powered,
            adapter_name,
            adapter_path: primary_adapter_path
                .map(|p| p.as_str().to_string())
                .unwrap_or_default(),
            discovering,
            devices,
            is_available: true,
        };

        let _ = state_tx.send(updated);
    }

    /// Reactive signal monitor handling live interface registrations from BlueZ.
    async fn run_signal_monitor(
        conn: zbus::connection::Connection,
        adapter_path_lock: Arc<Mutex<Option<OwnedObjectPath>>>,
        state_tx: watch::Sender<BluetoothState>,
    ) {
        let om = match BluezObjectManagerProxyProxy::new(&conn).await {
            Ok(p) => p,
            Err(_) => return,
        };

        let mut added_stream = match om.receive_interfaces_added().await {
            Ok(s) => s,
            Err(_) => return,
        };

        let mut removed_stream = match om.receive_interfaces_removed().await {
            Ok(s) => s,
            Err(_) => return,
        };

        loop {
            tokio::select! {
                Some(_) = added_stream.next() => {
                    Self::query_managed_objects(&conn, &adapter_path_lock, &state_tx).await;
                }
                Some(_) = removed_stream.next() => {
                    Self::query_managed_objects(&conn, &adapter_path_lock, &state_tx).await;
                }
                else => break,
            }
        }
    }

    /// Powers on or off the primary Bluetooth adapter radio.
    ///
    /// # Arguments
    /// * `powered` - `true` to enable adapter radio; `false` to shut down and disconnect peripherals.
    pub async fn set_powered(&self, powered: bool) {
        let conn = match self.conn {
            Some(ref c) => c,
            None => return,
        };

        let adapter_path = {
            let guard = self.adapter_path.lock().await;
            guard.clone()
        };

        if let Some(ref a_path) = adapter_path {
            if let Ok(builder) = BluezAdapterProxyProxy::builder(conn).path(a_path) {
                if let Ok(adapter) = builder.build().await {
                    if let Err(err) = adapter.set_powered(powered).await {
                        warn!("BluetoothBackend: Failed to set powered {powered}: {err}");
                    }
                }
            }
        }

        self.refresh_all(conn).await;
    }

    /// Instructs the primary adapter to begin scanning for nearby advertising Bluetooth peripherals.
    pub async fn start_discovery(&self) {
        let conn = match self.conn {
            Some(ref c) => c,
            None => return,
        };

        let adapter_path = {
            let guard = self.adapter_path.lock().await;
            guard.clone()
        };

        if let Some(ref a_path) = adapter_path {
            if let Ok(builder) = BluezAdapterProxyProxy::builder(conn).path(a_path) {
                if let Ok(adapter) = builder.build().await {
                    let _ = adapter.start_discovery().await;
                    info!("BluetoothBackend: Started device discovery on {}", a_path.as_str());
                }
            }
        }

        self.refresh_all(conn).await;
    }

    /// Halts active Bluetooth discovery scanning to conserve radio power.
    pub async fn stop_discovery(&self) {
        let conn = match self.conn {
            Some(ref c) => c,
            None => return,
        };

        let adapter_path = {
            let guard = self.adapter_path.lock().await;
            guard.clone()
        };

        if let Some(ref a_path) = adapter_path {
            if let Ok(builder) = BluezAdapterProxyProxy::builder(conn).path(a_path) {
                if let Ok(adapter) = builder.build().await {
                    let _ = adapter.stop_discovery().await;
                    info!("BluetoothBackend: Stopped device discovery");
                }
            }
        }

        self.refresh_all(conn).await;
    }

    /// Pairs and initiates connection with a remote device object path.
    ///
    /// # Arguments
    /// * `device_path` - The canonical D-Bus object path (e.g. `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF`).
    #[allow(dead_code)]
    pub async fn pair_and_connect(&self, device_path: &str) {
        let conn = match self.conn {
            Some(ref c) => c,
            None => return,
        };

        let d_path = match OwnedObjectPath::try_from(device_path) {
            Ok(p) => p,
            Err(_) => return,
        };

        if let Ok(builder) = BluezDeviceProxyProxy::builder(conn).path(&d_path) {
            if let Ok(device) = builder.build().await {
                if let Err(e) = device.pair().await {
                    warn!("BluetoothBackend: Pair returned: {e}");
                }
                if let Err(e) = device.connect().await {
                    warn!("BluetoothBackend: Connect returned: {e}");
                }
            }
        }

        self.refresh_all(conn).await;
    }

    /// Toggles connection state for the device matching the specified MAC address.
    ///
    /// If currently connected, issues `Disconnect()`; otherwise, attempts `Connect()`.
    ///
    /// # Arguments
    /// * `address` - Target device hardware address in `AA:BB:CC:DD:EE:FF` format.
    pub async fn toggle_device(&self, address: &str) {
        let conn = match self.conn {
            Some(ref c) => c,
            None => return,
        };

        let target_device = {
            let state = self.state_rx.borrow();
            state
                .devices
                .iter()
                .find(|d| d.address == address)
                .map(|d| (d.path.clone(), d.is_connected))
        };

        if let Some((path_str, is_connected)) = target_device {
            if let Ok(d_path) = OwnedObjectPath::try_from(path_str) {
                if let Ok(builder) = BluezDeviceProxyProxy::builder(conn).path(&d_path) {
                    if let Ok(device) = builder.build().await {
                        if is_connected {
                            info!("BluetoothBackend: Disconnecting device {address}");
                            let _ = device.disconnect().await;
                        } else {
                            info!("BluetoothBackend: Connecting device {address}");
                            let _ = device.connect().await;
                        }
                    }
                }
            }
        }

        self.refresh_all(conn).await;
    }
}
