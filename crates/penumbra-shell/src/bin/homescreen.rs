//! Standalone Penumbra Homescreen binary.
//!
//! A dedicated mobile homescreen and launcher for Penumbra Mobile DE, built with Slint and
//! `layer-shell-slint` on Wayland `zwlr_layer_shell_v1` (`Layer::Background`, fullscreen 412x915).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::Local;
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_gestures::Spring;
use penumbra_shell::apps::{discover_installed_apps, find_single_app, launch_app, InstalledApp};
use penumbra_shell::config::{
    load_layout_config, save_layout_config, FolderEntry, HomescreenAppEntry,
};
use penumbra_shell::ipc::{read_windows, send_command, PhoneMode, ShellState};
use penumbra_shell::wallpaper::{
    read_file_browser_entries, scan_wallpaper_paths_from_folders,
};

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Initializing Penumbra Homescreen via layer-shell-slint on Layer::Background...");

    // 1. Initialize Wayland Layer Shell Platform backend on Background layer
    let config = LayerConfig::default()
        .with_namespace("penumbra-homescreen")
        .with_layer(Layer::Background)
        .with_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right)
        .with_size(412, 915)
        .with_exclusive_zone(-1)
        .with_keyboard_interactivity(KeyboardInteractivity::None);

    layer_shell_slint::init(config)?;

    // 2. Instantiate Slint Homescreen UI
    let ui = HomescreenWindow::new()?;
    let ui_weak = ui.as_weak();

    // 3. Telemetry Clock (updates every second)
    let clock_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        let update_time = move || {
            let now = Local::now();
            let time_str = now.format("%H:%M").to_string();
            let date_str = now.format("%A, %b %-d").to_string();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_time_str(time_str.into());
                ui.set_date_str(date_str.into());
            }
        };
        update_time();
        clock_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(1000),
            update_time,
        );
    }

    // 4. Load persisted Layout Config
    let layout_config = Rc::new(RefCell::new(load_layout_config(&[])));
    let apps_list: Arc<RwLock<Vec<InstalledApp>>> = Arc::new(RwLock::new(Vec::new()));

    // Initial Wallpaper Load
    {
        let cfg = layout_config.borrow();
        if !cfg.wallpaper_path.is_empty() {
            let wp_path = std::path::Path::new(&cfg.wallpaper_path);
            if wp_path.exists() {
                if let Ok(img) = slint::Image::load_from_path(wp_path) {
                    ui.set_has_wallpaper(true);
                    ui.set_wallpaper_image(img);
                    ui.set_current_wallpaper_path(cfg.wallpaper_path.clone().into());
                }
            }
        }
    }

    // Helper closure to synchronize Slint models with LayoutConfig
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

    update_layout_models();

    // Wallpaper Refresh Helper
    let refresh_wallpapers = {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        Rc::new(move || {
            let folders = layout_config.borrow().wallpaper_folders.clone();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let scanned_paths = scan_wallpaper_paths_from_folders(&folders);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        let folder_models = Rc::new(VecModel::default());
                        for f in &folders {
                            folder_models.push(FolderItem {
                                path: f.clone().into(),
                                is_custom: true,
                            });
                        }
                        ui.set_wallpaper_folders(ModelRc::from(folder_models));

                        let wp_models = Rc::new(VecModel::default());
                        for p in scanned_paths {
                            if let Ok(img) = slint::Image::load_from_path(std::path::Path::new(&p.path)) {
                                wp_models.push(WallpaperItem {
                                    path: p.path.into(),
                                    name: p.name.into(),
                                    folder: p.folder.into(),
                                    thumbnail: img,
                                });
                            }
                        }
                        ui.set_wallpapers(ModelRc::from(wp_models));
                    }
                });
            });
        })
    };

    refresh_wallpapers();

    // Default browse path for file picker
    let default_browse = std::env::var("HOME").unwrap_or_else(|_| "/home/nyl".to_string());
    ui.set_current_browse_path(default_browse.into());

    // 5. Asynchronous App Discovery
    {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);

        std::thread::spawn(move || {
            let start = Instant::now();
            let discovered = discover_installed_apps();
            info!(
                "Discovered {} applications in {:.2?}",
                discovered.len(),
                start.elapsed()
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
                    ui.set_installed_apps(ModelRc::new(VecModel::from(slint_items)));
                }
            });
        });
    }

    // 6. App Click / Launching
    {
        let apps_list = Arc::clone(&apps_list);
        ui.on_app_clicked(move |app_id_slint| {
            let app_id = app_id_slint.as_str();
            info!("Launching application from homescreen: '{}'", app_id);
            let app_opt = {
                let apps = apps_list.read().unwrap();
                apps.iter().find(|a| a.id.eq_ignore_ascii_case(app_id) || a.name.eq_ignore_ascii_case(app_id)).cloned()
            }.or_else(|| find_single_app(app_id));

            if let Some(ref target_app) = app_opt {
                if let Err(e) = launch_app(target_app, Some("phone")) {
                    warn!("Failed to launch app '{}': {:?}", app_id, e);
                } else {
                    let mut state = ShellState::load();
                    state.mode = PhoneMode::App;
                    state.drawer_open = false;
                    let _ = state.save();
                }
            } else {
                warn!("Could not resolve app '{}' to launch", app_id);
            }
        });
    }

    // 7. Dock & Homescreen Drag-and-Drop Handlers
    {
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        let reject_timer = Rc::new(Timer::default());

        ui.on_app_pinned_to_dock(move |app_id_slint| {
            let app_id = app_id_slint.as_str().to_string();
            let mut cfg = layout_config.borrow_mut();
            if cfg.dock_apps.len() >= 4 {
                info!("Bottom Dock is full! Rejecting pin for '{}'", app_id);
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
            } else if !cfg.dock_apps.contains(&app_id) {
                info!("Pinning app '{}' to bottom dock", app_id);
                cfg.dock_apps.push(app_id);
                save_layout_config(&cfg);
                drop(cfg);
                update_ui();
            }
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_app_dropped_on_homescreen(move |app_id_slint, page, sx, sy| {
            let app_id = app_id_slint.as_str().to_string();
            let mut cfg = layout_config.borrow_mut();

            let col = (((sx - 20.0) / 93.0).round() as i32).clamp(0, 3);
            let origin_y = if page == 0 { 152.0 } else { 86.0 };
            let row = (((sy - origin_y) / 105.0).round() as i32).clamp(0, 3);

            let existing_folder = cfg.folders.iter().find(|f| f.page == page && f.x == col && f.y == row).cloned();
            let existing_app = cfg.homescreen_apps.iter().find(|e| e.page == page && e.x == col && e.y == row).cloned();

            if let Some(folder) = existing_folder {
                if !folder.app_ids.contains(&app_id) {
                    if let Some(f) = cfg.folders.iter_mut().find(|f| f.id == folder.id) {
                        f.app_ids.push(app_id.clone());
                    }
                    cfg.homescreen_apps.retain(|e| e.id != app_id);
                }
            } else if let Some(target_app) = existing_app {
                if target_app.id != app_id {
                    let folder_id = format!("folder_{}_{}", col, row);
                    cfg.homescreen_apps.retain(|e| e.id != target_app.id && e.id != app_id);
                    cfg.folders.push(FolderEntry {
                        id: folder_id,
                        name: "Folder".to_string(),
                        page,
                        x: col,
                        y: row,
                        app_ids: vec![target_app.id, app_id.clone()],
                    });
                }
            } else {
                cfg.homescreen_apps.retain(|e| e.id != app_id);
                cfg.homescreen_apps.push(HomescreenAppEntry {
                    id: app_id,
                    page,
                    x: col,
                    y: row,
                });
            }

            save_layout_config(&cfg);
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
            cfg.dock_apps.retain(|id| id != app_id);
            save_layout_config(&cfg);
            drop(cfg);
            update_ui();
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_app_removed_from_homescreen(move |app_id_slint| {
            let app_id = app_id_slint.as_str();
            let mut cfg = layout_config.borrow_mut();
            cfg.homescreen_apps.retain(|e| e.id != app_id);
            cfg.folders.retain(|f| f.id != app_id);
            save_layout_config(&cfg);
            drop(cfg);
            update_ui();
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let update_ui = update_layout_models.clone();
        ui.on_folder_renamed(move |fid_slint, name_slint| {
            let fid = fid_slint.as_str();
            let name = name_slint.as_str();
            let mut cfg = layout_config.borrow_mut();
            if let Some(f) = cfg.folders.iter_mut().find(|f| f.id == fid) {
                f.name = name.to_string();
                save_layout_config(&cfg);
            }
            drop(cfg);
            update_ui();
        });
    }

    // 8. Folders: Open and remove items
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
        let ui_weak = ui_weak.clone();
        let layout_config = Rc::clone(&layout_config);
        let apps_list = Arc::clone(&apps_list);
        let update_ui = update_layout_models.clone();
        ui.on_folder_item_removed(move |folder_id_slint, app_id_slint| {
            let folder_id = folder_id_slint.as_str();
            let app_id = app_id_slint.as_str();
            let mut cfg = layout_config.borrow_mut();
            if let Some(folder) = cfg.folders.iter_mut().find(|f| f.id == folder_id) {
                folder.app_ids.retain(|id| id != app_id);
                if folder.app_ids.is_empty() {
                    let fid = folder.id.clone();
                    cfg.folders.retain(|f| f.id != fid);
                }
                save_layout_config(&cfg);
            }

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

    // 9. Search query filtering & Accepted launch
    {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);
        ui.on_app_search_query_changed(move |query_slint| {
            let query = query_slint.as_str().to_lowercase();
            if let Some(ui) = ui_weak.upgrade() {
                let apps = apps_list.read().unwrap();
                let filtered: Vec<AppItem> = apps
                    .iter()
                    .filter(|a| query.is_empty() || a.name.to_lowercase().contains(&query) || a.id.to_lowercase().contains(&query))
                    .map(|app| {
                        let (has_icon, img) = if let Some(ref path) = app.icon_path {
                            match slint::Image::load_from_path(path) {
                                Ok(img) => (true, img),
                                Err(_) => (false, slint::Image::default()),
                            }
                        } else {
                            (false, slint::Image::default())
                        };
                        AppItem {
                            id: app.id.clone().into(),
                            name: app.name.clone().into(),
                            has_icon,
                            icon: img,
                        }
                    })
                    .collect();
                ui.set_installed_apps(ModelRc::new(VecModel::from(filtered)));
            }
        });
    }

    {
        let apps_list = Arc::clone(&apps_list);
        ui.on_app_search_accepted(move |query_slint| {
            let query = query_slint.as_str().trim().to_lowercase();
            if query.is_empty() {
                return;
            }
            let app_opt = {
                let apps = apps_list.read().unwrap();
                apps.iter().find(|a| a.name.to_lowercase().starts_with(&query) || a.id.to_lowercase().contains(&query)).cloned()
            };
            if let Some(target) = app_opt {
                info!("Launching search match '{}'", target.name);
                if let Err(e) = launch_app(&target, Some("phone")) {
                    warn!("Failed to launch app: {:?}", e);
                } else {
                    let mut state = ShellState::load();
                    state.mode = PhoneMode::App;
                    state.drawer_open = false;
                    let _ = state.save();
                }
            }
        });
    }

    // 10. Wallpaper Callbacks
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
            info!("Wallpaper cleared");
        });
    }

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
                    let raw_entries = read_file_browser_entries(&next_path);
                    let entries: Vec<FileEntry> = raw_entries
                        .into_iter()
                        .map(|e| FileEntry {
                            name: e.name.into(),
                            path: e.path.into(),
                            is_dir: e.is_dir,
                            is_image: e.is_image,
                            size_str: e.size_str.into(),
                        })
                        .collect();
                    ui.set_file_entries(ModelRc::from(Rc::new(VecModel::from(entries))));
                }
            }
        });
    }

    {
        let layout_config = Rc::clone(&layout_config);
        let refresh_wp = refresh_wallpapers.clone();
        ui.on_add_scan_folder(move |folder_slint| {
            let folder_str = folder_slint.as_str().to_string();
            let path = std::path::Path::new(&folder_str);
            if path.is_dir() {
                let mut cfg = layout_config.borrow_mut();
                cfg.add_wallpaper_folder(&folder_str);
                save_layout_config(&cfg);
                drop(cfg);
                info!("Added custom wallpaper directory: {}", folder_str);
                refresh_wp();
            }
        });
    }

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

    {
        let refresh_wp = refresh_wallpapers.clone();
        ui.on_rescan_wallpapers(move || {
            info!("Rescanning wallpaper directories...");
            refresh_wp();
        });
    }

    // 11. Tasks & Overview synchronization
    {
        ui.on_select_task(move |task_id| {
            info!("Overview: Focusing task {}", task_id);
            let _ = send_command(&format!("focus {}", task_id));
            let mut state = ShellState::load();
            state.mode = PhoneMode::App;
            let _ = state.save();
        });
    }

    {
        ui.on_close_task(move |task_id| {
            info!("Overview: Closing task {}", task_id);
            let _ = send_command(&format!("close {}", task_id));
        });
    }

    {
        ui.on_clear_all_recents(move || {
            info!("Overview: Closing all tasks");
            let _ = send_command("close-all");
            let mut state = ShellState::load();
            state.mode = PhoneMode::Home;
            let _ = state.save();
        });
    }

    {
        ui.on_resume_active_task(move || {
            let mut state = ShellState::load();
            state.mode = PhoneMode::App;
            let _ = state.save();
        });
    }

    // 11. Spring physics engine for overview carousel flick momentum & magnetic snap
    // Underdamped spring-damper system: stiffness 280.0, damping: 26.0 (F = -k*(x - target) - c*v)
    let spring_state = Rc::new(RefCell::new(Spring::from_physics(280.0, 26.0, 1.0)));
    let spring_timer = Rc::new(RefCell::new(Timer::default()));
    let last_spring_time = Rc::new(RefCell::new(Instant::now()));

    {
        let spring_timer = Rc::clone(&spring_timer);
        ui.on_overview_scroll_changed(move |scroll, is_dragging| {
            if is_dragging {
                spring_timer.borrow().stop();
            }
            let mut state = ShellState::load();
            state.overview_scroll = scroll as f64;
            state.overview_index = (scroll.round().max(0.0)) as usize;
            state.is_dragging = is_dragging;
            let _ = state.save();
        });
    }

    {
        let ui_weak = ui_weak.clone();
        let spring_state = Rc::clone(&spring_state);
        let spring_timer = Rc::clone(&spring_timer);
        let last_spring_time = Rc::clone(&last_spring_time);

        ui.on_overview_drag_released(move |current_scroll, velocity_px_s| {
            let release_scroll = current_scroll as f64;
            // 341px per card step (321px width + 20px gap):
            let step = 341.0;
            // velocity_px_s is positive when dragging right (scroll increases towards older tasks),
            // negative when dragging left (scroll decreases towards newer tasks).
            let v_scroll = velocity_px_s as f64 / step;

            let windows = read_windows();
            let max_idx = (windows.len().saturating_sub(1)) as f64;

            // Evaluate velocity and distance to automatically snap the nearest card:
            let target = if v_scroll.abs() > 0.8 {
                let projected = release_scroll + (v_scroll * 0.22);
                projected.round().clamp(0.0, max_idx)
            } else {
                release_scroll.round().clamp(0.0, max_idx)
            };

            {
                let mut sp = spring_state.borrow_mut();
                sp.position = release_scroll;
                sp.velocity = v_scroll;
                sp.target = target;
            }
            *last_spring_time.borrow_mut() = Instant::now();

            let ui_weak = ui_weak.clone();
            let spring_state = Rc::clone(&spring_state);
            let spring_timer_inner = Rc::clone(&spring_timer);
            let last_spring_time = Rc::clone(&last_spring_time);

            spring_timer.borrow().start(
                TimerMode::Repeated,
                Duration::from_millis(16),
                move || {
                    let dt = {
                        let mut t = last_spring_time.borrow_mut();
                        let elapsed = t.elapsed().as_secs_f64().clamp(0.001, 0.05);
                        *t = Instant::now();
                        elapsed
                    };

                    let mut sp = spring_state.borrow_mut();
                    let new_pos = sp.step(dt);

                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_overview_scroll(new_pos as f32);
                    }

                    let mut state = ShellState::load();
                    state.overview_scroll = new_pos;
                    state.is_dragging = false;

                    if sp.snap_if_settled(0.002, 0.02) {
                        spring_timer_inner.borrow().stop();
                        let final_idx = sp.target as usize;
                        state.overview_scroll = sp.target;
                        state.overview_index = final_idx;
                        let _ = state.save();
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_overview_scroll(sp.target as f32);
                            ui.set_overview_index(final_idx as i32);
                        }
                    } else {
                        let _ = state.save();
                    }
                },
            );
        });
    }

    {
        ui.on_overview_index_changed(move |idx| {
            let mut state = ShellState::load();
            state.overview_index = idx as usize;
            state.overview_scroll = idx as f64;
            state.is_dragging = false;
            let _ = state.save();
        });
    }

    {
        ui.on_active_card_dy_changed(move |dy| {
            let mut state = ShellState::load();
            state.active_card_dy = dy as f64;
            let _ = state.save();
        });
    }

    {
        ui.on_exit_overview(move || {
            info!("Overview: Exiting to Home");
            let mut state = ShellState::load();
            state.mode = PhoneMode::Home;
            state.active_card_dy = 0.0;
            let _ = state.save();
        });
    }

    // 12. Periodic Task List & ShellState synchronization
    let task_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);
        let spring_state = Rc::clone(&spring_state);
        task_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(50),
            move || {
                let state = ShellState::load();
                if let Some(ui) = ui_weak.upgrade() {
                    let current_mode = ui.get_active_mode();
                    let target_mode = match state.mode {
                        PhoneMode::Home => {
                            if current_mode == 2 {
                                2 // preserve local app drawer open
                            } else {
                                0
                            }
                        }
                        PhoneMode::Overview => 1,
                        PhoneMode::App => 3,
                    };

                    if current_mode != target_mode {
                        info!("Homescreen sync active_mode: {} -> {}", current_mode, target_mode);
                        ui.set_active_mode(target_mode);
                    }

                    if state.mode == PhoneMode::Overview {
                        if !ui.get_is_dragging_carousel() {
                            let cur_scroll = ui.get_overview_scroll() as f64;
                            let target_scroll = state.overview_index as f64;
                            if (cur_scroll - target_scroll).abs() > 0.05 && spring_state.borrow().is_settled(0.01, 0.05) {
                                ui.set_overview_index(state.overview_index as i32);
                                ui.set_overview_scroll(state.overview_index as f32);
                            }
                        }
                    }

                    let windows = read_windows();
                    let apps = apps_list.read().unwrap();
                    let tasks_model = Rc::new(VecModel::default());
                    for win in windows {
                        let (name, has_icon, icon) = if let Some(app) = apps.iter().find(|a| {
                            a.id.eq_ignore_ascii_case(&win.app_id) || a.name.eq_ignore_ascii_case(&win.app_id)
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
                            let display = if !win.title.is_empty() {
                                win.title.clone()
                            } else {
                                win.app_id.clone()
                            };
                            (display, false, slint::Image::default())
                        };

                        tasks_model.push(AppCardData {
                            id: win.id as i32,
                            app_id: win.app_id.into(),
                            name: name.into(),
                            has_icon,
                            icon,
                        });
                    }
                    ui.set_running_tasks(ModelRc::from(tasks_model));
                }
            },
        );
    }

    info!("Penumbra Homescreen running event loop on Layer::Background...");
    ui.run()?;
    Ok(())
}
