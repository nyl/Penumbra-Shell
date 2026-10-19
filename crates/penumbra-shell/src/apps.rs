use std::path::PathBuf;
use freedesktop_desktop_entry::{default_paths, DesktopEntry, Iter};
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
                    let icon_path = entry.icon().and_then(|icon_name| {
                        freedesktop_icons::lookup(icon_name)
                            .with_theme("hicolor")
                            .with_size(64)
                            .find()
                    });

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
    let socket = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-penumbra-0".to_string());
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
