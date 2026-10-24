//! # KDE-Compliant Virtual Keyboard & Physical Keyboard Hardware Detection
//!
//! Provides discovery for both Wayland on-screen virtual keyboards and physical
//! hardware input devices.
//!
//! ## KDE Virtual Keyboard Detection Standard
//! In KDE Plasma / KWin Wayland:
//! 1. Virtual keyboards are installed as desktop entry files into standard XDG directories
//!    (`/usr/share/applications/`, `~/.local/share/applications/`, etc.).
//! 2. They declare `X-KDE-Wayland-VirtualKeyboard=true` inside their `[Desktop Entry]` section.
//! 3. They are typically hidden from general application launchers using `NoDisplay=true`.
//!
//! This module scans XDG application directories, parses matching desktop entries,
//! and presents all compliant virtual keyboards.
//!
//! ## Physical Keyboard Detection
//! Reads `/proc/bus/input/devices`, filters out virtual devices (bus `0006` and uinput nodes),
//! and inspects the `B: KEY` bitmask to find physical devices with alphanumeric keys (`KEY_A`, `KEY_SPACE`).

use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Metadata representing an installed KDE-compliant virtual keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualKeyboardEntry {
    /// Human-friendly display name (e.g., "Penumbra Keyboard", "Maliit Keyboard").
    pub name: String,
    /// Detailed description or summary.
    pub comment: String,
    /// Desktop file identifier (e.g., "penumbra-keyboard.desktop", "org.maliit.keyboard.desktop").
    pub desktop_id: String,
    /// Absolute or binary command line executed to spawn the keyboard.
    pub exec: String,
    /// Freedesktop icon theme identifier.
    pub icon: String,
    /// Whether this keyboard is the active system default.
    pub is_active: bool,
}

/// Metadata representing an active physical hardware keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalKeyboardEntry {
    /// Device model or hardware description.
    pub name: String,
    /// Bus type (e.g. "USB", "Bluetooth", "PS/2").
    pub bus: String,
    /// Device sysfs path.
    pub sysfs: String,
}

/// Returns the list of standard directories to search for `.desktop` files.
fn get_application_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. User local applications
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }

    // 2. Custom XDG directories
    if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(xdg_data_home).join("applications"));
    }
    if let Ok(xdg_dirs) = std::env::var("XDG_DATA_DIRS") {
        for dir in xdg_dirs.split(':') {
            if !dir.trim().is_empty() {
                dirs.push(PathBuf::from(dir.trim()).join("applications"));
            }
        }
    }

    // 3. System application paths
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));

    // 4. Local repository data directory (for local testing/development)
    if let Ok(cwd) = std::env::current_dir() {
        let local_data = cwd.join("data");
        if local_data.is_dir() {
            dirs.push(local_data);
        }
    }

    dirs
}

/// Returns the path to the user's Penumbra keyboard configuration file.
pub fn keyboard_config_path() -> PathBuf {
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(config_home).join("penumbra/keyboard.conf")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/penumbra/keyboard.conf")
    } else {
        PathBuf::from("/tmp/penumbra-keyboard.conf")
    }
}

/// Reads the currently configured active virtual keyboard desktop ID.
pub fn get_active_virtual_keyboard_id() -> String {
    let path = keyboard_config_path();
    if let Ok(content) = fs::read_to_string(path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(val) = trimmed.strip_prefix("active_desktop_id=") {
                return val.trim().to_string();
            }
        }
    }
    "penumbra-keyboard.desktop".to_string()
}

/// Switches the active virtual keyboard by writing configuration, terminating previous
/// keyboard instances, and launching the chosen keyboard daemon.
pub fn switch_active_virtual_keyboard(desktop_id: &str) {
    let all = detect_virtual_keyboards();
    let Some(target) = all.into_iter().find(|k| k.desktop_id == desktop_id) else {
        info!("No virtual keyboard found matching desktop ID '{}'", desktop_id);
        return;
    };

    let path = keyboard_config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let config_content = format!(
        "[Keyboard]\nactive_desktop_id={}\ncommand={}\nname={}\n",
        target.desktop_id, target.exec, target.name
    );
    if let Err(err) = fs::write(&path, config_content) {
        info!("Failed to write keyboard config to {:?}: {:?}", path, err);
    } else {
        info!("Saved active virtual keyboard configuration: {}", target.desktop_id);
    }

    // Terminate existing running virtual keyboard instances
    let _ = std::process::Command::new("pkill").arg("-f").arg("penumbra-keyboard").status();
    let _ = std::process::Command::new("pkill").arg("-f").arg("squeekboard").status();
    let _ = std::process::Command::new("pkill").arg("-f").arg("maliit-keyboard").status();
    let _ = std::process::Command::new("pkill").arg("-f").arg("wvkbd").status();

    // Spawn the newly selected virtual keyboard daemon in the background
    let exec_cmd = target.exec.split_whitespace().next().unwrap_or(&target.exec);
    match std::process::Command::new(exec_cmd).spawn() {
        Ok(child) => {
            info!("Spawned virtual keyboard daemon '{}' (PID {})", exec_cmd, child.id());
        }
        Err(err) => {
            info!("Failed to spawn virtual keyboard '{}': {:?}", exec_cmd, err);
        }
    }
}

