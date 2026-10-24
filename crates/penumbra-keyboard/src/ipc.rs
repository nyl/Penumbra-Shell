//! # Inter-Process Command Bridge for Penumbra Keyboard
//!
//! Listens for runtime IPC commands (`show`, `hide`, `toggle`) delivered via
//! `$XDG_RUNTIME_DIR/penumbra-keyboard-cmd`.

use std::fs;
use std::path::PathBuf;
use tracing::info;

pub const KEYBOARD_CMD_FILE: &str = "penumbra-keyboard-cmd";
pub const KEYBOARD_STATE_FILE: &str = "penumbra-keyboard-state";

/// Returns `$XDG_RUNTIME_DIR/<name>` (falls back to `/tmp/<name>`).
pub fn runtime_path(name: &str) -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join(name)
}

/// Atomically persists current open/closed status for compositor hit testing & rendering.
pub fn write_keyboard_state(is_open: bool) {
    let path = runtime_path(KEYBOARD_STATE_FILE);
    let tmp = path.with_extension("tmp");
    let val = if is_open { "open=1\n" } else { "open=0\n" };
    if std::fs::write(&tmp, val).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Checks whether Quick Settings drawer, power dialog, or volume modal is currently open.
pub fn is_drawer_or_dialog_open() -> bool {
    let path = runtime_path("penumbra-shell-state");
    if let Ok(content) = fs::read_to_string(path) {
        content.lines().any(|l| {
            l.starts_with("drawer=1")
                || l.starts_with("power_dialog=1")
                || l.starts_with("volume_modal=1")
        })
    } else {
        false
    }
}

/// Supported external control commands for `penumbra-keyboard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardCommand {
    Show,
    Hide,
    Toggle,
}

/// Checks and consumes any pending command from `$XDG_RUNTIME_DIR/penumbra-keyboard-cmd`.
pub fn poll_pending_command() -> Option<KeyboardCommand> {
    let path = runtime_path(KEYBOARD_CMD_FILE);
    if !path.exists() {
        return None;
    }

    let content = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);

    match content.trim().to_lowercase().as_str() {
        "show" | "open" | "1" => {
            info!("Received IPC command: show keyboard");
            Some(KeyboardCommand::Show)
        }
        "hide" | "close" | "0" => {
            info!("Received IPC command: hide keyboard");
            Some(KeyboardCommand::Hide)
        }
        "toggle" => {
            info!("Received IPC command: toggle keyboard");
            Some(KeyboardCommand::Toggle)
        }
        other => {
            info!("Ignoring unknown keyboard command: '{}'", other);
            None
        }
    }
}
