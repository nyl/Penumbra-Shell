//! # Hardware D-Bus Services & Single-Instance IPC Subsystem
//!
//! Exposes asynchronous wrappers for hardware daemons (NetworkManager, BlueZ, UPower)
//! alongside the application-level single-instance IPC service (`org.penumbra.Settings`).
//!
//! ## Submodules
//! * [`network`]: NetworkManager client for 802.11 Wi-Fi scanning and link association.
//! * [`bluetooth`]: BlueZ ObjectManager client for Bluetooth discovery and peripheral control.
//! * [`power`]: UPower client for system battery telemetry and power saver management.
//!
//! ## Single-Instance D-Bus Protocol
//! To prevent multiple conflicting settings windows on Linux mobile environments,
//! `penumbra-settings` claims the well-known bus name `org.penumbra.Settings` on the user session bus:
//! 1. When a new process launches with `--page <PAGE>`, it first attempts to invoke `OpenPage(<PAGE>)`
//!    on `org.penumbra.Settings`.
//! 2. If an existing instance responds within 600ms, the secondary process exits immediately.
//! 3. If no instance exists, the current process registers `org.penumbra.Settings`, renders the
//!    window, and processes subsequent incoming activations asynchronously.

pub mod bluetooth;
pub mod network;
pub mod power;

#[allow(unused_imports)]
pub use bluetooth::{BluetoothBackend, BluetoothDevice, BluetoothState};
#[allow(unused_imports)]
pub use network::{NetworkBackend, NetworkState, WifiAccessPoint};
#[allow(unused_imports)]
pub use power::{PowerBackend, PowerState};

use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

/// D-Bus client proxy interface used by secondary CLI invocations to signal the active window.
#[zbus::proxy(
    interface = "org.penumbra.Settings",
    default_service = "org.penumbra.Settings",
    default_path = "/org/penumbra/Settings"
)]
pub trait SettingsClientProxy {
    /// Requests the active settings instance to navigate to the specified subpage.
    fn open_page(&self, page: &str) -> zbus::Result<()>;
}

/// Server object handling runtime activation requests on the user session bus.
pub struct SettingsServer {
    page_sender: UnboundedSender<String>,
}

#[zbus::interface(name = "org.penumbra.Settings")]
impl SettingsServer {
    /// D-Bus method invoked by external applications or secondary CLI processes.
    ///
    /// # Arguments
    /// * `page` - Target page identifier (e.g., `"wifi"`, `"bluetooth"`, `"display"`).
    async fn open_page(&self, page: String) -> zbus::fdo::Result<()> {
        info!("Received D-Bus OpenPage request for '{page}'");
        let _ = self.page_sender.send(page);
        Ok(())
    }
}

/// Attempts to activate an already running instance of `penumbra-settings` via D-Bus.
///
/// # Arguments
/// * `page` - Canonical subpage name to pass to the active instance.
///
/// # Returns
/// `true` if an active instance handled the navigation request, allowing the current
/// process to terminate cleanly; `false` if no instance is running.
pub async fn try_activate_existing(page: &str) -> bool {
    match zbus::connection::Connection::session().await {
        Ok(conn) => match SettingsClientProxyProxy::new(&conn).await {
            Ok(proxy) => {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(600),
                    proxy.open_page(page),
                )
                .await
                {
                    Ok(Ok(())) => {
                        info!("Successfully routed page '{page}' to active Settings instance");
                        true
                    }
                    _ => false,
                }
            }
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Registers the `org.penumbra.Settings` single-instance server on the user session bus.
///
/// Incoming `OpenPage` calls are forwarded into the provided `page_sender` channel,
/// which dispatches page navigation onto the Slint UI thread.
///
/// # Arguments
/// * `page_sender` - Unbounded Tokio channel sender receiving target page strings.
///
/// # Returns
/// `Some(Connection)` keeping the D-Bus registration alive, or `None` if the session bus was unreachable.
pub async fn start_service(
    page_sender: UnboundedSender<String>,
) -> Option<zbus::connection::Connection> {
    match zbus::connection::Builder::session() {
        Ok(builder) => {
            let server = SettingsServer { page_sender };
            match builder
                .name("org.penumbra.Settings")
                .and_then(|b| b.serve_at("/org/penumbra/Settings", server))
            {
                Ok(b) => match b.build().await {
                    Ok(conn) => {
                        info!("Registered org.penumbra.Settings single-instance D-Bus service");
                        Some(conn)
                    }
                    Err(err) => {
                        warn!("Could not bind session D-Bus service: {err}");
                        None
                    }
                },
                Err(err) => {
                    warn!("Could not configure D-Bus server: {err}");
                    None
                }
            }
        }
        Err(err) => {
            warn!("Session D-Bus unavailable: {err}");
            None
        }
    }
}
