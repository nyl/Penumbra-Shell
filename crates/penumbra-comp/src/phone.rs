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

/// Logical mobile UI canvas used by the shell and app layout (phone design size).
/// The native backend scales this canvas onto the physical panel.
pub const PHONE_W: i32 = 412;
/// Logical mobile UI canvas height.
pub const PHONE_H: i32 = 915;
/// Physical panel size used when no compositor-provided mode is available.
pub const NATIVE_OUTPUT_W: u32 = 1080;
pub const NATIVE_OUTPUT_H: u32 = 2340;
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
pub const OVERVIEW_SCALE: f64 = 0.67;
/// Overview card width (412 * 0.67, rounded).
pub const CARD_W: i32 = 276;
/// Overview card height (841 * 0.67, rounded).
pub const CARD_H: i32 = 563;
/// Overview card top edge.
pub const CARD_Y: i32 = 176;
/// Horizontal distance between adjacent cards.
pub const CARD_STEP: i32 = 298;

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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellState {
    pub mode: PhoneMode,
    pub drawer_open: bool,
    pub drawer_pull_dy: f64,
    pub drawer_dragging: bool,
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
                ("drawer_dragging", v) => s.drawer_dragging = v == "1" || v == "true",
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
            "mode={}\ndrawer={}\ndrawer_pull_dy={:.1}\ndrawer_dragging={}\npower_dialog={}\nvolume_modal={}\nvolume_level={:.2}\nnav_gesture_active={}\nnav_gesture_dy={:.1}\nnav_gesture_progress={:.3}\nnav_gesture_vy={:.1}\nnav_gesture_dx={:.1}\nnav_gesture_vx={:.1}\nnav_switch_active={}\noverview_index={}\noverview_scroll={:.3}\nis_dragging={}\nactive_card_dy={:.1}\n",
            mode,
            if self.drawer_open { 1 } else { 0 },
            self.drawer_pull_dy,
            if self.drawer_dragging { 1 } else { 0 },
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
}

/// One-shot task command issued by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommand {
    Focus(u32),
    Close(u32),
    CloseAll,
    Back,
    PowerDialog,
    ScreenToggle,
}