/// Parses a single `.desktop` file and returns a [`VirtualKeyboardEntry`] if it
/// adheres to the KDE `X-KDE-Wayland-VirtualKeyboard=true` standard or standard Wayland OSK naming.
fn parse_virtual_keyboard_desktop_file(path: &Path) -> Option<VirtualKeyboardEntry> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut is_virtual_keyboard = false;
    let mut name = String::new();
    let mut comment = String::new();
    let mut exec = String::new();
    let mut icon = "input-keyboard-virtual".to_string();

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    if file_name.contains("squeekboard")
        || file_name.contains("maliit")
        || file_name.contains("wvkbd")
        || file_name.contains("penumbra-keyboard")
    {
        is_virtual_keyboard = true;
    }

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        if trimmed == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        } else if trimmed.starts_with('[') {
            in_desktop_entry = false;
            continue;
        }

        if in_desktop_entry {
            if let Some((k, v)) = trimmed.split_once('=') {
                let key = k.trim();
                let val = v.trim();
                match key {
                    "X-KDE-Wayland-VirtualKeyboard" => {
                        if val.eq_ignore_ascii_case("true") || val == "1" {
                            is_virtual_keyboard = true;
                        }
                    }
                    "Categories" => {
                        if val.contains("InputMethod")
                            || val.contains("VirtualKeyboard")
                            || (val.contains("Accessibility") && val.contains("Utility"))
                        {
                            is_virtual_keyboard = true;
                        }
                    }
                    "Name" if name.is_empty() => {
                        name = val.to_string();
                    }
                    "Comment" if comment.is_empty() => {
                        comment = val.to_string();
                    }
                    "Exec" if exec.is_empty() => {
                        exec = val.to_string();
                    }
                    "Icon" if !val.is_empty() => {
                        icon = val.to_string();
                    }
                    _ => {}
                }
            }
        }
    }

    if is_virtual_keyboard && !exec.is_empty() {
        let desktop_id = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.desktop")
            .to_string();

        if name.is_empty() {
            name = desktop_id.trim_end_matches(".desktop").replace(['-', '_'], " ");
        }

        Some(VirtualKeyboardEntry {
            name,
            comment,
            desktop_id,
            exec,
            icon,
            is_active: false,
        })
    } else {
        None
    }
}

/// Discovers all virtual keyboards installed across the system matching KDE / Phosh / Wayland standards.
pub fn detect_virtual_keyboards() -> Vec<VirtualKeyboardEntry> {
    let mut entries = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for dir in get_application_search_dirs() {
        if !dir.is_dir() {
            continue;
        }

        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                if let Some(vk) = parse_virtual_keyboard_desktop_file(&path) {
                    if seen_ids.insert(vk.desktop_id.clone()) {
                        info!(
                            "Detected Wayland/KDE virtual keyboard: '{}' ({})",
                            vk.name, vk.desktop_id
                        );
                        entries.push(vk);
                    }
                }
            }
        }
    }

    // Also inspect standard system binaries for 3rd party keyboards in case their .desktop file
    // is in a non-standard path or packaged without one:
    let known_binaries = [
        ("squeekboard", "squeekboard.desktop", "Squeekboard", "Purism Phosh On-Screen Virtual Keyboard"),
        ("maliit-keyboard", "org.maliit.keyboard.desktop", "Maliit Keyboard", "KDE Plasma Mobile Virtual Keyboard"),
        ("wvkbd-mobintl", "wvkbd-mobintl.desktop", "wvkbd", "Lightweight Wayland Virtual Keyboard"),
        ("wvkbd", "wvkbd.desktop", "wvkbd", "Lightweight Wayland Virtual Keyboard"),
    ];

    for (bin, desktop_id, display_name, comment) in known_binaries {
        if !seen_ids.contains(desktop_id) {
            let found_path = ["/usr/bin", "/usr/local/bin"]
                .iter()
                .map(|p| Path::new(p).join(bin))
                .find(|p| p.is_file());

            if let Some(bin_path) = found_path {
                seen_ids.insert(desktop_id.to_string());
                info!("Discovered installed system virtual keyboard binary: {:?}", bin_path);
                entries.push(VirtualKeyboardEntry {
                    name: display_name.to_string(),
                    comment: comment.to_string(),
                    desktop_id: desktop_id.to_string(),
                    exec: bin_path.to_string_lossy().to_string(),
                    icon: "input-keyboard-virtual".to_string(),
                    is_active: false,
                });
            }
        }
    }

    // Default fallback: Always ensure Penumbra Keyboard is selectable
    if !seen_ids.contains("penumbra-keyboard.desktop") {
        entries.push(VirtualKeyboardEntry {
            name: "Penumbra Keyboard".to_string(),
            comment: "Penumbra On-Screen Virtual Keyboard".to_string(),
            desktop_id: "penumbra-keyboard.desktop".to_string(),
            exec: "/usr/local/bin/penumbra-keyboard".to_string(),
            icon: "input-keyboard-virtual".to_string(),
            is_active: false,
        });
    }

    // Determine active keyboard from user configuration (defaulting to penumbra-keyboard.desktop)
    let active_id = get_active_virtual_keyboard_id();
    let mut has_active = false;
    for item in &mut entries {
        if item.desktop_id == active_id {
            item.is_active = true;
            has_active = true;
        } else {
            item.is_active = false;
        }
    }

    // Fallback if configured ID wasn't found in discovered list
    if !has_active {
        if let Some(first) = entries.first_mut() {
            first.is_active = true;
        }
    }

    entries
}

