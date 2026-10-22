//! UPower D-Bus interface client with automatic mock fallback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{info, warn};

/// Battery charging state modeled after UPower Device State.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryState {
    Unknown,
    Charging,
    Discharging,
    Empty,
    FullyCharged,
    PendingCharge,
    PendingDischarge,
}

impl BatteryState {
    pub fn is_charging(&self) -> bool {
        matches!(self, BatteryState::Charging | BatteryState::PendingCharge)
    }
}

/// Instantaneous battery telemetry.
#[derive(Debug, Clone, PartialEq)]
pub struct BatteryInfo {
    pub percentage: f64,
    pub state: BatteryState,
    pub is_charging: bool,
}

impl Default for BatteryInfo {
    fn default() -> Self {
        Self {
            percentage: 86.0,
            state: BatteryState::Discharging,
            is_charging: false,
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait UPowerDevice {
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;

    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

/// Battery service providing async telemetry and mock fallback.
pub struct BatteryService {
    _info_tx: watch::Sender<BatteryInfo>,
    info_rx: watch::Receiver<BatteryInfo>,
    is_mock: Arc<AtomicBool>,
}

impl BatteryService {
    /// Connects to UPower on the system bus. If unavailable, initializes the mock provider.
    pub async fn new() -> Self {
        let (info_tx, info_rx) = watch::channel(Self::read_system());
        let is_mock = Arc::new(AtomicBool::new(false));

        let service = Self {
            _info_tx: info_tx.clone(),
            info_rx,
            is_mock: is_mock.clone(),
        };

        match zbus::connection::Connection::system().await {
            Ok(connection) => match UPowerDeviceProxy::new(&connection).await {
                Ok(proxy) => {
                    info!("Connected to UPower DisplayDevice on system bus");
                    tokio::spawn(Self::run_dbus_poller(proxy, info_tx));
                }
                Err(err) => {
                    warn!("Failed to bind UPowerDevice proxy: {err}. Starting mock provider");
                    is_mock.store(true, Ordering::SeqCst);
                    tokio::spawn(Self::run_mock_simulator(info_tx));
                }
            },
            Err(err) => {
                warn!("System D-Bus unavailable: {err}. Starting mock battery provider");
                is_mock.store(true, Ordering::SeqCst);
                tokio::spawn(Self::run_mock_simulator(info_tx));
            }
        }

        service
    }

    /// Explicitly forces a mock battery service for headless simulation or testing.
    pub fn new_mock(percentage: f64, is_charging: bool) -> Self {
        let info = BatteryInfo {
            percentage,
            state: if is_charging {
                BatteryState::Charging
            } else {
                BatteryState::Discharging
            },
            is_charging,
        };
        let (info_tx, info_rx) = watch::channel(info);
        let is_mock = Arc::new(AtomicBool::new(true));

        Self {
            _info_tx: info_tx,
            info_rx,
            is_mock,
        }
    }

    /// Subscribes to battery changes.
    pub fn subscribe(&self) -> watch::Receiver<BatteryInfo> {
        self.info_rx.clone()
    }

    /// Current snapshot.
    pub fn current(&self) -> BatteryInfo {
        self.info_rx.borrow().clone()
    }

    /// Whether this service is backed by mock fallback.
    pub fn is_mock(&self) -> bool {
        self.is_mock.load(Ordering::SeqCst)
    }

    async fn run_dbus_poller(proxy: UPowerDeviceProxy<'_>, tx: watch::Sender<BatteryInfo>) {
        loop {
            let percentage = proxy.percentage().await.unwrap_or(86.0);
            let state_code = proxy.state().await.unwrap_or(2);
            let state = match state_code {
                1 => BatteryState::Charging,
                2 => BatteryState::Discharging,
                3 => BatteryState::Empty,
                4 => BatteryState::FullyCharged,
                5 => BatteryState::PendingCharge,
                6 => BatteryState::PendingDischarge,
                _ => BatteryState::Unknown,
            };

            let is_charging = state.is_charging();
            let _ = tx.send(BatteryInfo {
                percentage,
                state,
                is_charging,
            });

            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    }

    /// Reads the real battery from /sys/class/power_supply; no battery => AC at 100%.
    fn read_system() -> BatteryInfo {
        if let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") {
            for e in dir.flatten() {
                let p = e.path();
                let ty = std::fs::read_to_string(p.join("type")).unwrap_or_default();
                if ty.trim() != "Battery" {
                    continue;
                }
                let pct = std::fs::read_to_string(p.join("capacity"))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .unwrap_or(100.0);
                let state = match std::fs::read_to_string(p.join("status"))
                    .unwrap_or_default()
                    .trim()
                {
                    "Charging" => BatteryState::Charging,
                    "Discharging" => BatteryState::Discharging,
                    "Full" => BatteryState::FullyCharged,
                    "Not charging" => BatteryState::PendingCharge,
                    _ => BatteryState::Unknown,
                };
                return BatteryInfo {
                    percentage: pct,
                    is_charging: state.is_charging(),
                    state,
                };
            }
        }
        BatteryInfo {
            percentage: 100.0,
            state: BatteryState::FullyCharged,
            is_charging: false,
        }
    }

    async fn run_mock_simulator(tx: watch::Sender<BatteryInfo>) {
        loop {
            let _ = tx.send(Self::read_system());
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    }
}
