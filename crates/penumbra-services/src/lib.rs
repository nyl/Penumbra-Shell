//! Penumbra Services: Asynchronous D-Bus hardware clients for UPower,
//! NetworkManager, and iio-sensor-proxy with deterministic mock fallbacks.

pub mod audio;
pub mod backlight;
pub mod battery;
pub mod network;
pub mod sensor;

pub use audio::{adjust_volume_step, apply_volume_ratio, read_volume_ratio};
pub use backlight::{apply_backlight_ratio, discover_backlight_dir, read_backlight_ratio};
pub use battery::{BatteryInfo, BatteryService, BatteryState};
pub use network::{ConnectivityState, NetworkInfo, NetworkService};
pub use sensor::{Orientation, SensorInfo, SensorService};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_battery_service() {
        let service = BatteryService::new_mock(92.0, false);
        let current = service.current();
        assert_eq!(current.percentage, 92.0);
        assert!(!current.is_charging);
        assert!(service.is_mock());
    }

    #[tokio::test]
    async fn test_mock_network_service() {
        let service = NetworkService::new_mock("TestSSID", 88);
        let current = service.current();
        assert_eq!(current.ssid, "TestSSID");
        assert_eq!(current.signal_strength, 88);
        assert!(current.is_connected);
        assert!(service.is_mock());
    }

    #[tokio::test]
    async fn test_mock_sensor_service() {
        let service = SensorService::new_mock(550.0, Orientation::Normal);
        let current = service.current();
        assert_eq!(current.ambient_light_lux, 550.0);
        assert_eq!(current.orientation, Orientation::Normal);
        assert!(service.is_mock());
    }
}