/// Detects all active physical hardware keyboards.
pub fn detect_physical_keyboards() -> Vec<PhysicalKeyboardEntry> {
    let content = match fs::read_to_string("/proc/bus/input/devices") {
        Ok(c) => c,
        Err(err) => {
            debug!("Unable to read /proc/bus/input/devices: {:?}", err);
            return Vec::new();
        }
    };

    let mut keyboards = Vec::new();

    for block in content.split("\n\n") {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut bus = String::new();
        let mut name = String::new();
        let mut phys = String::new();
        let mut sysfs = String::new();
        let mut handlers = String::new();
        let mut ev_mask = 0u64;
        let mut key_words: Vec<u64> = Vec::new();

        for line in trimmed.lines() {
            if let Some(rest) = line.strip_prefix("I: ") {
                for token in rest.split_whitespace() {
                    if let Some(val) = token.strip_prefix("Bus=") {
                        bus = match val {
                            "0003" => "USB".to_string(),
                            "0005" => "Bluetooth".to_string(),
                            "0011" => "PS/2".to_string(),
                            "0018" => "I2C".to_string(),
                            "0019" => "Platform".to_string(),
                            other => format!("Bus {}", other),
                        };
                    }
                }
            } else if let Some(rest) = line.strip_prefix("N: Name=") {
                name = rest.trim_matches('"').to_string();
            } else if let Some(rest) = line.strip_prefix("P: Phys=") {
                phys = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("S: Sysfs=") {
                sysfs = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
                handlers = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("B: EV=") {
                ev_mask = u64::from_str_radix(rest.trim(), 16).unwrap_or(0);
            } else if let Some(rest) = line.strip_prefix("B: KEY=") {
                key_words = rest
                    .split_whitespace()
                    .filter_map(|w| u64::from_str_radix(w, 16).ok())
                    .collect();
            }
        }

        // Exclude virtual/uinput devices
        if bus.contains("0006") {
            continue;
        }
        let name_lower = name.to_lowercase();
        if name_lower.contains("uinput")
            || name_lower.contains("penumbra")
            || name_lower.contains("virtual")
            || name_lower.contains("ydotool")
        {
            continue;
        }
        if phys.to_lowercase().contains("uinput") {
            continue;
        }

        // Require kbd handler and EV_KEY bit
        if !handlers.split_whitespace().any(|h| h == "kbd") {
            continue;
        }
        if (ev_mask & 0x2) == 0 {
            continue;
        }

        // Require alphanumeric keys (KEY_ENTER=28, KEY_A=30, KEY_SPACE=57 in word 0)
        if let Some(&word0) = key_words.last() {
            let has_enter = (word0 & (1u64 << 28)) != 0;
            let has_a = (word0 & (1u64 << 30)) != 0;
            let has_space = (word0 & (1u64 << 57)) != 0;

            if has_enter || (has_a && has_space) {
                keyboards.push(PhysicalKeyboardEntry { name, bus, sysfs });
            }
        }
    }

    keyboards
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_keyboard_detection() {
        let vks = detect_virtual_keyboards();
        assert!(!vks.is_empty(), "Should discover at least Penumbra Keyboard");
        assert!(vks.iter().any(|v| v.desktop_id == "penumbra-keyboard.desktop"));
    }
}
