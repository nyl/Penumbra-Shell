use freedesktop_desktop_entry::{default_paths, DesktopEntry, Iter};
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon_path: Option<PathBuf>,
    pub terminal: bool,
}

/// Discovers and parses all valid .desktop files on the host system
pub fn discover_installed_apps() -> Vec<InstalledApp> {
    let mut apps = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // Standard XDG application paths
    for entry_path in Iter::new(default_paths()) {
        if let Ok(entry) = DesktopEntry::from_path(&entry_path, None::<&[&str]>) {
            // Ignore entries flagged as hidden or no-display
            if entry.no_display() {
                continue;
            }

            if let Some(name) = entry.name(&[""]) {
                if let Some(exec_raw) = entry.exec() {
                    let id = entry.id().to_string();
                    if seen_ids.contains(&id) {
                        continue;
                    }
                    seen_ids.insert(id.clone());

                    // Clean FreeDesktop exec parameters (%u, %F, %i, etc.)
                    let exec_clean = clean_exec_cmd(exec_raw);

                    // Resolve icon path if specified
                    let icon_path = entry.icon().and_then(find_icon_fast);

                    apps.push(InstalledApp {
                        id,
                        name: name.to_string(),
                        exec: exec_clean,
                        icon_path,
                        terminal: entry.terminal(),
                    });
                }
            }
        }
    }

    // Sort alphabetically by application name
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    info!("Discovered {} installed XDG applications", apps.len());
    apps
}

/// Fast icon lookup checking standard system directories before slow freedesktop recursion
pub fn find_icon_fast(icon_name: &str) -> Option<PathBuf> {
    let p = std::path::Path::new(icon_name);
    if p.is_absolute() && p.exists() {
        return Some(p.to_path_buf());
    }

    let search_patterns = [
        format!("/usr/share/icons/hicolor/scalable/apps/{}.svg", icon_name),
        format!("/usr/share/icons/hicolor/48x48/apps/{}.png", icon_name),
        format!("/usr/share/icons/hicolor/64x64/apps/{}.png", icon_name),
        format!("/usr/share/icons/hicolor/128x128/apps/{}.png", icon_name),
        format!("/usr/share/icons/hicolor/256x256/apps/{}.png", icon_name),
        format!(
            "/usr/share/icons/hicolor/symbolic/apps/{}-symbolic.svg",
            icon_name
        ),
        format!("/usr/share/pixmaps/{}.png", icon_name),
        format!("/usr/share/pixmaps/{}.svg", icon_name),
    ];

    for candidate in &search_patterns {
        let path = std::path::Path::new(candidate);
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }

    freedesktop_icons::lookup(icon_name)
        .with_theme("hicolor")
        .with_size(64)
        .find()
}

/// Fast lookup for a single application .desktop file without traversing all directories
pub fn find_single_app(id: &str) -> Option<InstalledApp> {
    for base in default_paths() {
        let direct = base.join(format!("{}.desktop", id));
        if direct.exists() {
            if let Ok(entry) = DesktopEntry::from_path(&direct, None::<&[&str]>) {
                if let Some(name) = entry.name(&[""]) {
                    let exec_clean = entry.exec().map(clean_exec_cmd).unwrap_or_default();
                    let icon_path = entry.icon().and_then(find_icon_fast);
                    return Some(InstalledApp {
                        id: id.to_string(),
                        name: name.to_string(),
                        exec: exec_clean,
                        icon_path,
                        terminal: entry.terminal(),
                    });
                }
            }
        }
    }
    None
}

/// Strips field codes like %f, %F, %u, %U, %i, %c, %k from desktop Exec lines
pub fn clean_exec_cmd(exec: &str) -> String {
    let parts: Vec<&str> = exec
        .split_whitespace()
        .filter(|part| !part.starts_with('%'))
        .collect();
    parts.join(" ")
}

/// Spawns the application strictly on Penumbra's Wayland display
pub fn launch_app(app: &InstalledApp, target_output: Option<&str>) -> std::io::Result<()> {
    let socket =
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-penumbra-0".to_string());
    let target = target_output.unwrap_or("phone");

    // Communicate target output to penumbra-comp compositor via runtime file
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let target_file = std::path::Path::new(&runtime_dir).join("penumbra-target-output");
    let _ = std::fs::write(&target_file, target);

    let command_str = if app.terminal {
        // Fallback terminal wrapper if an app specifies Terminal=true
        format!("foot -e {}", app.exec)
    } else {
        app.exec.clone()
    };

    info!(
        "[Penumbra-Launcher] Spawning '{}' ({}) on {} (target: {})",
        app.name, command_str, socket, target
    );

    std::process::Command::new("sh")
        .arg("-c")
        .arg(command_str)
        .env("WAYLAND_DISPLAY", &socket)
        .env("GDK_BACKEND", "wayland")
        .env("QT_QPA_PLATFORM", "wayland")
        .spawn()?;

    Ok(())
}
