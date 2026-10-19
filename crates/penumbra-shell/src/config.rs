use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomescreenAppEntry {
    pub id: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellLayoutConfig {
    pub dock_apps: Vec<String>,
    pub homescreen_apps: Vec<HomescreenAppEntry>,
}

impl Default for ShellLayoutConfig {
    fn default() -> Self {
        Self {
            dock_apps: vec![
                "org.gnome.Calls".to_string(),
                "sm.puri.Chatty".to_string(),
                "org.gnome.Epiphany".to_string(),
                "org.gnome.Console".to_string(),
            ],
            homescreen_apps: Vec::new(),
        }
    }
}

pub fn get_layout_config_path() -> PathBuf {
    let base = dirs_next().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/nyl".to_string());
        PathBuf::from(home).join(".config")
    });
    base.join("penumbra").join("layout.json")
}

fn dirs_next() -> Option<PathBuf> {
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        if !config_home.is_empty() {
            return Some(PathBuf::from(config_home));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(PathBuf::from(home).join(".config"));
    }
    None
}

/// Loads the shell layout from `~/.config/penumbra/layout.json`.
/// If not present or invalid, creates default configuration using fallback GNOME Mobile app IDs.
pub fn load_layout_config(available_app_ids: &[String]) -> ShellLayoutConfig {
    let path = get_layout_config_path();
    if path.exists() {
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str::<ShellLayoutConfig>(&data) {
                info!("Loaded shell layout config from {:?}", path);
                return config;
            } else {
                warn!("Failed to parse layout config at {:?}, falling back to default", path);
            }
        }
    }

    let config = create_default_layout(available_app_ids);
    save_layout_config(&config);
    config
}

pub fn create_default_layout(available_app_ids: &[String]) -> ShellLayoutConfig {
    let mut default_dock = Vec::new();
    let preferred_slots = [
        &["org.gnome.Calls", "calls", "dialer"][..],
        &["sm.puri.Chatty", "chatty", "org.gnome.Polari"][..],
        &["org.gnome.Epiphany", "epiphany", "firefox", "chromium", "google-chrome"][..],
        &["org.gnome.Console", "foot", "org.gnome.Terminal", "alacritty", "kitty", "org.gnome.Camera"][..],
    ];

    for candidates in preferred_slots.iter() {
        let mut matched = false;
        for candidate in candidates.iter() {
            if let Some(found) = available_app_ids.iter().find(|id| {
                id.eq_ignore_ascii_case(candidate)
                    || id.to_lowercase().contains(&candidate.to_lowercase())
            }) {
                default_dock.push(found.clone());
                matched = true;
                break;
            }
        }
        if !matched {
            // Push placeholder/fallback ID
            default_dock.push(candidates[0].to_string());
        }
    }

    // Limit to 4
    default_dock.truncate(4);

    ShellLayoutConfig {
        dock_apps: default_dock,
        homescreen_apps: Vec::new(),
    }
}

pub fn save_layout_config(config: &ShellLayoutConfig) {
    let path = get_layout_config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        if let Err(e) = fs::write(&path, json) {
            warn!("Failed to write shell layout to {:?}: {}", path, e);
        } else {
            info!("Saved shell layout to {:?}", path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_layout_dock_slots() {
        let cfg = ShellLayoutConfig::default();
        assert_eq!(cfg.dock_apps.len(), 4);
        assert_eq!(cfg.dock_apps[0], "org.gnome.Calls");
        assert_eq!(cfg.dock_apps[1], "sm.puri.Chatty");
        assert_eq!(cfg.dock_apps[2], "org.gnome.Epiphany");
        assert_eq!(cfg.dock_apps[3], "org.gnome.Console");
        assert!(cfg.homescreen_apps.is_empty());
    }

    #[test]
    fn test_layout_serialization_roundtrip() {
        let cfg = ShellLayoutConfig {
            dock_apps: vec!["calls".to_string(), "chatty".to_string()],
            homescreen_apps: vec![
                HomescreenAppEntry {
                    id: "firefox".to_string(),
                    x: 0,
                    y: 0,
                },
                HomescreenAppEntry {
                    id: "foot".to_string(),
                    x: 1,
                    y: 0,
                },
            ],
        };

        let json = serde_json::to_string_pretty(&cfg).expect("Serialization failed");
        let deserialized: ShellLayoutConfig = serde_json::from_str(&json).expect("Deserialization failed");
        assert_eq!(deserialized.dock_apps.len(), 2);
        assert_eq!(deserialized.homescreen_apps.len(), 2);
        assert_eq!(deserialized.homescreen_apps[0].id, "firefox");
        assert_eq!(deserialized.homescreen_apps[1].id, "foot");
    }

    #[test]
    fn test_create_default_layout_fallback_generation() {
        let available = vec![
            "firefox.desktop".to_string(),
            "alacritty.desktop".to_string(),
        ];
        let cfg = create_default_layout(&available);
        assert_eq!(cfg.dock_apps.len(), 4);
        // slot 0 fallback calls
        assert_eq!(cfg.dock_apps[0], "org.gnome.Calls");
        // slot 1 fallback chatty
        assert_eq!(cfg.dock_apps[1], "sm.puri.Chatty");
        // slot 2 matched firefox
        assert_eq!(cfg.dock_apps[2], "firefox.desktop");
        // slot 3 matched alacritty
        assert_eq!(cfg.dock_apps[3], "alacritty.desktop");
        // homescreen starts empty
        assert!(cfg.homescreen_apps.is_empty());
    }
}