pub fn parse_commands(text: &str) -> Vec<ShellCommand> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next()?, parts.next()) {
                ("focus", Some(id)) => id.parse().ok().map(ShellCommand::Focus),
                ("close", Some(id)) => id.parse().ok().map(ShellCommand::Close),
                ("close-all", _) => Some(ShellCommand::CloseAll),
                ("back", _) => Some(ShellCommand::Back),
                ("power-dialog", _) => Some(ShellCommand::PowerDialog),
                ("screen-toggle", _) => Some(ShellCommand::ScreenToggle),
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

/// Computes the exact (x, y, w, h, scale) transform for an overview card.
/// All cards render at the same OVERVIEW_SCALE (flat Material 3 layout, no 3D parallax).
pub fn card_transform(
    index: usize,
    scroll: f64,
    active_index: usize,
    active_card_dy: f64,
) -> (i32, i32, i32, i32, f64) {
    let dist = (index as f64) - scroll;
    let scale = OVERVIEW_SCALE;
    let w = CARD_W;
    let h = CARD_H;
    let base_centered_x = ((PHONE_W - w) / 2) as f64;
    let target_x = base_centered_x - (dist * CARD_STEP as f64);
    let x = target_x.round() as i32;
    let dy = if index == active_index {
        active_card_dy
    } else {
        0.0
    };
    let y = (CARD_Y as f64 + dy).round() as i32;
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

/// Calculates the uniform output scale factor that maps the logical mobile shell
/// (412x915) onto the physical panel after a native DRM mode has been chosen.
/// The logical shell layout stays fixed while the physical output size drives the scale.
pub fn native_output_scale(physical: (u32, u32), logical: (u32, u32)) -> f64 {
    let physical_w = physical.0 as f64;
    let physical_h = physical.1 as f64;
    let logical_w = logical.0 as f64;
    let logical_h = logical.1 as f64;
    let scale_w = if logical_w > 0.0 {
        physical_w / logical_w
    } else {
        1.0
    };
    let scale_h = if logical_h > 0.0 {
        physical_h / logical_h
    } else {
        1.0
    };
    scale_w.min(scale_h)
}

/// Logical-space offset that centers the scaled logical canvas on the physical panel.
pub fn letterbox_offset(physical: (u32, u32), logical: (u32, u32), scale: f64) -> (i32, i32) {
    if scale <= 0.0 {
        return (0, 0);
    }
    let dx = (physical.0 as f64 / scale - logical.0 as f64) / 2.0;
    let dy = (physical.1 as f64 / scale - logical.1 as f64) / 2.0;
    (dx.round() as i32, dy.round() as i32)
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
            drawer_dragging: true,
            overview_index: 2,
            overview_scroll: 2.0,
            is_dragging: false,
            active_card_dy: 0.0,
            power_dialog_open: false,
            volume_modal_open: false,
            volume_level: 0.65,
            nav_gesture_active: true,
            nav_gesture_progress: 0.42,
            nav_gesture_dy: -120.0,
            nav_gesture_vy: -550.0,
            nav_gesture_dx: 35.0,
            nav_gesture_vx: 120.0,
            nav_switch_active: true,
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
        let center = ((PHONE_W - CARD_W) / 2) as f64;
        let expected_center = center.round() as i32;
        assert_eq!(card_x(0, 0.0), expected_center);
        assert_eq!(card_x(1, 0.0), expected_center - CARD_STEP);
        assert_eq!(card_x(1, 1.0), expected_center);
    }

    #[test]
    fn letterbox_centers_logical_canvas() {
        assert_eq!(letterbox_offset((412, 915), (412, 915), 1.0), (0, 0));
        // Panel 1080x2340: height limits the scale, width is centered (pillarbox).
        let scale = native_output_scale((NATIVE_OUTPUT_W, NATIVE_OUTPUT_H), (PHONE_W as u32, PHONE_H as u32));
        assert!((scale - 2.557).abs() < 0.01, "scale={scale}");
        assert_eq!(
            letterbox_offset((NATIVE_OUTPUT_W, NATIVE_OUTPUT_H), (PHONE_W as u32, PHONE_H as u32), scale),
            (5, 0)
        );
        // Short wide output: height limits the scale, width is centered.
        let scale = native_output_scale((1080, 1000), (412, 915));
        assert_eq!(letterbox_offset((1080, 1000), (412, 915), scale), (288, 0));
    }

    #[test]
    fn test_resolve_app_icon_target() {
        assert_eq!(resolve_app_icon_target("org.gnome.Calls"), (75, 820));
        assert_eq!(resolve_app_icon_target("sm.puri.Chatty"), (162, 820));
        assert_eq!(resolve_app_icon_target("org.gnome.Epiphany"), (249, 820));
        assert_eq!(resolve_app_icon_target("org.gnome.Console"), (336, 820));
        // Fallback to dock center
        assert_eq!(resolve_app_icon_target("org.random.UnknownApp"), (206, 820));
    }
}

/// Resolves destination icon center coordinate $(X, Y)$ on the homescreen for App-to-Home dismiss.
///
/// Matches known dock positions, or falls back to dock center $(206, 820)$.
pub fn resolve_app_icon_target(app_id: &str) -> (i32, i32) {
    let dock_apps = [
        "org.gnome.Calls",
        "sm.puri.Chatty",
        "org.gnome.Epiphany",
        "org.gnome.Console",
    ];
    let dock_xs = [75, 162, 249, 336];
    let dock_y = 820;

    for (i, &dock_id) in dock_apps.iter().enumerate() {
        if app_id.eq_ignore_ascii_case(dock_id)
            || app_id.to_lowercase().contains(&dock_id.to_lowercase())
            || (dock_id.contains("Calls") && app_id.to_lowercase().contains("call"))
            || (dock_id.contains("Chatty") && app_id.to_lowercase().contains("chatt"))
            || (dock_id.contains("Epiphany") && (app_id.to_lowercase().contains("browser") || app_id.to_lowercase().contains("epiphany")))
            || (dock_id.contains("Console") && (app_id.to_lowercase().contains("terminal") || app_id.to_lowercase().contains("console")))
        {
            return (dock_xs[i], dock_y);
        }
    }

    (206, 820)
}
