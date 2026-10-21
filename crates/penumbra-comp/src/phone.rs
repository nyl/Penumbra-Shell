//! Phone output (DSI-1) layout constants, overview geometry, and shell IPC.
//!
//! The shell (`penumbra-shell`) and the compositor communicate through small
//! files in `$XDG_RUNTIME_DIR`:
//!
//! * `penumbra-shell-state`  (shell -> comp, overwritten): current phone mode,
//!   quick settings drawer state, and selected overview card.
//! * `penumbra-shell-cmd`    (shell -> comp, appended, consumed by comp):
//!   one-shot task commands (`focus <id>`, `close <id>`, `close-all`).
//! * `penumbra-windows`      (comp -> shell, overwritten): running phone tasks
//!   in MRU order, one `id\tapp_id\ttitle` per line. First line = active task.

use std::path::PathBuf;

/// Phone display width.
pub const PHONE_W: i32 = 412;
/// Phone display height.
pub const PHONE_H: i32 = 915;
/// Persistent top status bar height (`y: 0..42`).
pub const STATUS_BAR_H: i32 = 42;
/// Persistent bottom gesture bar height (`y: 883..915`).
pub const NAV_BAR_H: i32 = 32;
/// Application inset origin Y.
pub const APP_Y: i32 = STATUS_BAR_H;
/// Application inset height (915 - 42 - 32 = 841).
pub const APP_H: i32 = PHONE_H - STATUS_BAR_H - NAV_BAR_H;
/// Y coordinate where the bottom gesture bar starts (883).
pub const NAV_BAR_Y: i32 = PHONE_H - NAV_BAR_H;

/// Scale applied to app surfaces when shown as overview cards.
pub const OVERVIEW_SCALE: f64 = 0.78;
/// Overview card width (412 * 0.78, rounded).
pub const CARD_W: i32 = 321;
/// Overview card height (841 * 0.78, rounded).
pub const CARD_H: i32 = 656;
/// Overview card top edge.
pub const CARD_Y: i32 = APP_Y + 70;
/// Horizontal distance between adjacent cards (321px width + 20px gap).
pub const CARD_STEP: i32 = 341;

pub const SHELL_STATE_FILE: &str = "penumbra-shell-state";
pub const SHELL_CMD_FILE: &str = "penumbra-shell-cmd";
pub const WINDOWS_FILE: &str = "penumbra-windows";

/// Returns `$XDG_RUNTIME_DIR/<name>` (falls back to `/tmp`).
pub fn runtime_path(name: &str) -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join(name)
}

/// Navigation state of the phone output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PhoneMode {
    /// Home screen / launcher visible, apps hidden.
    #[default]
    Home,
    /// Active application shown inside the system insets.
    App,
    /// Task switcher overlay with horizontal cards.
    Overview,
}

/// Snapshot of shell UI state relevant to compositing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ShellState {
    pub mode: PhoneMode,
    pub drawer_open: bool,
    pub drawer_pull_dy: f64,
    pub overview_index: usize,
    pub overview_scroll: f64,
    pub is_dragging: bool,
    pub active_card_dy: f64,
}

impl ShellState {
    /// Parses `key=value` lines. Unknown keys are ignored.
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
            "mode={}\ndrawer={}\ndrawer_pull_dy={:.1}\noverview_index={}\noverview_scroll={:.3}\nis_dragging={}\nactive_card_dy={:.1}\n",
            mode,
            if self.drawer_open { 1 } else { 0 },
            self.drawer_pull_dy,
            self.overview_index,
            self.overview_scroll,
            if self.is_dragging { 1 } else { 0 },
            self.active_card_dy
        )
    }
}

/// One-shot task command issued by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommand {
    Focus(u32),
    Close(u32),
    CloseAll,
}

pub fn parse_commands(text: &str) -> Vec<ShellCommand> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next()?, parts.next()) {
                ("focus", Some(id)) => id.parse().ok().map(ShellCommand::Focus),
                ("close", Some(id)) => id.parse().ok().map(ShellCommand::Close),
                ("close-all", _) => Some(ShellCommand::CloseAll),
                _ => None,
            }
        })
        .collect()
}

/// Left edge of overview card `index` given the (animated) carousel scroll.
/// Older tasks (higher index) sit to the left of the active one, like Android.
pub fn card_x(index: usize, scroll: f64) -> i32 {
    let center = ((PHONE_W - CARD_W) / 2) as f64;
    (center - (index as f64 - scroll) * CARD_STEP as f64).round() as i32
}

/// Computes the exact (x, y, w, h, scale) transform for an overview card,
/// applying progressive parallax scaling and vertical dismissal translation.
pub fn card_transform(
    index: usize,
    scroll: f64,
    active_index: usize,
    active_card_dy: f64,
) -> (i32, i32, i32, i32, f64) {
    let dist = (index as f64) - scroll;
    let abs_dist = dist.abs();
    // Parallax progressive scaling: Center 1.0, Left/Right neighbors 0.90, Outer cards down to 0.82
    let card_scale = if abs_dist <= 1.0 {
        1.0 - (abs_dist * 0.10)
    } else {
        (0.90 - ((abs_dist - 1.0) * 0.08)).max(0.82)
    };
    let scale = OVERVIEW_SCALE * card_scale;
    let w = (CARD_W as f64 * card_scale).round() as i32;
    let h = (CARD_H as f64 * card_scale).round() as i32;
    let base_centered_x = ((PHONE_W - w) / 2) as f64;
    let target_x = base_centered_x - (dist * CARD_STEP as f64);
    let x = target_x.round() as i32;
    let dy = if index == active_index {
        active_card_dy
    } else {
        0.0
    };
    let base_centered_y = ((PHONE_H - h) / 2) as f64;
    let target_y = base_centered_y + dy;
    let y = target_y.round() as i32;
    (x, y, w, h, scale)
}

/// Serializes a task list line, stripping separators from user-controlled strings.
pub fn task_line(id: u32, app_id: &str, title: &str) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    format!("{}\t{}\t{}\n", id, clean(app_id), clean(title))
}

/// Atomically writes `contents` to `path` (write temp file + rename).
pub fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inset_geometry() {
        assert_eq!(APP_Y, 42);
        assert_eq!(APP_H, 841);
        assert_eq!(NAV_BAR_Y, 883);
        assert_eq!(APP_Y + APP_H, NAV_BAR_Y);
    }

    #[test]
    fn shell_state_roundtrip() {
        let s = ShellState {
            mode: PhoneMode::Overview,
            drawer_open: true,
            drawer_pull_dy: 45.0,
            overview_index: 2,
            overview_scroll: 2.0,
            is_dragging: false,
            active_card_dy: 0.0,
        };
        assert_eq!(ShellState::parse(&s.serialize()), s);
        assert_eq!(ShellState::parse("garbage"), ShellState::default());
    }

    #[test]
    fn commands_parse() {
        assert_eq!(
            parse_commands("focus 3\nclose 7\nclose-all\nbogus 1\nfocus x\n"),
            vec![
                ShellCommand::Focus(3),
                ShellCommand::Close(7),
                ShellCommand::CloseAll
            ]
        );
    }

    #[test]
    fn overview_cards_layout() {
        // Active card centered: (412 - 321) / 2 = 45.5 -> round = 46 (or 45)
        let center = ((PHONE_W - CARD_W) / 2) as f64;
        let expected_center = center.round() as i32;
        assert_eq!(card_x(0, 0.0), expected_center);
        assert_eq!(card_x(1, 0.0), expected_center - CARD_STEP);
        // Scrolling to index 1 centers it
        assert_eq!(card_x(1, 1.0), expected_center);
    }
}
