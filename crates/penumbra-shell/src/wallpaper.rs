//! Wallpaper discovery and file browsing utilities for Penumbra Homescreen.

use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct ScannedWallpaperPath {
    pub path: String,
    pub name: String,
    pub folder: String,
}

pub fn scan_wallpaper_paths_from_folders(folders: &[String]) -> Vec<ScannedWallpaperPath> {
    let mut items = Vec::new();
    let mut seen_paths = HashSet::new();
    let supported_exts = ["png", "jpg", "jpeg", "webp", "svg", "bmp"];

    for folder in folders {
        let root_dir = Path::new(folder);
        if !root_dir.exists() || !root_dir.is_dir() {
            continue;
        }

        // Never scan a screenshots folder directly
        let folder_name_lower = root_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if folder_name_lower == "screenshots" || folder_name_lower == "screenshot" {
            continue;
        }

        let mut dirs_to_visit = vec![(root_dir.to_path_buf(), 0)];
        while let Some((dir, depth)) = dirs_to_visit.pop() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    let file_name_lower = file_name.to_lowercase();

                    if path.is_dir() {
                        if file_name.starts_with('.')
                            || file_name_lower == "screenshots"
                            || file_name_lower == "screenshot"
                            || file_name_lower.starts_with("screenshot")
                        {
                            continue;
                        }
                        if depth < 2 {
                            dirs_to_visit.push((path, depth + 1));
                        }
                    } else if path.is_file() {
                        if file_name.starts_with('.') || file_name_lower.starts_with("screenshot") {
                            continue;
                        }

                        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                            if supported_exts.iter().any(|&s| s.eq_ignore_ascii_case(ext)) {
                                let lossy = path.to_string_lossy().to_string();
                                if seen_paths.insert(lossy.clone()) {
                                    let name = path
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "Wallpaper".to_string());
                                    let folder_name = path
                                        .parent()
                                        .and_then(|p| p.file_name())
                                        .map(|s| s.to_string_lossy().to_string())
                                        .unwrap_or_default();

                                    items.push(ScannedWallpaperPath {
                                        path: lossy,
                                        name,
                                        folder: folder_name,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items
}

#[derive(Clone, Debug)]
pub struct RawFileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_image: bool,
    pub size_str: String,
}

pub fn read_file_browser_entries(dir_path: &Path) -> Vec<RawFileEntry> {
    let mut dirs = Vec::new();
    let mut images = Vec::new();
    let mut others = Vec::new();
    let supported_exts = ["png", "jpg", "jpeg", "webp", "svg", "bmp"];

    if let Ok(entries) = std::fs::read_dir(dir_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }

            if path.is_dir() {
                dirs.push(RawFileEntry {
                    name: name.clone(),
                    path: path.to_string_lossy().to_string(),
                    is_dir: true,
                    is_image: false,
                    size_str: "Folder".to_string(),
                });
            } else if path.is_file() {
                let is_image = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| supported_exts.iter().any(|&s| s.eq_ignore_ascii_case(ext)))
                    .unwrap_or(false);

                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let size_str = if size > 1_000_000 {
                    format!("{:.1} MB", size as f64 / 1_000_000.0)
                } else if size > 1_000 {
                    format!("{} KB", size / 1_000)
                } else {
                    format!("{} B", size)
                };

                let item = RawFileEntry {
                    name,
                    path: path.to_string_lossy().to_string(),
                    is_dir: false,
                    is_image,
                    size_str,
                };

                if is_image {
                    images.push(item);
                } else {
                    others.push(item);
                }
            }
        }
    }

    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    images.sort_by(|a, b| a.name.cmp(&b.name));
    others.sort_by(|a, b| a.name.cmp(&b.name));

    let mut result = dirs;
    result.extend(images);
    result.extend(others);
    result
}
