//! Error types for the Slint Layer Shell backend.

use thiserror::Error;

/// Strongly typed error enum for all layer-shell and Wayland platform operations.
#[derive(Debug, Error)]
pub enum LayerShellError {
    /// Failed to connect to the Wayland display socket.
    #[error("Wayland connection error: {0}")]
    ConnectionError(#[from] wayland_client::ConnectError),

    /// Error during Wayland event dispatching.
    #[error("Wayland dispatch error: {0}")]
    DispatchError(#[from] wayland_client::DispatchError),

    /// Missing protocol, interface, or invalid compositor state.
    #[error("Wayland protocol error: {0}")]
    ProtocolError(String),

    /// Failed to allocate or configure a shared memory buffer.
    #[error("Buffer allocation error: {0}")]
    BufferAllocationError(String),

    /// Error originating from the Slint platform abstraction.
    #[error("Slint platform error: {0}")]
    SlintPlatformError(#[from] slint::PlatformError),

    /// Error setting global Slint platform.
    #[error("Set platform error: {0}")]
    SetPlatformError(#[from] slint::platform::SetPlatformError),

    /// Error originating from the Calloop event loop driver.
    #[error("Event loop error: {0}")]
    EventLoopError(String),
}

impl From<LayerShellError> for slint::PlatformError {
    fn from(err: LayerShellError) -> Self {
        slint::PlatformError::Other(err.to_string())
    }
}
