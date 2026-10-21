use std::rc::Rc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::Local;
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use clap::Parser;
use penumbra_gestures::{NavGesture, NavGestureRecognizer, Spring};
use penumbra_services::{BatteryService, NetworkService};

mod apps;
use apps::{discover_installed_apps, find_single_app, launch_app, InstalledApp};

mod config;
use config::{load_layout_config, save_layout_config};

slint::include_modules!();

fn resolve_app_item(id: &str, apps_list: &[InstalledApp]) -> AppItem {
    let app_opt = apps_list
        .iter()
        .find(|a| {
            a.id.eq_ignore_ascii_case(id)
                || a.name.eq_ignore_ascii_case(id)
                || a.id.to_lowercase().contains(&id.to_lowercase())
        })
        .cloned()
        .or_else(|| find_single_app(id));

    if let Some(app) = app_opt {
        let (has_icon, img) = if let Some(ref path) = app.icon_path {
            match slint::Image::load_from_path(path) {
                Ok(img) => (true, img),
                Err(_) => (false, slint::Image::default()),
            }
        } else {
            (false, slint::Image::default())
        };
        AppItem {
            id: app.id.into(),
            name: app.name.into(),
            has_icon,
            icon: img,
        }
    } else {
        let name = match id {
            "org.gnome.Calls" | "calls" => "Calls",
            "sm.puri.Chatty" | "chatty" => "Chatty",
            "org.gnome.Epiphany" | "epiphany" => "Web",
            "org.gnome.Console" | "foot" => "Console",
            _ => id.split('.').last().unwrap_or(id),
        };
        AppItem {
            id: id.to_string().into(),
            name: name.to_string().into(),
            has_icon: false,
            icon: slint::Image::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScannedWallpaperPath {
    pub path: String,
    pub name: String,
    pub folder: String,
}

pub fn scan_wallpaper_paths_from_folders(folders: &[String]) -> Vec<ScannedWallpaperPath> {
    let mut items = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    let supported_exts = ["png", "jpg", "jpeg", "webp", "svg", "bmp"];

    for folder in folders {
        let root_dir = std::path::Path::new(folder);
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

        // Recursively inspect directory up to depth 2 (e.g. /usr/share/backgrounds/gnome)
        let mut dirs_to_visit = vec![(root_dir.to_path_buf(), 0)];
        while let Some((dir, depth)) = dirs_to_visit.pop() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    let file_name_lower = file_name.to_lowercase();

                    if path.is_dir() {
                        // Skip hidden dirs and screenshots dirs
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
                        // Skip screenshot files (e.g. Screenshot from..., Screenshot_...)
                        if file_name.starts_with('.') || file_name_lower.starts_with("screenshot") {
                            continue;
                        }

                        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                            if supported_exts.iter().any(|&s| s.eq_ignore_ascii_case(ext)) {
                                let canonical_or_lossy = path.to_string_lossy().to_string();
                                if seen_paths.insert(canonical_or_lossy.clone()) {
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
                                        path: canonical_or_lossy,
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

#[cfg(test)]
fn scan_wallpapers_from_folders(folders: &[String]) -> Vec<WallpaperItem> {
    let paths = scan_wallpaper_paths_from_folders(folders);
    let mut items = Vec::new();
    for p in paths {
        if let Ok(img) = slint::Image::load_from_path(std::path::Path::new(&p.path)) {
            items.push(WallpaperItem {
                path: p.path.into(),
                name: p.name.into(),
                folder: p.folder.into(),
                thumbnail: img,
            });
        }
    }
    items
}

fn read_file_browser_entries(dir_path: &std::path::Path) -> Vec<FileEntry> {
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
                dirs.push(FileEntry {
                    name: name.into(),
                    path: path.to_string_lossy().to_string().into(),
                    is_dir: true,
                    is_image: false,
                    size_str: "Folder".into(),
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

                let entry = FileEntry {
                    name: name.into(),
                    path: path.to_string_lossy().to_string().into(),
                    is_dir: false,
                    is_image,
                    size_str: size_str.into(),
                };

                if is_image {
                    images.push(entry);
                } else {
                    others.push(entry);
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

#[derive(Parser, Debug, Clone)]
#[command(
    name = "penumbra-shell",
    about = "Material 3 Mobile Desktop Environment & Convergence Emulator",
    version = "0.1.0"
)]
pub struct ShellArgs {
    /// Launch in dual-screen convergence emulation mode (Phone DSI + External HDMI).
    #[arg(short, long, default_value_t = true)]
    pub dual_screen: bool,

    /// Launch only the phone screen (412x915).
    #[arg(short, long, default_value_t = false)]
    pub phone_only: bool,
}

const DRAWER_CLOSED_Y: f64 = -915.0;
const DRAWER_OPEN_Y: f64 = 0.0;

fn is_binary_available(bin: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let full_path = dir.join(bin);
            if full_path.is_file() {
                return true;
            }
        }
    }
    false
}

fn sync_shell_state(mode_str: &str, drawer_open: bool, overview_idx: usize) {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&runtime_dir).join("penumbra-shell-state");
    let content = format!(
        "mode={}\ndrawer={}\noverview_index={}\n",
        mode_str,
        if drawer_open { 1 } else { 0 },
        overview_idx
    );
    let tmp = path.with_extension("tmp");
    let _ = std::fs::write(&tmp, content);
    let _ = std::fs::rename(&tmp, &path);
}

fn send_shell_cmd(cmd: &str) {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&runtime_dir).join("penumbra-shell-cmd");
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{}", cmd);
    }
}

#[derive(Clone, Debug, Default)]
pub struct RunningTask {
    pub id: i32,
    pub app_id: String,
    pub title: String,
}

fn load_running_tasks() -> Vec<RunningTask> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&runtime_dir).join("penumbra-windows");
    if let Ok(content) = std::fs::read_to_string(path) {
        content
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 {
                    let id = parts[0].parse::<i32>().ok()?;
                    Some(RunningTask {
                        id,
                        app_id: parts[1].to_string(),
                        title: parts[2].to_string(),
                    })
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = ShellArgs::parse();

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Initializing Penumbra Mobile Shell (Material 3 Expressive)...");
    info!(
        "Dual-screen convergence emulator: {}",
        args.dual_screen && !args.phone_only
    );

    let t0 = Instant::now();
    let ui = ShellRoot::new()?;
    info!("ShellRoot::new completed in {:.2?}", t0.elapsed());

    let t1 = Instant::now();
    ui.set_convergence_mode(args.dual_screen && !args.phone_only);
    sync_shell_state("home", false, 0);

    let available_ids: Vec<String> = Vec::new();
    let layout_config = Rc::new(std::cell::RefCell::new(load_layout_config(&available_ids)));
    info!("load_layout_config completed in {:.2?}", t1.elapsed());

    let apps_list = Arc::new(RwLock::new(Vec::<InstalledApp>::new()));

    let ui_weak = ui.as_weak();

    // Populate dynamic dock and homescreen models from layout config
    let update_layout_models = {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);
        let layout_config = Rc::clone(&layout_config);
        Rc::new(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let cfg = layout_config.borrow();
                let apps = apps_list.read().unwrap();
                let dock_model = Rc::new(VecModel::default());
                for id in cfg.dock_apps.iter().take(4) {
                    dock_model.push(resolve_app_item(id, &apps));
                }
                ui.set_dock_apps(ModelRc::from(dock_model));

                let home_model = Rc::new(VecModel::default());
                for entry in cfg.homescreen_apps.iter() {
                    let app_item = resolve_app_item(&entry.id, &apps);
                    home_model.push(HomescreenItem {
                        id: app_item.id.clone(),
                        name: app_item.name.clone(),
                        is_folder: false,
                        folder_id: "".into(),
                        page: entry.page,
                        x: entry.x,
                        y: entry.y,
                        has_icon: app_item.has_icon,
                        icon: app_item.icon.clone(),
                        folder_count: 0,
                        has_prev_1: false,
                        preview_1: slint::Image::default(),
                        has_prev_2: false,
                        preview_2: slint::Image::default(),
                        has_prev_3: false,
                        preview_3: slint::Image::default(),
                        has_prev_4: false,
                        preview_4: slint::Image::default(),
                    });
                }
                for folder in cfg.folders.iter() {
                    let p1 = folder.app_ids.first().map(|id| resolve_app_item(id, &apps));
                    let p2 = folder.app_ids.get(1).map(|id| resolve_app_item(id, &apps));
                    let p3 = folder.app_ids.get(2).map(|id| resolve_app_item(id, &apps));
                    let p4 = folder.app_ids.get(3).map(|id| resolve_app_item(id, &apps));

                    home_model.push(HomescreenItem {
                        id: folder.id.clone().into(),
                        name: folder.name.clone().into(),
                        is_folder: true,
                        folder_id: folder.id.clone().into(),
                        page: folder.page,
                        x: folder.x,
                        y: folder.y,
                        has_icon: false,
                        icon: slint::Image::default(),
                        folder_count: folder.app_ids.len() as i32,
                        has_prev_1: p1.as_ref().map_or(false, |p| p.has_icon),
                        preview_1: p1.map_or_else(slint::Image::default, |p| p.icon),
                        has_prev_2: p2.as_ref().map_or(false, |p| p.has_icon),
                        preview_2: p2.map_or_else(slint::Image::default, |p| p.icon),
                        has_prev_3: p3.as_ref().map_or(false, |p| p.has_icon),
                        preview_3: p3.map_or_else(slint::Image::default, |p| p.icon),
                        has_prev_4: p4.as_ref().map_or(false, |p| p.has_icon),
                        preview_4: p4.map_or_else(slint::Image::default, |p| p.icon),
                    });
                }
                ui.set_homescreen_items(ModelRc::from(home_model));
                ui.set_num_pages(cfg.num_pages);
            }
        })
    };
    let t_models = Instant::now();
    update_layout_models();
    info!("update_layout_models completed in {:.2?}", t_models.elapsed());

    // Helper to refresh Slint running-tasks model from compositor IPC
    let refresh_running_tasks = {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);
        Rc::new(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let tasks = load_running_tasks();
                let slint_tasks = Rc::new(VecModel::default());
                let apps = apps_list.read().unwrap();
                for task in tasks {
                    let (name, has_icon, icon) = if let Some(app) = apps.iter().find(|a| {
                        a.id.eq_ignore_ascii_case(&task.app_id)
                            || a.name.eq_ignore_ascii_case(&task.app_id)
                            || task.app_id.contains(&a.id)
                    }) {
                        let (has_icon, img) = if let Some(ref path) = app.icon_path {
                            match slint::Image::load_from_path(path) {
                                Ok(img) => (true, img),
                                Err(_) => (false, slint::Image::default()),
                            }
                        } else {
                            (false, slint::Image::default())
                        };
                        (app.name.clone(), has_icon, img)
                    } else {
                        let display_name = if !task.title.is_empty() {
                            task.title.clone()
                        } else if !task.app_id.is_empty() {
                            task.app_id.clone()
                        } else {
                            format!("Task {}", task.id)
                        };
                        (display_name, false, slint::Image::default())
                    };

                    slint_tasks.push(AppCardData {
                        id: task.id,
                        app_id: task.app_id.into(),
                        name: name.into(),
                        has_icon,
                        icon,
                    });
                }
                ui.set_running_tasks(ModelRc::from(slint_tasks));
            }
        })
    };
    refresh_running_tasks();

    // Periodic task poller to keep running-tasks model synced with compositor
    let task_poll_timer = Timer::default();
    {
        let refresh_tasks = refresh_running_tasks.clone();
        task_poll_timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            refresh_tasks();
        });
    }

    // 1. Damped Spring Physics State for Quick Settings Drawer
    let drawer_spring = Rc::new(std::cell::RefCell::new(
        Spring::expressive()
            .with_position(DRAWER_CLOSED_Y)
            .with_target(DRAWER_CLOSED_Y),
    ));

    // Physics Animation Timer driving F = -k*x - c*v
    let anim_timer = Rc::new(Timer::default());
    let is_animating = Rc::new(std::cell::Cell::new(false));

    // Function to initiate spring animation towards target
    let start_spring_transition = {
        let ui_weak = ui_weak.clone();
        let drawer_spring = drawer_spring.clone();
        let anim_timer = anim_timer.clone();
        let is_animating = is_animating.clone();

        Rc::new(move |target: f64, initial_velocity: f64| {
            {
                let mut spring = drawer_spring.borrow_mut();
                spring.set_target(target);
                if initial_velocity.abs() > 50.0 {
                    spring.set_velocity(initial_velocity);
                }
            }

            if is_animating.get() {
                return;
            }
            is_animating.set(true);

            let ui_weak = ui_weak.clone();
            let drawer_spring = drawer_spring.clone();
            let is_animating = is_animating.clone();
            let mut last_tick = Instant::now();

            anim_timer.start(
                TimerMode::Repeated,
                Duration::from_millis(16), // ~60fps smooth physics step
                move || {
                    let now = Instant::now();
                    let dt = (now.duration_since(last_tick))
                        .as_secs_f64()
                        .clamp(0.001, 0.05);
                    last_tick = now;

                    let mut spring = drawer_spring.borrow_mut();
                    let new_y = spring.step(dt);

                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_drawer_y(new_y as f32);
                        ui.set_drawer_open(new_y > DRAWER_CLOSED_Y + 10.0);
                    }

                    // Settle check
                    if spring.is_settled(0.5, 5.0) {
                        let final_target = spring.target;
                        spring.position = final_target;
                        spring.velocity = 0.0;
                        if let Some(ui) = ui_weak.upgrade() {
                            let is_open = final_target >= DRAWER_OPEN_Y - 5.0;
                            ui.set_drawer_y(final_target as f32);
                            ui.set_drawer_open(is_open);
                            let mode_str = match ui.get_active_mode() {
                                1 => "overview",
                                3 => "app",
                                _ => "home",
                            };
                            sync_shell_state(mode_str, is_open, 0);
                        }
                        is_animating.set(false);
                    }
                },
            );
        })
    };

    // Callback: Toggle Drawer via tap on status bar or handle
    {
        let drawer_spring = drawer_spring.clone();
        let start_spring = start_spring_transition.clone();

        ui.on_toggle_drawer(move || {
            let current_target = drawer_spring.borrow().target;
            let next_target = if current_target >= DRAWER_OPEN_Y - 50.0 {
                DRAWER_CLOSED_Y
            } else {
                DRAWER_OPEN_Y
            };
            info!("Toggling drawer: current target={current_target}, next={next_target}");
            start_spring(next_target, 0.0);
        });
    }

    // 2. Status Bar Top Pull-Down & Tap Gesture Tracking
    let status_bar_drag_start = Rc::new(std::cell::Cell::new(None::<(f64, f64)>));
    {
        let start_pos = status_bar_drag_start.clone();
        ui.on_status_bar_down(move |x, y| {
            start_pos.set(Some((x as f64, y as f64)));
        });
    }

    {
        let start_pos = status_bar_drag_start.clone();
        let drawer_spring = drawer_spring.clone();
        let ui_weak = ui_weak.clone();

        ui.on_status_bar_moved(move |_x, y| {
            if let Some((_, start_y)) = start_pos.get() {
                let delta_y = (y as f64 - start_y).max(0.0);
                let drag_y =
                    (DRAWER_CLOSED_Y + delta_y * 1.5).clamp(DRAWER_CLOSED_Y, DRAWER_OPEN_Y);
                drawer_spring.borrow_mut().set_position(drag_y);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_drawer_y(drag_y as f32);
                    ui.set_drawer_open(drag_y > DRAWER_CLOSED_Y + 10.0);
                }
            }
        });
    }

    {
        let start_pos = status_bar_drag_start.clone();
        let drawer_spring = drawer_spring.clone();
        let start_spring = start_spring_transition.clone();

        ui.on_status_bar_up(move |_x, y| {
            if let Some((_, start_y)) = start_pos.take() {
                let delta_y = y as f64 - start_y;
                let current_pos = drawer_spring.borrow().position;
                if delta_y > 70.0 || current_pos > -600.0 {
                    info!("Pull down drawer opened (drag distance: {:.0}px)", delta_y);
                    start_spring(DRAWER_OPEN_Y, 800.0);
                } else if delta_y < 12.0 {
                    // Tap on status bar toggles drawer
                    let current_target = drawer_spring.borrow().target;
                    let next_target = if current_target >= DRAWER_OPEN_Y - 50.0 {
                        DRAWER_CLOSED_Y
                    } else {
                        DRAWER_OPEN_Y
                    };
                    info!("Status bar tap toggles drawer -> target={}", next_target);
                    start_spring(next_target, 0.0);
                } else {
                    start_spring(DRAWER_CLOSED_Y, -800.0);
                }
            }
        });
    }

    {
        let drawer_spring = drawer_spring.clone();
        let ui_weak = ui_weak.clone();
        ui.on_homescreen_pulled_down(move |dy| {
            let drag_y = (DRAWER_CLOSED_Y + dy as f64).clamp(DRAWER_CLOSED_Y, DRAWER_OPEN_Y);
            drawer_spring.borrow_mut().set_position(drag_y);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_drawer_y(drag_y as f32);
                ui.set_drawer_open(true);
            }
        });
    }

    {
        let drawer_spring = drawer_spring.clone();
        let start_spring = start_spring_transition.clone();
        ui.on_homescreen_pull_released(move || {
            let current_pos = drawer_spring.borrow().position;
            if current_pos > DRAWER_CLOSED_Y + 40.0 {
                info!("Homescreen pull down opened Quick Settings drawer");
                start_spring(DRAWER_OPEN_Y, 800.0);
            } else {
                start_spring(DRAWER_CLOSED_Y, -800.0);
            }
        });
    }

    // 3. Quick Settings Bottom Pull-Up Handle
    let handle_drag_start = Rc::new(std::cell::Cell::new(None::<(f64, f64)>));
    {
        let start_pos = handle_drag_start.clone();
        ui.on_drawer_handle_down(move |x, y| {
            start_pos.set(Some((x as f64, y as f64)));
        });
    }

    {
        let start_pos = handle_drag_start.clone();
        let drawer_spring = drawer_spring.clone();
        let ui_weak = ui_weak.clone();

        ui.on_drawer_handle_moved(move |_x, y| {
            if let Some((_, start_y)) = start_pos.get() {
                let delta_y = y as f64 - start_y; // Negative when dragging up
                let drag_y = (DRAWER_OPEN_Y + delta_y * 1.5).clamp(DRAWER_CLOSED_Y, DRAWER_OPEN_Y);
                drawer_spring.borrow_mut().set_position(drag_y);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_drawer_y(drag_y as f32);
                    ui.set_drawer_open(drag_y > DRAWER_CLOSED_Y + 10.0);
                }
            }
        });
    }

    {
        let start_pos = handle_drag_start.clone();
        let drawer_spring = drawer_spring.clone();
        let start_spring = start_spring_transition.clone();

        ui.on_drawer_handle_up(move |_x, y| {
            if let Some((_, start_y)) = start_pos.take() {
                let delta_y = y as f64 - start_y;
                let current_pos = drawer_spring.borrow().position;
                if delta_y < -50.0 || current_pos < -300.0 {
                    info!("Dismiss drawer via handle pull-up");
                    start_spring(DRAWER_CLOSED_Y, -800.0);
                } else if delta_y.abs() < 12.0 {
                    // Tap on handle closes drawer
                    start_spring(DRAWER_CLOSED_Y, 0.0);
                } else {
                    start_spring(DRAWER_OPEN_Y, 800.0);
                }
            }
        });
    }

    // 4. Bottom Nav Bar Gestures (Fast Flick -> Home, Drag up & hold > 80px -> Overview)
    let nav_recognizer = Rc::new(std::cell::RefCell::new(NavGestureRecognizer::default()));
    let nav_timer = Rc::new(Timer::default());
    let nav_start = Rc::new(std::cell::Cell::new(Instant::now()));

    let apply_nav_gesture = {
        let ui_weak = ui_weak.clone();
        let refresh_tasks = refresh_running_tasks.clone();
        Rc::new(move |gesture: NavGesture| {
            if let Some(ui) = ui_weak.upgrade() {
                match gesture {
                    NavGesture::GoHome => {
                        info!("NavGesture: GoHome (Fast upward flick) -> Returning Home");
                        ui.set_active_mode(0);
                        sync_shell_state("home", ui.get_drawer_open(), 0);
                    }
                    NavGesture::OpenOverview => {
                        info!("NavGesture: OpenOverview (Drag up & hold > 80px) -> Opening Task Switcher");
                        refresh_tasks();
                        ui.set_overview_index(0);
                        ui.set_active_mode(1);
                        sync_shell_state("overview", ui.get_drawer_open(), 0);
                    }
                    NavGesture::SwitchNextApp => {
                        info!("NavGesture: SwitchNextApp (Swipe left on bottom bar)");
                        let tasks = load_running_tasks();
                        if tasks.len() >= 2 {
                            let next_task = &tasks[1];
                            info!("Switching to next app: {} (id: {})", next_task.title, next_task.id);
                            send_shell_cmd(&format!("focus {}", next_task.id));
                            let target_name = if !next_task.title.is_empty() {
                                next_task.title.clone()
                            } else {
                                next_task.app_id.clone()
                            };
                            ui.set_active_app(target_name.into());
                            ui.set_active_mode(3);
                            sync_shell_state("app", ui.get_drawer_open(), 0);
                            refresh_tasks();
                        }
                    }
                    NavGesture::SwitchPrevApp => {
                        info!("NavGesture: SwitchPrevApp (Swipe right on bottom bar)");
                        let tasks = load_running_tasks();
                        if tasks.len() >= 2 {
                            let prev_task = tasks.last().unwrap();
                            info!("Switching to prev app: {} (id: {})", prev_task.title, prev_task.id);
                            send_shell_cmd(&format!("focus {}", prev_task.id));
                            let target_name = if !prev_task.title.is_empty() {
                                prev_task.title.clone()
                            } else {
                                prev_task.app_id.clone()
                            };
                            ui.set_active_app(target_name.into());
                            ui.set_active_mode(3);
                            sync_shell_state("app", ui.get_drawer_open(), 0);
                            refresh_tasks();
                        }
                    }
                }
            }
        })
    };

    {
        let nav_rec = nav_recognizer.clone();
        let nav_start = nav_start.clone();
        let nav_timer = nav_timer.clone();
        let apply_nav = apply_nav_gesture.clone();

        ui.on_nav_bar_down(move |x, y| {
            let now = Instant::now();
            nav_start.set(now);
            nav_rec.borrow_mut().on_down(x as f64, y as f64, Duration::ZERO);

            let nav_rec = nav_rec.clone();
            let nav_start = nav_start.clone();
            let apply_nav = apply_nav.clone();
            nav_timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
                let elapsed = nav_start.get().elapsed();
                if let Some(gesture) = nav_rec.borrow_mut().poll(elapsed) {
                    apply_nav(gesture);
                }
            });
        });
    }

    {
        let nav_rec = nav_recognizer.clone();
        let nav_start = nav_start.clone();
        let apply_nav = apply_nav_gesture.clone();

        ui.on_nav_bar_moved(move |x, y| {
            let elapsed = nav_start.get().elapsed();
            if let Some(gesture) = nav_rec.borrow_mut().on_move(x as f64, y as f64, elapsed) {
                apply_nav(gesture);
            }
        });
    }

    {
        let nav_rec = nav_recognizer.clone();
        let nav_start = nav_start.clone();
        let nav_timer = nav_timer.clone();
        let apply_nav = apply_nav_gesture.clone();

        ui.on_nav_bar_up(move |x, y| {
            nav_timer.stop();
            let elapsed = nav_start.get().elapsed();
            if let Some(gesture) = nav_rec.borrow_mut().on_up(x as f64, y as f64, elapsed) {
                apply_nav(gesture);
            }
        });
    }

