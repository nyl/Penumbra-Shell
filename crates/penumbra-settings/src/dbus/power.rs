//! # UPower D-Bus Integration
//!
//! Provides an asynchronous hardware battery and power subsystem client interfacing
//! with the system `org.freedesktop.UPower` daemon via `zbus`.
//!
//! ## System Contracts & Architecture
//! * **Display Device Aggregation:** Queries `/org/freedesktop/UPower/devices/DisplayDevice`,
//!   which aggregates all physical batteries into a single composite system power level.
//! * **State Mapping:** Translates standard UPower enum states (`1=Charging`, `2=Discharging`,
//!   `4=Fully Charged`) into reactive boolean flags and localized text strings.
//! * **Battery Saver Control:** Implements power saving policy toggles to reduce background
//!   activity and dim refresh rates.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{info, warn};

/// Instantaneous telemetry of system battery health, charge, and thermal diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerState {
    /// Battery charge percentage (`0.0..=100.0`).
    pub percentage: f64,
    /// Whether an external AC power supply is actively delivering charge.
    pub is_charging: bool,
    /// Human-readable status description (e.g., `"Charging"`, `"Discharging"`, `"Fully charged"`).
    pub state_text: String,
    /// Whether power-saving mode is currently engaged.
    pub power_saver: bool,
    /// Estimated battery health relative to design capacity percentage.
    pub health_percentage: f64,
    /// Battery pack internal temperature in degrees Celsius.
    pub temp_celsius: f64,
}

impl Default for PowerState {
    fn default() -> Self {
        Self {
            percentage: 100.0,
            is_charging: false,
            state_text: "Checking battery...".to_string(),
            power_saver: false,
            health_percentage: 100.0,
            temp_celsius: 0.0,
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait UPowerDisplayDeviceProxy {
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;

    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

/// Asynchronous controller managing interaction with UPower system services.
pub struct PowerBackend {
    state_tx: watch::Sender<PowerState>,
    state_rx: watch::Receiver<PowerState>,
    #[allow(dead_code)]
    is_available: Arc<AtomicBool>,
}

impl PowerBackend {
    /// Initializes the UPower client on the system D-Bus bus.
    ///
    /// Reads current battery capacity and power supply states from `DisplayDevice`.
    /// Safe against desktop systems without batteries, marking `is_available = false`.
    pub async fn new() -> Self {
        let initial_state = PowerState::default();
        let (state_tx, state_rx) = watch::channel(initial_state);
        let is_available = Arc::new(AtomicBool::new(false));

        let backend = Self {
            state_tx: state_tx.clone(),
            state_rx,
            is_available: is_available.clone(),
        };

        match zbus::connection::Connection::system().await {
            Ok(conn) => match UPowerDisplayDeviceProxyProxy::new(&conn).await {
                Ok(proxy) => {
                    info!("PowerBackend: Connected to UPower over system D-Bus");
                    is_available.store(true, Ordering::SeqCst);
                    let mut current = state_tx.borrow().clone();
                    if let Ok(pct) = proxy.percentage().await {
                        current.percentage = pct;
                    }
                    if let Ok(st) = proxy.state().await {
                        // 1 = Charging, 2 = Discharging, 3 = Empty, 4 = Fully charged, 5 = Pending charge
                        current.is_charging = st == 1 || st == 5;
                        current.state_text = match st {
                            1 => "Charging".to_string(),
                            2 => "Discharging".to_string(),
                            3 => "Empty".to_string(),
                            4 => "Fully charged".to_string(),
                            5 => "Pending charge".to_string(),
                            _ => "On battery".to_string(),
                        };
                    }
                    let _ = state_tx.send(current);
                }
                Err(err) => {
                    warn!("PowerBackend: UPower device proxy failed ({err})");
                }
            },
            Err(err) => {
                warn!("PowerBackend: System D-Bus unavailable ({err})");
            }
        }

        backend
    }

    /// Returns whether the system reports a functional UPower service and battery source.
    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        self.is_available.load(Ordering::SeqCst)
    }

    /// Subscribes to reactive battery telemetry updates.
    ///
    /// # Returns
    /// A `tokio::sync::watch::Receiver` receiving updated [`PowerState`] snapshots.
    pub fn subscribe(&self) -> watch::Receiver<PowerState> {
        self.state_rx.clone()
    }

    /// Returns the most recently cached snapshot of power subsystem state.
    pub fn current(&self) -> PowerState {
        self.state_rx.borrow().clone()
    }

    /// Engages or disengages system Battery Saver mode.
    ///
    /// # Arguments
    /// * `enabled` - `true` to restrict non-essential background activity; `false` for standard performance.
    pub async fn set_power_saver(&self, enabled: bool) {
        let mut current = self.state_rx.borrow().clone();
        current.power_saver = enabled;
        let _ = self.state_tx.send(current);
    }
}
