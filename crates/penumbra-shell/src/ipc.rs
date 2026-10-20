//! IPC client helpers for communicating with the Penumbra Compositor.
//!
//! Provides state serialization, command submission, and window tracking
//! matching the protocol defined by `penumbra-comp`.

use std::path::{Path, PathBuf};

pub const SHELL_STATE_FILE: &str = "penumbra-shell-state";
pub const SHELL_CMD_FILE: &str = "penumbra-shell-cmd";
pub const WINDOWS_FILE: &str = "penumbra-windows";

/// Returns `$XDG_RUNTIME_DIR/<name>` (falls back to `/tmp`).
pub fn runtime_path(name: &str) -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join(name)
}

/// Navigation mode of the phone output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PhoneMode {
    #[default]
    Home,
    App,
    Overview,
}

/// Snapshot of shell UI state relevant to compositing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShellState {
    pub mode: PhoneMode,
    pub drawer_open: bool,
    pub overview_index: usize,
}

impl ShellState {
    pub fn parse(text: &str) -> Self {
        let mut s = ShellState::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match (k.trim(), v.trim()) {
                ("mode", "app") => s.mode = PhoneMode::App,
                ("mode", "overview") => s.mode = PhoneMode::Overview,
                ("mode", _) => s.mode = PhoneMode::Home,
                ("drawer", v) => s.drawer_open = v == "1" || v == "true",
                ("overview_index", v) => s.overview_index = v.parse().unwrap_or(0),
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        let mode = match self.mode {
            PhoneMode::Home => "home",
            PhoneMode::App => "app",
            PhoneMode::Overview => "overview",
        };
        format!(
            "mode={}\ndrawer={}\noverview_index={}\n",
            mode,
            if self.drawer_open { 1 } else { 0 },
            self.overview_index
        )
    }

    pub fn load() -> Self {
        let path = runtime_path(SHELL_STATE_FILE);
        std::fs::read_to_string(&path)
            .ok()
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = runtime_path(SHELL_STATE_FILE);
        write_atomic(&path, &self.serialize())
    }
}

/// Atomically writes `contents` to `path` via a temporary file.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Appends a one-shot command line to `penumbra-shell-cmd`.
pub fn send_command(cmd: &str) -> std::io::Result<()> {
    use std::io::Write;
    let path = runtime_path(SHELL_CMD_FILE);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", cmd)
}

/// Represents a running task listed in `penumbra-windows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u32,
    pub app_id: String,
    pub title: String,
}

/// Reads the current list of running tasks from `penumbra-windows`.
pub fn read_windows() -> Vec<WindowInfo> {
    let path = runtime_path(WINDOWS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let id = parts.next()?.parse().ok()?;
            let app_id = parts.next()?.to_string();
            let title = parts.next()?.to_string();
            Some(WindowInfo { id, app_id, title })
        })
        .collect()
}
