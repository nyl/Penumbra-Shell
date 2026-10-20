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
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_shell::apps::{discover_installed_apps, find_single_app, launch_app, InstalledApp};
use penumbra_shell::config::{
    load_layout_config, save_layout_config, FolderEntry, HomescreenAppEntry,
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
        .with_exclusive_zone(0)
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

    // 5. Background Installed App Discovery
    {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);

        std::thread::spawn(move || {
            let start = Instant::now();
            let discovered = discover_installed_apps();
            info!(
                "Discovered {} apps in {:.2?}",
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
                    tracing::warn!("Failed to launch app '{}': {:?}", app_id, e);
                }
            } else {
                tracing::warn!("Could not resolve app '{}' to launch", app_id);
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

    // 8. Search query filtering
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

    info!("Penumbra Homescreen running event loop on Layer::Background...");
    ui.run()?;
    Ok(())
}
