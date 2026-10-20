use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomescreenAppEntry {
    pub id: String,
    #[serde(default)]
    pub page: i32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FolderEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub page: i32,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub app_ids: Vec<String>,
}

fn default_pages() -> i32 {
    1
}

pub fn default_wallpaper_folders() -> Vec<String> {
    let mut folders = Vec::new();
    if PathBuf::from("/usr/share/backgrounds").exists() {
        folders.push("/usr/share/backgrounds".to_string());
    }
    if PathBuf::from("/usr/share/wallpapers").exists() {
        folders.push("/usr/share/wallpapers".to_string());
    }
    if let Ok(home) = std::env::var("HOME") {
        let p_wall = PathBuf::from(&home).join("Pictures").join("Wallpapers");
        if p_wall.exists() {
            folders.push(p_wall.to_string_lossy().to_string());
        }
        let p_pic = PathBuf::from(&home).join("Pictures");
        if p_pic.exists() && !folders.contains(&p_pic.to_string_lossy().to_string()) {
            folders.push(p_pic.to_string_lossy().to_string());
        }
        let p_bg = PathBuf::from(&home).join(".local").join("share").join("backgrounds");
        if p_bg.exists() && !folders.contains(&p_bg.to_string_lossy().to_string()) {
            folders.push(p_bg.to_string_lossy().to_string());
        }
    }
    if folders.is_empty() {
        folders.push("/usr/share/backgrounds".to_string());
    }
    folders
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellLayoutConfig {
    pub dock_apps: Vec<String>,
    pub homescreen_apps: Vec<HomescreenAppEntry>,
    #[serde(default)]
    pub folders: Vec<FolderEntry>,
    #[serde(default = "default_pages")]
    pub num_pages: i32,
    #[serde(default)]
    pub wallpaper_path: String,
    #[serde(default = "default_wallpaper_folders")]
    pub wallpaper_folders: Vec<String>,
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
            folders: Vec::new(),
            num_pages: 1,
            wallpaper_path: String::new(),
            wallpaper_folders: default_wallpaper_folders(),
        }
    }
}

impl ShellLayoutConfig {
    /// Adds an app or moves it to a specific cell (page, x, y).
    /// If it was already on the homescreen, its position is updated.
    pub fn place_or_move_app(&mut self, app_id: &str, page: i32, x: i32, y: i32) {
        if let Some(entry) = self.homescreen_apps.iter_mut().find(|e| e.id == app_id) {
            entry.page = page;
            entry.x = x;
            entry.y = y;
        } else {
            self.homescreen_apps.push(HomescreenAppEntry {
                id: app_id.to_string(),
                page,
                x,
                y,
            });
        }
        if page >= self.num_pages {
            self.num_pages = page + 1;
        }
    }

    /// Creates a new folder or adds to an existing item at (page, x, y).
    /// If an existing app is at (page, x, y), it is merged with the new app into a folder!
    pub fn merge_into_folder(
        &mut self,
        target_app_id: &str,
        new_app_id: &str,
        page: i32,
        x: i32,
        y: i32,
    ) -> String {
        // Remove both apps from standalone homescreen list
        self.homescreen_apps.retain(|e| e.id != target_app_id && e.id != new_app_id);

        let folder_id = format!("folder_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis());

        let folder = FolderEntry {
            id: folder_id.clone(),
            name: "Folder".to_string(),
            page,
            x,
            y,
            app_ids: vec![target_app_id.to_string(), new_app_id.to_string()],
        };
        self.folders.push(folder);

        if page >= self.num_pages {
            self.num_pages = page + 1;
        }

        folder_id
    }

    /// Adds an app to an existing folder by folder ID.
    pub fn add_to_folder(&mut self, folder_id: &str, app_id: &str) {
        self.homescreen_apps.retain(|e| e.id != app_id);
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == folder_id) {
            if !folder.app_ids.contains(&app_id.to_string()) {
                folder.app_ids.push(app_id.to_string());
            }
        }
    }

