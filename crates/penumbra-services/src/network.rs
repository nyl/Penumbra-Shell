//! NetworkManager D-Bus interface client with automatic mock fallback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{info, warn};

/// Connectivity status as defined by NetworkManager NMConnectivityState.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityState {
    Unknown = 0,
    None = 1,
    Portal = 2,
    Limited = 3,
    Full = 4,
}

/// Instantaneous network telemetry.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkInfo {
    pub is_connected: bool,
    pub is_wifi_enabled: bool,
    pub ssid: String,
    pub signal_strength: u8,
    pub connectivity: ConnectivityState,
}

impl Default for NetworkInfo {
    fn default() -> Self {
        Self {
            is_connected: true,
            is_wifi_enabled: true,
            ssid: "Penumbra-Mesh-5G".to_string(),
            signal_strength: 95,
            connectivity: ConnectivityState::Full,
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManagerProxy {
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;
}

/// Network service providing Wi-Fi / Cellular telemetry.
pub struct NetworkService {
    _info_tx: watch::Sender<NetworkInfo>,
    info_rx: watch::Receiver<NetworkInfo>,
    is_mock: Arc<AtomicBool>,
}

impl NetworkService {
    /// Connects to NetworkManager on the system bus. Falls back to mock provider if unavailable.
    pub async fn new() -> Self {
        let (info_tx, info_rx) = watch::channel(Self::read_system());
        let is_mock = Arc::new(AtomicBool::new(false));

        let service = Self {
            _info_tx: info_tx.clone(),
            info_rx,
            is_mock: is_mock.clone(),
        };

        match zbus::connection::Connection::system().await {
            Ok(connection) => match NetworkManagerProxyProxy::new(&connection).await {
                Ok(proxy) => {
                    info!("Connected to NetworkManager on system bus");
                    tokio::spawn(Self::run_dbus_poller(proxy, info_tx));
                }
                Err(err) => {
                    warn!("Failed to bind NetworkManager proxy: {err}. Starting mock provider");
                    is_mock.store(true, Ordering::SeqCst);
                    tokio::spawn(Self::run_mock_simulator(info_tx));
                }
            },
            Err(err) => {
                warn!("System D-Bus unavailable: {err}. Starting mock network provider");
                is_mock.store(true, Ordering::SeqCst);
                tokio::spawn(Self::run_mock_simulator(info_tx));
            }
        }

        service
    }

    /// Explicitly forces a mock network service for headless testing.
    pub fn new_mock(ssid: impl Into<String>, signal_strength: u8) -> Self {
        let info = NetworkInfo {
            is_connected: true,
            is_wifi_enabled: true,
            ssid: ssid.into(),
            signal_strength,
            connectivity: ConnectivityState::Full,
        };
        let (info_tx, info_rx) = watch::channel(info);
        let is_mock = Arc::new(AtomicBool::new(true));

        Self {
            _info_tx: info_tx,
            info_rx,
            is_mock,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<NetworkInfo> {
        self.info_rx.clone()
    }

    pub fn current(&self) -> NetworkInfo {
        self.info_rx.borrow().clone()
    }

    pub fn is_mock(&self) -> bool {
        self.is_mock.load(Ordering::SeqCst)
    }

    async fn run_dbus_poller(proxy: NetworkManagerProxyProxy<'_>, tx: watch::Sender<NetworkInfo>) {
        loop {
            let wifi_enabled = proxy.wireless_enabled().await.unwrap_or(true);
            let conn_code = proxy.connectivity().await.unwrap_or(4);
            let connectivity = match conn_code {
                1 => ConnectivityState::None,
                2 => ConnectivityState::Portal,
                3 => ConnectivityState::Limited,
                4 => ConnectivityState::Full,
                _ => ConnectivityState::Unknown,
            };

            let is_connected = matches!(
                connectivity,
                ConnectivityState::Full | ConnectivityState::Limited
            );

            let sys = Self::read_system();
            let _ = tx.send(NetworkInfo {
                is_connected,
                is_wifi_enabled: wifi_enabled,
                ssid: sys.ssid,
                signal_strength: if is_connected { sys.signal_strength } else { 0 },
                connectivity,
            });

            tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;
        }
    }

    /// Reads real link state from the OS (no fabricated values).
    fn read_system() -> NetworkInfo {
        let mut up_iface = false;
        let mut wifi_iface = false;
        if let Ok(dir) = std::fs::read_dir("/sys/class/net") {
            for e in dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name == "lo" {
                    continue;
                }
                let state = std::fs::read_to_string(e.path().join("operstate")).unwrap_or_default();
                if state.trim() == "up" {
                    up_iface = true;
                    if e.path().join("wireless").exists() {
                        wifi_iface = true;
                    }
                }
            }
        }
        // Link quality (0..70) from /proc/net/wireless.
        let mut signal = 0u8;
        if let Ok(s) = std::fs::read_to_string("/proc/net/wireless") {
            for line in s.lines().skip(2) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if let Some(q) = cols.get(2) {
                    if let Ok(q) = q.trim_end_matches('.').parse::<f32>() {
                        signal = ((q / 70.0) * 100.0).clamp(0.0, 100.0) as u8;
                    }
                }
            }
        }
        let mut ssid = String::new();
        if wifi_iface {
            if let Ok(out) = std::process::Command::new("nmcli")
                .args(["-t", "-f", "active,ssid", "dev", "wifi"])
                .output()
            {
                for l in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some(s) = l.strip_prefix("yes:") {
                        ssid = s.to_string();
                    }
                }
            }
        }
        let wifi_enabled = wifi_iface || std::path::Path::new("/proc/net/wireless").exists();
        NetworkInfo {
            is_connected: up_iface,
            is_wifi_enabled: wifi_enabled,
            ssid: if up_iface && ssid.is_empty() && !wifi_iface { "Ethernet".into() } else { ssid },
            signal_strength: if wifi_iface { signal } else if up_iface { 100 } else { 0 },
            connectivity: if up_iface { ConnectivityState::Full } else { ConnectivityState::None },
        }
    }

    async fn run_mock_simulator(tx: watch::Sender<NetworkInfo>) {
        // Fallback without NetworkManager: report real kernel state.
        loop {
            let _ = tx.send(Self::read_system());
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    }
}