fn spawn_real_app(app: &str, target: &str) {
    info!(
        "Shell launching real Wayland application '{}' on target '{}'",
        app, target
    );

    // Communicate target output to penumbra-comp compositor via runtime file
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let target_file = std::path::Path::new(&runtime_dir).join("penumbra-target-output");
    let _ = std::fs::write(&target_file, target);

    let wayland_display = std::env::var("WAYLAND_DISPLAY")
        .unwrap_or_else(|_| "wayland-penumbra-0".to_string());

    let candidates: &[&str] = match app {
        "terminal" | "org.gnome.Console" | "foot" => &["foot", "alacritty", "kgx", "kitty", "xterm", "gnome-terminal"],
        "files" | "org.gnome.Nautilus" => &["nautilus", "nemo", "thunar", "pcmanfm"],
        "calc" | "org.gnome.Calculator" => &["gnome-calculator", "kcalc"],
        "browser" | "org.gnome.Epiphany" | "epiphany" => &["epiphany", "firefox", "chromium", "google-chrome"],
        "notes" | "editor" | "org.gnome.TextEditor" => &["gnome-text-editor", "gedit", "kate"],
        "calls" | "org.gnome.Calls" => &["calls", "gnome-calls"],
        "chatty" | "sm.puri.Chatty" => &["chatty", "puri-chatty"],
        "camera" | "org.gnome.Camera" | "snapshot" => &["snapshot", "gnome-camera", "cheese"],
        "settings" => &["gnome-control-center"],
        custom => &[custom],
    };

    let mut spawned = false;
    for bin in candidates {
        if is_binary_available(bin) {
            info!(
                "Spawning real binary '{}' with WAYLAND_DISPLAY={} for target {}",
                bin, wayland_display, target
            );
            match std::process::Command::new(bin)
                .env("WAYLAND_DISPLAY", &wayland_display)
                .env("GDK_BACKEND", "wayland")
                .env("QT_QPA_PLATFORM", "wayland")
                .spawn()
            {
                Ok(child) => {
                    info!(
                        "Successfully spawned '{}' (PID: {}) on target {}",
                        bin,
                        child.id(),
                        target
                    );
                    spawned = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!("Failed to spawn '{}': {}", bin, e);
                }
            }
        }
    }

    if !spawned {
        tracing::warn!(
            "No available candidate binary found in PATH for app '{}'",
            app
        );
    }
}

    // 5. In-Shell Application Engine (Phone Screen Output A)
    {
        let ui_weak = ui_weak.clone();
        let apps_lookup = Arc::clone(&apps_list);
        ui.on_app_launched(move |app_id_or_name| {
            let id_str = app_id_or_name.as_str();
            let target_app_opt = {
                let apps = apps_lookup.read().unwrap();
                apps.iter().find(|a| a.id == id_str || a.name.eq_ignore_ascii_case(id_str)).cloned()
            }.or_else(|| find_single_app(id_str));

            let app_name = target_app_opt
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_else(|| app_id_or_name.to_string());

            if let Some(ref target_app) = target_app_opt {
                if let Err(e) = launch_app(target_app, Some("phone")) {
                    tracing::warn!("Failed to launch XDG app '{}': {}", target_app.name, e);
                }
            } else {
                spawn_real_app(id_str, "phone");
            }

            if let Some(ui) = ui_weak.upgrade() {
                ui.set_active_app(app_name.into());
                ui.set_active_mode(3);
                sync_shell_state("app", ui.get_drawer_open(), 0);
            }
        });
    }

    // 6. Convergence Desktop Application Engine (External Monitor Output B)
    {
        let apps_lookup = Arc::clone(&apps_list);
        ui.on_desktop_app_launched(move |app_id_or_name| {
            let id_str = app_id_or_name.as_str();
            let target_app_opt = {
                let apps = apps_lookup.read().unwrap();
                apps.iter().find(|a| a.id == id_str || a.name.eq_ignore_ascii_case(id_str)).cloned()
            }.or_else(|| find_single_app(id_str));

            if let Some(ref target_app) = target_app_opt {
                if let Err(e) = launch_app(target_app, Some("desktop")) {
                    tracing::warn!("Failed to launch XDG app '{}' on desktop: {}", target_app.name, e);
                }
            } else {
                spawn_real_app(id_str, "desktop");
            }
        });
    }

    // 7. App Drawer Live Search & Filtering Engine
    {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);
        ui.on_app_search_query_changed(move |query_slint| {
            let query = query_slint.trim().to_lowercase();
            if let Some(ui) = ui_weak.upgrade() {
                let apps = apps_list.read().unwrap();
                if query.is_empty() {
                    let mut items = Vec::with_capacity(apps.len());
                    for app in apps.iter() {
                        items.push(resolve_app_item(&app.id, &apps));
                    }
                    ui.set_installed_apps(ModelRc::from(Rc::new(VecModel::from(items))));
                } else {
                    let mut exact_prefix = Vec::new();
                    let mut name_matches = Vec::new();
                    let mut other_matches = Vec::new();

                    for app in apps.iter() {
                        let name_lower = app.name.to_lowercase();
                        let id_lower = app.id.to_lowercase();

                        if name_lower.starts_with(&query) || id_lower.starts_with(&query) {
                            exact_prefix.push(resolve_app_item(&app.id, &apps));
                        } else if name_lower.contains(&query) {
                            name_matches.push(resolve_app_item(&app.id, &apps));
                        } else if id_lower.contains(&query) || app.exec.to_lowercase().contains(&query) {
                            other_matches.push(resolve_app_item(&app.id, &apps));
                        }
                    }

                    let mut filtered = exact_prefix;
                    filtered.extend(name_matches);
                    filtered.extend(other_matches);

                    ui.set_installed_apps(ModelRc::from(Rc::new(VecModel::from(filtered))));
                }
            }
        });
    }

    // App Drawer Search Enter / Accepted -> Launch First Match
    {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);
        ui.on_app_search_accepted(move |query_slint| {
            let query = query_slint.trim().to_lowercase();
            if query.is_empty() {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                let apps = apps_list.read().unwrap();
                let first_match = apps.iter().find(|app| {
                    let name_lower = app.name.to_lowercase();
                    let id_lower = app.id.to_lowercase();
                    name_lower.starts_with(&query)
                        || name_lower.contains(&query)
                        || id_lower.contains(&query)
                }).cloned();

                if let Some(target_app) = first_match {
                    let id_str = target_app.id.clone();
                    let name_str = target_app.name.clone();
                    info!("App search enter: launching top result '{}' (id: {})", name_str, id_str);
                    if let Err(e) = launch_app(&target_app, Some("phone")) {
                        tracing::warn!("Failed to launch XDG app '{}': {}", target_app.name, e);
                    }
                    ui.set_active_app(name_str.into());
                    ui.set_active_mode(3);
                    sync_shell_state("app", ui.get_drawer_open(), 0);
                }
            }
        });
    }

    // Terminal command executor: executes real commands on Linux and returns output
    ui.on_execute_command(|cmd_str| {
        let cmd = cmd_str.trim();
        if cmd.is_empty() {
            return String::new().into();
        }
        if cmd == "clear" {
            return "".into();
        }
        match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
            Ok(output) => {
                let mut res = String::from_utf8_lossy(&output.stdout).to_string();
                if !output.stderr.is_empty() {
                    if !res.is_empty() {
                        res.push('\n');
                    }
                    res.push_str(&String::from_utf8_lossy(&output.stderr));
                }
                if res.is_empty() {
                    res = "(command completed with no output)".to_string();
                }
                res.into()
            }
            Err(e) => format!("Error executing command: {e}").into(),
        }
    });

    // Real directory reader for in-shell Files manager
    ui.on_list_directory(|dir_path| {
        let path = if dir_path.is_empty() {
            "/home/nyl"
        } else {
            dir_path.as_str()
        };
        let mut listing = String::new();
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut dirs = Vec::new();
            let mut files = Vec::new();
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if ft.is_dir() {
                        dirs.push(format!("[DIR]  {name}/"));
                    } else {
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        files.push(format!("[FILE] {name} ({size} B)"));
                    }
                }
            }
            dirs.sort();
            files.sort();
            for d in dirs {
                listing.push_str(&d);
                listing.push('\n');
            }
            for f in files {
                listing.push_str(&f);
                listing.push('\n');
            }
        } else {
            listing = format!("Unable to read directory: {path}");
        }
        listing.into()
    });

    // File saver for in-shell Notes editor
    ui.on_save_file(|path_str, content| {
        let path = if path_str.is_empty() {
            "/home/nyl/quick_notes.txt"
        } else {
            path_str.as_str()
        };
        std::fs::write(path, content.as_str()).is_ok()
    });

    // Arithmetic evaluator for in-shell Calculator
    ui.on_eval_calc(|a_str, op, b_str| {
        let a: f64 = a_str.trim().parse().unwrap_or(0.0);
        let b: f64 = b_str.trim().parse().unwrap_or(0.0);
        let res = match op.as_str() {
            "+" => a + b,
            "-" => a - b,
            "*" => a * b,
            "/" => {
                if b.abs() < 1e-9 {
                    return "Error".into();
                }
                a / b
            }
            _ => b,
        };
        if (res.fract()).abs() < 1e-6 {
            format!("{:.0}", res).into()
        } else {
            format!("{:.4}", res)
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string()
                .into()
        }
    });

    // Clear all tasks from recents overview
    {
        let ui_weak = ui_weak.clone();
        let refresh_tasks = refresh_running_tasks.clone();
        ui.on_clear_all_recents(move || {
            info!("Overview: clear all tasks requested");
            send_shell_cmd("close-all");
            let ui_weak = ui_weak.clone();
            let refresh_tasks = refresh_tasks.clone();
            slint::Timer::single_shot(Duration::from_millis(100), move || {
                refresh_tasks();
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_active_mode(0);
                    sync_shell_state("home", ui.get_drawer_open(), 0);
                }
            });
        });
    }

    // Select task from overview carousel tap
    {
        let ui_weak = ui_weak.clone();
        let refresh_tasks = refresh_running_tasks.clone();
        ui.on_select_task(move |task_id| {
            info!("Overview: selecting task id={}", task_id);
            send_shell_cmd(&format!("focus {}", task_id));
            if let Some(ui) = ui_weak.upgrade() {
                let tasks = load_running_tasks();
                if let Some(t) = tasks.iter().find(|t| t.id == task_id) {
                    let name = if !t.title.is_empty() {
                        t.title.clone()
                    } else {
                        t.app_id.clone()
                    };
                    ui.set_active_app(name.into());
                }
                ui.set_active_mode(3);
                sync_shell_state("app", ui.get_drawer_open(), 0);
                refresh_tasks();
            }
        });
    }

    // Close individual task from overview card swipe-up
    {
        let ui_weak = ui_weak.clone();
        let refresh_tasks = refresh_running_tasks.clone();
        ui.on_close_task(move |task_id| {
            info!("Overview: closing task id={}", task_id);
            send_shell_cmd(&format!("close {}", task_id));
            let ui_weak = ui_weak.clone();
            let refresh_tasks = refresh_tasks.clone();
            slint::Timer::single_shot(Duration::from_millis(80), move || {
                refresh_tasks();
                if let Some(ui) = ui_weak.upgrade() {
                    let tasks = load_running_tasks();
                    if tasks.is_empty() {
                        ui.set_active_mode(0);
                        sync_shell_state("home", ui.get_drawer_open(), 0);
                    } else {
                        let cur_idx = ui.get_overview_index();
                        if cur_idx >= tasks.len() as i32 {
                            ui.set_overview_index((tasks.len() - 1) as i32);
                        }
                        sync_shell_state("overview", ui.get_drawer_open(), ui.get_overview_index() as usize);
                    }
                }
            });
        });
    }

    // Overview carousel index changed (swiped left/right or chevron clicked)
    {
        let ui_weak = ui_weak.clone();
        ui.on_overview_index_changed(move |idx| {
            info!("Overview: carousel index changed to {}", idx);
            if let Some(ui) = ui_weak.upgrade() {
                sync_shell_state("overview", ui.get_drawer_open(), idx as usize);
            }
        });
    }

    // Resume task from overview card tap
    {
        let ui_weak = ui_weak.clone();
        ui.on_resume_active_task(move || {
            info!("Overview: resuming active application");
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_active_mode(3);
                sync_shell_state("app", ui.get_drawer_open(), 0);
            }
        });
    }

    // Nav gesture invoked callback from UI
    {
        let ui_weak = ui_weak.clone();
        let refresh_tasks = refresh_running_tasks.clone();
        ui.on_nav_gesture_invoked(move |action| {
            info!("Nav bar direct action: {action}");
            if let Some(ui) = ui_weak.upgrade() {
                match action.as_str() {
                    "home" => {
                        ui.set_active_mode(0);
                        sync_shell_state("home", ui.get_drawer_open(), 0);
                    }
                    "recents" => {
                        refresh_tasks();
                        ui.set_overview_index(0);
                        ui.set_active_mode(1);
                        sync_shell_state("overview", ui.get_drawer_open(), 0);
                    }
                    _ => {}
                }
            }
        });
    }

    // 7. Dynamic Pinned App Handlers (Homescreen & Dock with Drag-and-Drop + Layout Persistence)
    {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        let reject_timer = Rc::new(Timer::default());

        ui.on_app_pinned_to_dock(move |app_id_slint| {
            let app_id = app_id_slint.as_str().to_string();
            let mut cfg = layout_config.borrow_mut();
            if cfg.dock_apps.len() >= 4 {
                info!("Bottom Dock is full (max 4 slots)! Rejecting pin for '{}'", app_id);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_reject_dock_drop(true);
                    let ui_weak = ui_weak.clone();
                    let reject_timer = reject_timer.clone();
                    reject_timer.start(TimerMode::SingleShot, Duration::from_millis(600), move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_reject_dock_drop(false);
                        }
                    });
                }
            } else {
                if !cfg.dock_apps.contains(&app_id) {
                    info!("Pinning app '{}' to bottom dock", app_id);
                    cfg.dock_apps.push(app_id);
                    save_layout_config(&cfg);
                    drop(cfg);
                    update_ui();
                }
            }
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_app_dropped_on_homescreen(move |app_id_slint, page, sx, sy| {
            let app_id = app_id_slint.as_str().to_string();
            let mut cfg = layout_config.borrow_mut();

            // Calculate target cell (4 columns, 4 rows)
            let col = (((sx - 20.0) / 93.0).round() as i32).clamp(0, 3);
            let origin_y = if page == 0 { 152.0 } else { 86.0 };
            let row = (((sy - origin_y) / 105.0).round() as i32).clamp(0, 3);

            // Check if there is already an item at this target slot
            let existing_app = cfg.homescreen_apps.iter().find(|e| e.page == page && e.x == col && e.y == row).cloned();
            let existing_folder = cfg.folders.iter().find(|f| f.page == page && f.x == col && f.y == row).cloned();

            if let Some(folder) = existing_folder {
                info!("Dropping app '{}' into existing folder '{}' ({})", app_id, folder.name, folder.id);
                cfg.add_to_folder(&folder.id, &app_id);
            } else if let Some(target) = existing_app {
                if target.id != app_id {
                    info!("Merging '{}' and '{}' into new folder at page {}, cell ({}, {})", target.id, app_id, page, col, row);
                    cfg.merge_into_folder(&target.id, &app_id, page, col, row);
                } else {
                    cfg.place_or_move_app(&app_id, page, col, row);
                }
            } else {
                info!("Placing app '{}' at page {}, cell ({}, {})", app_id, page, col, row);
                cfg.place_or_move_app(&app_id, page, col, row);
            }

            save_layout_config(&cfg);
            drop(cfg);
            update_ui();
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_app_pinned_to_homescreen(move |app_id_slint| {
            let app_id = app_id_slint.as_str().to_string();
            let mut cfg = layout_config.borrow_mut();
            if !cfg.homescreen_apps.iter().any(|e| e.id == app_id) {
                info!("Pinning app '{}' to homescreen", app_id);
                let count = (cfg.homescreen_apps.len() + cfg.folders.len()) as i32;
                let page = count / 16;
                let slot = count % 16;
                cfg.place_or_move_app(&app_id, page, slot % 4, slot / 4);
                save_layout_config(&cfg);
                drop(cfg);
                update_ui();
            }
        });
    }

    {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        let apps_list = Arc::clone(&apps_list);
        ui.on_open_folder_requested(move |folder_id_slint| {
            let folder_id = folder_id_slint.as_str();
            if let Some(ui) = ui_weak.upgrade() {
                let cfg = layout_config.borrow();
                if let Some(folder) = cfg.folders.iter().find(|f| f.id == folder_id) {
                    let folder_apps = Rc::new(VecModel::default());
                    let apps = apps_list.read().unwrap();
                    for id in &folder.app_ids {
                        folder_apps.push(resolve_app_item(id, &apps));
                    }
                    ui.set_open_folder_id(folder.id.clone().into());
                    ui.set_open_folder_name(folder.name.clone().into());
                    ui.set_open_folder_apps(ModelRc::from(folder_apps));
                    ui.set_is_folder_open(true);
                }
            }
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_folder_renamed(move |folder_id_slint, new_name_slint| {
            let mut cfg = layout_config.borrow_mut();
            cfg.rename_folder(folder_id_slint.as_str(), new_name_slint.as_str());
            save_layout_config(&cfg);
            drop(cfg);
            update_ui();
        });
    }

    {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        let apps_list = Arc::clone(&apps_list);
        let update_ui = update_layout_models.clone();
        ui.on_folder_item_removed(move |folder_id_slint, app_id_slint| {
            let folder_id = folder_id_slint.as_str();
            let app_id = app_id_slint.as_str();
            let mut cfg = layout_config.borrow_mut();
            cfg.remove_from_folder(folder_id, app_id);
            save_layout_config(&cfg);

            if let Some(ui) = ui_weak.upgrade() {
                if let Some(folder) = cfg.folders.iter().find(|f| f.id == folder_id) {
                    let folder_apps = Rc::new(VecModel::default());
                    let apps = apps_list.read().unwrap();
                    for id in &folder.app_ids {
                        folder_apps.push(resolve_app_item(id, &apps));
                    }
                    ui.set_open_folder_apps(ModelRc::from(folder_apps));
                } else {
                    ui.set_is_folder_open(false);
                }
            }
            drop(cfg);
            update_ui();
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_app_removed_from_dock(move |app_id_slint| {
            let app_id = app_id_slint.as_str();
            let mut cfg = layout_config.borrow_mut();
            if let Some(pos) = cfg.dock_apps.iter().position(|id| id == app_id) {
                info!("Removing app '{}' from bottom dock", app_id);
                cfg.dock_apps.remove(pos);
                save_layout_config(&cfg);
                drop(cfg);
                update_ui();
            }
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_app_removed_from_homescreen(move |app_id_slint| {
            let app_id = app_id_slint.as_str();
            let mut cfg = layout_config.borrow_mut();
            info!("Removing item '{}' from homescreen", app_id);
            cfg.remove_item(app_id);
            save_layout_config(&cfg);
            drop(cfg);
            update_ui();
        });
    }

    // Sliders feedback callbacks
    ui.on_brightness_changed(|val| {
        info!("Display brightness updated: {:.0}%", val * 100.0);
    });

    ui.on_volume_changed(|val| {
        info!("Media volume updated: {:.0}%", val * 100.0);
    });

    {
        let ui_weak = ui_weak.clone();
        ui.on_toggle_convergence(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let current = ui.get_convergence_mode();
                info!("Toggled dual-screen convergence mode: {}", !current);
                ui.set_convergence_mode(!current);
            }
        });
    }

    // -------------------------------------------------------------------------
    // Wallpaper & Style Management (Scanned Folders, File Browser & Persistence)
    // -------------------------------------------------------------------------
    let refresh_wallpapers = {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        Rc::new(move || {
            let folders = layout_config.borrow().wallpaper_folders.clone();
            if let Some(ui) = ui_weak.upgrade() {
                let folders_model = Rc::new(VecModel::default());
                for f in &folders {
                    let is_custom = !f.starts_with("/usr/share/backgrounds") && !f.starts_with("/usr/share/wallpapers");
                    folders_model.push(FolderItem {
                        path: f.clone().into(),
                        is_custom,
                    });
                }
                ui.set_wallpaper_folders(ModelRc::from(folders_model));
            }

            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let scanned_paths = scan_wallpaper_paths_from_folders(&folders);
                info!("Scanned {} wallpaper paths from {:?}", scanned_paths.len(), folders);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        let mut items = Vec::with_capacity(scanned_paths.len());
                        for p in scanned_paths {
                            let img = slint::Image::load_from_path(std::path::Path::new(&p.path))
                                .unwrap_or_default();
                            items.push(WallpaperItem {
                                path: p.path.into(),
                                name: p.name.into(),
                                folder: p.folder.into(),
                                thumbnail: img,
                            });
                        }
                        ui.set_wallpapers(ModelRc::from(Rc::new(VecModel::from(items))));
                    }
                });
            });
        })
    };

