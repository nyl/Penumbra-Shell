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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellState {
    pub mode: PhoneMode,
    pub drawer_open: bool,
    pub drawer_pull_dy: f64,
    pub drawer_dragging: bool,
    pub drawer_progress: f64,
    pub overview_index: usize,
    pub overview_scroll: f64,
    pub is_dragging: bool,
    pub active_card_dy: f64,
    pub power_dialog_open: bool,
    pub volume_modal_open: bool,
    pub volume_level: f32,
    pub nav_gesture_active: bool,
    pub nav_gesture_dy: f64,
    pub nav_gesture_progress: f64,
    pub nav_gesture_vy: f64,
    pub nav_gesture_dx: f64,
    pub nav_gesture_vx: f64,
    pub nav_switch_active: bool,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            mode: PhoneMode::Home,
            drawer_open: false,
            drawer_pull_dy: 0.0,
            drawer_dragging: false,
            drawer_progress: 0.0,
            overview_index: 0,
            overview_scroll: 0.0,
            is_dragging: false,
            active_card_dy: 0.0,
            power_dialog_open: false,
            volume_modal_open: false,
            volume_level: 0.65,
            nav_gesture_active: false,
            nav_gesture_dy: 0.0,
            nav_gesture_progress: 0.0,
            nav_gesture_vy: 0.0,
            nav_gesture_dx: 0.0,
            nav_gesture_vx: 0.0,
            nav_switch_active: false,
        }
    }
}

impl ShellState {
    pub fn parse(text: &str) -> Self {
        let mut s = ShellState::default();
        let mut has_scroll = false;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match (k.trim(), v.trim()) {
                ("mode", "app") => s.mode = PhoneMode::App,
                ("mode", "overview") => s.mode = PhoneMode::Overview,
                ("mode", _) => s.mode = PhoneMode::Home,
                ("drawer", v) => s.drawer_open = v == "1" || v == "true",
                ("drawer_pull_dy", v) => s.drawer_pull_dy = v.parse().unwrap_or(0.0),
                ("drawer_dragging", v) => s.drawer_dragging = v == "1" || v == "true",
                ("drawer_progress", v) => s.drawer_progress = v.parse().unwrap_or(0.0),
                ("power_dialog", v) => s.power_dialog_open = v == "1" || v == "true",
                ("volume_modal", v) => s.volume_modal_open = v == "1" || v == "true",
                ("volume_level", v) => s.volume_level = v.parse().unwrap_or(0.65),
                ("nav_gesture_active", v) => s.nav_gesture_active = v == "1" || v == "true",
                ("nav_gesture_dy", v) => s.nav_gesture_dy = v.parse().unwrap_or(0.0),
                ("nav_gesture_progress", v) => s.nav_gesture_progress = v.parse().unwrap_or(0.0),
                ("nav_gesture_vy", v) => s.nav_gesture_vy = v.parse().unwrap_or(0.0),
                ("nav_gesture_dx", v) => s.nav_gesture_dx = v.parse().unwrap_or(0.0),
                ("nav_gesture_vx", v) => s.nav_gesture_vx = v.parse().unwrap_or(0.0),
                ("nav_switch_active", v) => s.nav_switch_active = v == "1" || v == "true",
                ("overview_index", v) => s.overview_index = v.parse().unwrap_or(0),
                ("overview_scroll", v) => {
                    s.overview_scroll = v.parse().unwrap_or(0.0);
                    has_scroll = true;
                }
                ("is_dragging", v) => s.is_dragging = v == "1" || v == "true",
                ("active_card_dy", v) => s.active_card_dy = v.parse().unwrap_or(0.0),
                _ => {}
            }
        }
        if !has_scroll {
            s.overview_scroll = s.overview_index as f64;
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
            "mode={}\ndrawer={}\ndrawer_pull_dy={:.1}\ndrawer_dragging={}\ndrawer_progress={:.3}\npower_dialog={}\nvolume_modal={}\nvolume_level={:.2}\nnav_gesture_active={}\nnav_gesture_dy={:.1}\nnav_gesture_progress={:.3}\nnav_gesture_vy={:.1}\nnav_gesture_dx={:.1}\nnav_gesture_vx={:.1}\nnav_switch_active={}\noverview_index={}\noverview_scroll={:.3}\nis_dragging={}\nactive_card_dy={:.1}\n",
            mode,
            if self.drawer_open { 1 } else { 0 },
            self.drawer_pull_dy,
            if self.drawer_dragging { 1 } else { 0 },
            self.drawer_progress,
            if self.power_dialog_open { 1 } else { 0 },
            if self.volume_modal_open { 1 } else { 0 },
            self.volume_level,
            if self.nav_gesture_active { 1 } else { 0 },
            self.nav_gesture_dy,
            self.nav_gesture_progress,
            self.nav_gesture_vy,
            self.nav_gesture_dx,
            self.nav_gesture_vx,
            if self.nav_switch_active { 1 } else { 0 },
            self.overview_index,
            self.overview_scroll,
            if self.is_dragging { 1 } else { 0 },
            self.active_card_dy
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
