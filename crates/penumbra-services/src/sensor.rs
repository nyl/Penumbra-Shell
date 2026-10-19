//! iio-sensor-proxy client with mock fallback for ambient light and orientation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{info, warn};

/// Orientation reported by net.hadess.SensorProxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Normal,
    BottomUp,
    LeftUp,
    RightUp,
    Undefined,
}

/// Instantaneous sensor telemetry.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorInfo {
    pub ambient_light_lux: f64,
    pub orientation: Orientation,
}

impl Default for SensorInfo {
    fn default() -> Self {
        Self {
            ambient_light_lux: 420.0,
            orientation: Orientation::Normal,
        }
    }
}

#[zbus::proxy(
    interface = "net.hadess.SensorProxy",
    default_service = "net.hadess.SensorProxy",
    default_path = "/net/hadess/SensorProxy"
)]
trait SensorProxyDbus {
    #[zbus(property)]
    fn has_accelerometer(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn has_ambient_light(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn light_level(&self) -> zbus::Result<f64>;

    #[zbus(property)]
    fn accelerometer_orientation(&self) -> zbus::Result<String>;
}

/// Sensor service providing orientation and light sensor telemetry.
pub struct SensorService {
    _info_tx: watch::Sender<SensorInfo>,
    info_rx: watch::Receiver<SensorInfo>,
    is_mock: Arc<AtomicBool>,
}

impl SensorService {
    pub async fn new() -> Self {
        let (info_tx, info_rx) = watch::channel(SensorInfo::default());
        let is_mock = Arc::new(AtomicBool::new(false));

        let service = Self {
            _info_tx: info_tx.clone(),
            info_rx,
            is_mock: is_mock.clone(),
        };

        match zbus::connection::Connection::system().await {
            Ok(connection) => match SensorProxyDbusProxy::new(&connection).await {
                Ok(proxy) => {
                    info!("Connected to SensorProxy on system bus");
                    tokio::spawn(Self::run_dbus_poller(proxy, info_tx));
                }
                Err(err) => {
                    warn!("Failed to bind SensorProxy: {err}. Starting mock provider");
                    is_mock.store(true, Ordering::SeqCst);
                    tokio::spawn(Self::run_mock_simulator(info_tx));
                }
            },
            Err(err) => {
                warn!("System D-Bus unavailable: {err}. Starting mock sensor provider");
                is_mock.store(true, Ordering::SeqCst);
                tokio::spawn(Self::run_mock_simulator(info_tx));
            }
        }

        service
    }

    pub fn new_mock(lux: f64, orientation: Orientation) -> Self {
        let (info_tx, info_rx) = watch::channel(SensorInfo {
            ambient_light_lux: lux,
            orientation,
        });
        Self {
            _info_tx: info_tx,
            info_rx,
            is_mock: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<SensorInfo> {
        self.info_rx.clone()
    }

    pub fn current(&self) -> SensorInfo {
        self.info_rx.borrow().clone()
    }

    pub fn is_mock(&self) -> bool {
        self.is_mock.load(Ordering::SeqCst)
    }

    async fn run_dbus_poller(proxy: SensorProxyDbusProxy<'_>, tx: watch::Sender<SensorInfo>) {
        loop {
            let lux = proxy.light_level().await.unwrap_or(420.0);
            let orient_str = proxy.accelerometer_orientation().await.unwrap_or_default();
            let orientation = match orient_str.as_str() {
                "normal" => Orientation::Normal,
                "bottom-up" => Orientation::BottomUp,
                "left-up" => Orientation::LeftUp,
                "right-up" => Orientation::RightUp,
                _ => Orientation::Normal,
            };

            let _ = tx.send(SensorInfo {
                ambient_light_lux: lux,
                orientation,
            });

            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
        }
    }

    async fn run_mock_simulator(tx: watch::Sender<SensorInfo>) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(12)).await;
            let _ = tx.send(SensorInfo {
                ambient_light_lux: 450.0,
                orientation: Orientation::Normal,
            });
        }
    }
}