fn load_image_buffer_background(path: &std::path::Path) -> Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>> {
    let dyn_img = image::open(path).ok()?;
    let rgb = dyn_img.into_rgb8();
    let (width, height) = rgb.dimensions();
    let raw = rgb.into_raw();
    Some(slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(&raw, width, height))
}

    // Initialize wallpaper from layout config asynchronously
    {
        let cfg = layout_config.borrow();
        let wp_path_str = cfg.wallpaper_path.clone();
        if !wp_path_str.is_empty() {
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let wp_path = std::path::Path::new(&wp_path_str);
                if wp_path.exists() {
                    let t_wp = Instant::now();
                    if let Some(buf) = load_image_buffer_background(wp_path) {
                        info!("Background wallpaper decode completed in {:.2?}", t_wp.elapsed());
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                let img = slint::Image::from_rgb8(buf);
                                ui.set_has_wallpaper(true);
                                ui.set_wallpaper_image(img);
                                ui.set_current_wallpaper_path(wp_path_str.into());
                                info!("Applied active wallpaper to UI");
                            }
                        });
                    }
                }
            });
        }

        let default_browse = if let Ok(home) = std::env::var("HOME") {
            let pic = std::path::PathBuf::from(&home).join("Pictures");
            if pic.exists() {
                pic.to_string_lossy().to_string()
            } else {
                home
            }
        } else {
            "/home/nyl".to_string()
        };
        ui.set_current_browse_path(default_browse.clone().into());
        ui.set_file_entries(ModelRc::from(Rc::new(VecModel::default())));
    }

    // Wallpaper callback: select & apply wallpaper
    {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        ui.on_select_wallpaper(move |path_slint| {
            let path_str = path_slint.as_str().to_string();
            let path = std::path::Path::new(&path_str);
            if path.exists() {
                if let Ok(img) = slint::Image::load_from_path(path) {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_has_wallpaper(true);
                        ui.set_wallpaper_image(img);
                        ui.set_current_wallpaper_path(path_str.clone().into());
                    }
                    let mut cfg = layout_config.borrow_mut();
                    cfg.set_wallpaper(&path_str);
                    save_layout_config(&cfg);
                    info!("Wallpaper updated and saved: {}", path_str);
                }
            }
        });
    }

    // Wallpaper callback: clear / revert to default gradient
    {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        ui.on_clear_wallpaper(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_has_wallpaper(false);
                ui.set_current_wallpaper_path("".into());
            }
            let mut cfg = layout_config.borrow_mut();
            cfg.set_wallpaper("");
            save_layout_config(&cfg);
            info!("Wallpaper cleared (reverted to default ambient iris gradient)");
        });
    }

    // Wallpaper callback: file browser navigation
    {
        let ui_weak = ui_weak.clone();
        ui.on_browse_to(move |target_slint| {
            if let Some(ui) = ui_weak.upgrade() {
                let target = target_slint.as_str();
                let current = ui.get_current_browse_path();
                let next_path = if target == ".." {
                    std::path::Path::new(current.as_str())
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from("/"))
                } else {
                    std::path::PathBuf::from(target)
                };

                if next_path.is_dir() {
                    let path_str = next_path.to_string_lossy().to_string();
                    ui.set_current_browse_path(path_str.into());
                    let entries = read_file_browser_entries(&next_path);
                    ui.set_file_entries(ModelRc::from(Rc::new(VecModel::from(entries))));
                }
            }
        });
    }

    // Wallpaper callback: add scan directory
    {
        let layout_config = Rc::clone(&layout_config);
        let refresh_wp = refresh_wallpapers.clone();
        ui.on_add_scan_folder(move |folder_slint| {
            let folder_str = folder_slint.as_str().to_string();
            let path = std::path::Path::new(&folder_str);
            if path.is_dir() {
                let folder_name_lower = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if folder_name_lower == "screenshots" || folder_name_lower == "screenshot" {
                    warn!("Refusing to add screenshots folder to wallpaper scanner: {}", folder_str);
                    return;
                }
                let mut cfg = layout_config.borrow_mut();
                cfg.add_wallpaper_folder(&folder_str);
                save_layout_config(&cfg);
                drop(cfg);
                info!("Added custom wallpaper directory: {}", folder_str);
                refresh_wp();
            }
        });
    }

    // Wallpaper callback: remove scan directory
    {
        let layout_config = Rc::clone(&layout_config);
        let refresh_wp = refresh_wallpapers.clone();
        ui.on_remove_scan_folder(move |folder_slint| {
            let folder_str = folder_slint.as_str();
            let mut cfg = layout_config.borrow_mut();
            cfg.remove_wallpaper_folder(folder_str);
            save_layout_config(&cfg);
            drop(cfg);
            info!("Removed wallpaper directory: {}", folder_str);
            refresh_wp();
        });
    }

    // Wallpaper callback: manually rescan
    {
        let refresh_wp = refresh_wallpapers.clone();
        ui.on_rescan_wallpapers(move || {
            info!("Rescanning wallpaper directories...");
            refresh_wp();
        });
    }

    // 3. Telemetry Clock and System Service Poller Timer
    let telemetry_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        telemetry_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(1000),
            move || {
                let now = Local::now();
                let time_str = now.format("%H:%M").to_string();
                let date_str = now.format("%A, %b %-d").to_string();

                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_time_str(time_str.into());
                    ui.set_date_str(date_str.into());
                }
            },
        );
    }

    // Connect to Penumbra Services (Battery, Network)
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let (battery_service, network_service) = rt.block_on(async {
        let bat = BatteryService::new().await;
        let net = NetworkService::new().await;
        (bat, net)
    });

    // Initial Telemetry push
    {
        let bat_info = battery_service.current();
        let net_info = network_service.current();
        ui.set_battery_pct(bat_info.percentage as f32);
        ui.set_is_charging(bat_info.is_charging);
        ui.set_wifi_connected(net_info.is_connected);
        ui.set_wifi_signal(net_info.signal_strength as i32);
        ui.set_carrier_str(net_info.ssid.into());
    }

    // Telemetry Watcher Task
    {
        let ui_weak = ui_weak.clone();
        let mut bat_rx = battery_service.subscribe();
        let mut net_rx = network_service.subscribe();
        let _battery_service = battery_service;
        let _network_service = network_service;

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
                let _bat = _battery_service;
                let _net = _network_service;

                loop {
                    tokio::select! {
                        res = bat_rx.changed() => {
                            if res.is_ok() {
                                let info = bat_rx.borrow().clone();
                                let ui_weak = ui_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_weak.upgrade() {
                                        ui.set_battery_pct(info.percentage as f32);
                                        ui.set_is_charging(info.is_charging);
                                    }
                                });
                            } else {
                                break;
                            }
                        }
                        res = net_rx.changed() => {
                            if res.is_ok() {
                                let info = net_rx.borrow().clone();
                                let ui_weak = ui_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_weak.upgrade() {
                                        ui.set_wifi_connected(info.is_connected);
                                        ui.set_wifi_signal(info.signal_strength as i32);
                                        ui.set_carrier_str(info.ssid.into());
                                    }
                                });
                            } else {
                                break;
                            }
                        }
                    }
                }
            });
        });
    }

    // Background worker for full app discovery
    {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);

        std::thread::spawn(move || {
            let start = Instant::now();
            let discovered = discover_installed_apps();
            info!(
                "Background app discovery completed in {:.2?} ({} apps)",
                start.elapsed(),
                discovered.len()
            );

            {
                let mut list = apps_list.write().unwrap();
                *list = discovered;
            }

            let ui_weak = ui_weak.clone();
            let apps_list = Arc::clone(&apps_list);

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    let apps = apps_list.read().unwrap();
                    let mut slint_items = Vec::with_capacity(apps.len());
                    for app in apps.iter() {
                        let (has_icon, img) = if let Some(ref path) = app.icon_path {
                            match slint::Image::load_from_path(path) {
                                Ok(img) => (true, img),
                                Err(_) => (false, slint::Image::default()),
                            }
                        } else {
                            (false, slint::Image::default())
                        };

                        slint_items.push(AppItem {
                            id: app.id.clone().into(),
                            name: app.name.clone().into(),
                            has_icon,
                            icon: img,
                        });
                    }

                    ui.set_installed_apps(ModelRc::from(Rc::new(VecModel::from(slint_items))));
                }
            });
        });
    }

    refresh_wallpapers();

    info!("Starting Slint UI main event loop (412x915 @ 20:9)...");
    ui.run()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_pixel_buffer_send() {
        fn assert_send<T: Send>() {}
        assert_send::<slint::SharedPixelBuffer<slint::Rgb8Pixel>>();
        let buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(&[255, 0, 0], 1, 1);
        let _img = slint::Image::from_rgb8(buffer);
    }

    #[test]
    fn test_shell_root_instantiation_and_properties() {
        let ui = ShellRoot::new().expect("Failed to instantiate ShellRoot window");

        // Assert initial phone viewport dimensions and state
        assert_eq!(ui.get_drawer_y(), -915.0);
        assert!(!ui.get_drawer_open());
        assert_eq!(ui.get_active_mode(), 0);

        // Assert default M3 slider levels
        assert!((ui.get_brightness() - 0.75).abs() < 1e-4);
        assert!((ui.get_volume() - 0.60).abs() < 1e-4);

        // Test telemetry property updates
        ui.set_time_str("18:45".into());
        assert_eq!(ui.get_time_str(), "18:45");

        ui.set_battery_pct(98.0);
        assert_eq!(ui.get_battery_pct(), 98.0);

        // Test toggle state updates
        assert!(ui.get_tile_internet());
        ui.set_tile_internet(false);
        assert!(!ui.get_tile_internet());

        // Test drawer open transition
        ui.set_drawer_y(0.0);
        ui.set_drawer_open(true);
        assert_eq!(ui.get_drawer_y(), 0.0);
        assert!(ui.get_drawer_open());

        // Test dual screen convergence simulation mode
        assert!(ui.get_convergence_mode());
        assert!(ui.get_hdmi_connected());
        ui.set_hdmi_connected(false);
        assert!(!ui.get_hdmi_connected());
        ui.set_convergence_mode(false);
        assert!(!ui.get_convergence_mode());

        // Test app drawer search query property
        assert_eq!(ui.get_app_search_query(), "");
        ui.set_app_search_query("calc".into());
        assert_eq!(ui.get_app_search_query(), "calc");
    }

    #[test]
    fn test_scan_wallpapers_excludes_screenshots() {
        let temp_dir = std::env::temp_dir().join(format!("penumbra_wp_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
        let screenshots_dir = temp_dir.join("Screenshots");
        std::fs::create_dir_all(&screenshots_dir).unwrap();

        // Create a fake wallpaper file and a screenshot file
        let wp_path = temp_dir.join("lake.png");
        let ss_path = screenshots_dir.join("screenshot_app.png");
        let loose_ss_path = temp_dir.join("Screenshot from 2026-10-20.png");

        std::fs::write(&wp_path, b"not a real image but file exists").unwrap();
        std::fs::write(&ss_path, b"not a real image").unwrap();
        std::fs::write(&loose_ss_path, b"not a real image").unwrap();

        // Pass both root temp_dir and direct screenshots_dir
        let scanned = scan_wallpapers_from_folders(&[
            temp_dir.to_string_lossy().to_string(),
            screenshots_dir.to_string_lossy().to_string(),
        ]);

        for item in &scanned {
            let path_str = item.path.as_str().to_lowercase();
            assert!(!path_str.contains("screenshots"), "Should not contain screenshots folder: {}", path_str);
            assert!(!path_str.contains("screenshot"), "Should not contain screenshot file: {}", path_str);
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_app_drawer_search_filtering() {
        let all_apps = vec![
            AppItem { id: "org.gnome.Calculator".into(), name: "Calculator".into(), has_icon: false, icon: slint::Image::default() },
            AppItem { id: "org.gnome.Calendar".into(), name: "Calendar".into(), has_icon: false, icon: slint::Image::default() },
            AppItem { id: "foot".into(), name: "Terminal".into(), has_icon: false, icon: slint::Image::default() },
            AppItem { id: "firefox".into(), name: "Web Browser".into(), has_icon: false, icon: slint::Image::default() },
        ];

        // Search "cal": should match Calculator and Calendar
        let query = "cal";
        let mut matches = Vec::new();
        for item in &all_apps {
            let name_lower = item.name.to_lowercase();
            let id_lower = item.id.to_lowercase();
            if name_lower.contains(query) || id_lower.contains(query) {
                matches.push(item.name.as_str().to_string());
            }
        }
        assert_eq!(matches, vec!["Calculator", "Calendar"]);

        // Search "term": should match Terminal
        let query = "term";
        let mut matches = Vec::new();
        for item in &all_apps {
            let name_lower = item.name.to_lowercase();
            let id_lower = item.id.to_lowercase();
            if name_lower.contains(query) || id_lower.contains(query) {
                matches.push(item.name.as_str().to_string());
            }
        }
        assert_eq!(matches, vec!["Terminal"]);

        // Search non-existent: should be empty
        let query = "xyz123";
        let mut matches = Vec::new();
        for item in &all_apps {
            let name_lower = item.name.to_lowercase();
            let id_lower = item.id.to_lowercase();
            if name_lower.contains(query) || id_lower.contains(query) {
                matches.push(item.name.as_str().to_string());
            }
        }
        assert!(matches.is_empty());
    }
}