    /// Renames a folder.
    pub fn rename_folder(&mut self, folder_id: &str, new_name: &str) {
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == folder_id) {
            folder.name = new_name.trim().to_string();
        }
    }

    /// Removes an app from a folder. If only 1 app remains, optionally dissolves the folder.
    pub fn remove_from_folder(&mut self, folder_id: &str, app_id: &str) {
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == folder_id) {
            folder.app_ids.retain(|id| id != app_id);
        }
        // If empty, remove folder
        self.folders.retain(|f| !f.app_ids.is_empty());
    }

    /// Removes a shortcut or folder from the homescreen.
    pub fn remove_item(&mut self, id: &str) {
        self.homescreen_apps.retain(|e| e.id != id);
        self.folders.retain(|f| f.id != id);
    }

    pub fn set_wallpaper(&mut self, path: &str) {
        self.wallpaper_path = path.to_string();
    }

    pub fn add_wallpaper_folder(&mut self, folder: &str) {
        let f_str = folder.to_string();
        if !self.wallpaper_folders.contains(&f_str) {
            self.wallpaper_folders.push(f_str);
        }
    }

    pub fn remove_wallpaper_folder(&mut self, folder: &str) {
        self.wallpaper_folders.retain(|f| f != folder);
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
            if let Ok(mut config) = serde_json::from_str::<ShellLayoutConfig>(&data) {
                info!("Loaded shell layout config from {:?}", path);
                if config.num_pages < 1 {
                    config.num_pages = 1;
                }
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
            default_dock.push(candidates[0].to_string());
        }
    }

    default_dock.truncate(4);

    ShellLayoutConfig {
        dock_apps: default_dock,
        homescreen_apps: Vec::new(),
        folders: Vec::new(),
        num_pages: 1,
        wallpaper_path: String::new(),
        wallpaper_folders: default_wallpaper_folders(),
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
        assert_eq!(cfg.num_pages, 1);
        assert!(cfg.folders.is_empty());
    }

    #[test]
    fn test_layout_serialization_roundtrip() {
        let cfg = ShellLayoutConfig {
            dock_apps: vec!["calls".to_string(), "chatty".to_string()],
            homescreen_apps: vec![
                HomescreenAppEntry {
                    id: "firefox".to_string(),
                    page: 0,
                    x: 0,
                    y: 0,
                },
                HomescreenAppEntry {
                    id: "foot".to_string(),
                    page: 1,
                    x: 1,
                    y: 0,
                },
            ],
            folders: vec![
                FolderEntry {
                    id: "f1".to_string(),
                    name: "Utilities".to_string(),
                    page: 0,
                    x: 2,
                    y: 0,
                    app_ids: vec!["calc".to_string(), "terminal".to_string()],
                }
            ],
            num_pages: 2,
            wallpaper_path: "/usr/share/backgrounds/gnome/adwaita-d.jxl".to_string(),
            wallpaper_folders: vec!["/usr/share/backgrounds".to_string()],
        };

        let json = serde_json::to_string_pretty(&cfg).expect("Serialization failed");
        let deserialized: ShellLayoutConfig = serde_json::from_str(&json).expect("Deserialization failed");
        assert_eq!(deserialized.dock_apps.len(), 2);
        assert_eq!(deserialized.homescreen_apps.len(), 2);
        assert_eq!(deserialized.homescreen_apps[0].id, "firefox");
        assert_eq!(deserialized.homescreen_apps[1].id, "foot");
        assert_eq!(deserialized.homescreen_apps[1].page, 1);
        assert_eq!(deserialized.folders.len(), 1);
        assert_eq!(deserialized.folders[0].name, "Utilities");
        assert_eq!(deserialized.num_pages, 2);
        assert_eq!(deserialized.wallpaper_path, "/usr/share/backgrounds/gnome/adwaita-d.jxl");
        assert_eq!(deserialized.wallpaper_folders, vec!["/usr/share/backgrounds".to_string()]);
    }

    #[test]
    fn test_create_default_layout_fallback_generation() {
        let available = vec![
            "firefox.desktop".to_string(),
            "alacritty.desktop".to_string(),
        ];
        let cfg = create_default_layout(&available);
        assert_eq!(cfg.dock_apps.len(), 4);
        assert_eq!(cfg.dock_apps[0], "org.gnome.Calls");
        assert_eq!(cfg.dock_apps[1], "sm.puri.Chatty");
        assert_eq!(cfg.dock_apps[2], "firefox.desktop");
        assert_eq!(cfg.dock_apps[3], "alacritty.desktop");
        assert!(cfg.homescreen_apps.is_empty());
    }

    #[test]
    fn test_folder_creation_and_reorder() {
        let mut cfg = ShellLayoutConfig::default();
        cfg.place_or_move_app("firefox", 0, 0, 0);
        cfg.place_or_move_app("foot", 0, 1, 0);
        assert_eq!(cfg.homescreen_apps.len(), 2);

        // Drag foot over firefox to merge into folder
        let folder_id = cfg.merge_into_folder("firefox", "foot", 0, 0, 0);
        assert_eq!(cfg.homescreen_apps.len(), 0);
        assert_eq!(cfg.folders.len(), 1);
        assert_eq!(cfg.folders[0].id, folder_id);
        assert_eq!(cfg.folders[0].app_ids, vec!["firefox", "foot"]);

        // Rename folder
        cfg.rename_folder(&folder_id, "Dev Tools");
        assert_eq!(cfg.folders[0].name, "Dev Tools");

        // Drag to right edge: move app to page 1
        cfg.place_or_move_app("chatty", 1, 0, 0);
        assert_eq!(cfg.num_pages, 2);
        assert_eq!(cfg.homescreen_apps.len(), 1);
        assert_eq!(cfg.homescreen_apps[0].page, 1);
    }
}
