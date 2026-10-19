use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Local;
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use clap::Parser;
use penumbra_gestures::{NavGesture, NavGestureRecognizer, Spring};
use penumbra_services::{BatteryService, NetworkService};

mod apps;
use apps::{discover_installed_apps, launch_app, InstalledApp};

mod config;
use config::{load_layout_config, save_layout_config, HomescreenAppEntry};

slint::include_modules!();

fn resolve_app_item(id: &str, apps_list: &[InstalledApp]) -> AppItem {
    if let Some(app) = apps_list.iter().find(|a| {
        a.id.eq_ignore_ascii_case(id)
            || a.name.eq_ignore_ascii_case(id)
            || a.id.to_lowercase().contains(&id.to_lowercase())
    }) {
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

    let ui = ShellRoot::new()?;
    ui.set_convergence_mode(args.dual_screen && !args.phone_only);
    sync_shell_state("home", false, 0);

    // Discover installed XDG applications and populate Slint model
    let apps_list = Arc::new(discover_installed_apps());
    let slint_apps = Rc::new(VecModel::default());
    for app in apps_list.iter() {
        let (has_icon, img) = if let Some(ref path) = app.icon_path {
            match slint::Image::load_from_path(path) {
                Ok(img) => (true, img),
                Err(_) => (false, slint::Image::default()),
            }
        } else {
            (false, slint::Image::default())
        };

        slint_apps.push(AppItem {
            id: app.id.clone().into(),
            name: app.name.clone().into(),
            has_icon,
            icon: img,
        });
    }
    ui.set_installed_apps(ModelRc::from(slint_apps));

    let available_ids: Vec<String> = apps_list.iter().map(|a| a.id.clone()).collect();
    let layout_config = Rc::new(std::cell::RefCell::new(load_layout_config(&available_ids)));

    let ui_weak = ui.as_weak();

    // Populate dynamic dock and homescreen models from layout config
    let update_layout_models = {
        let ui_weak = ui_weak.clone();
        let apps_list = Arc::clone(&apps_list);
        let layout_config = Rc::clone(&layout_config);
        Rc::new(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let cfg = layout_config.borrow();
                let dock_model = Rc::new(VecModel::default());
                for id in cfg.dock_apps.iter().take(4) {
                    dock_model.push(resolve_app_item(id, &apps_list));
                }
                ui.set_dock_apps(ModelRc::from(dock_model));

                let home_model = Rc::new(VecModel::default());
                for entry in cfg.homescreen_apps.iter() {
                    home_model.push(resolve_app_item(&entry.id, &apps_list));
                }
                ui.set_homescreen_apps(ModelRc::from(home_model));
            }
        })
    };
    update_layout_models();

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
                        ui.set_active_mode(1);
                        sync_shell_state("overview", ui.get_drawer_open(), 0);
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
            if let Some(target_app) = apps_lookup.iter().find(|a| a.id == id_str || a.name.eq_ignore_ascii_case(id_str)) {
                if let Err(e) = launch_app(target_app, Some("phone")) {
                    tracing::warn!("Failed to launch XDG app '{}': {}", target_app.name, e);
                }
            } else {
                spawn_real_app(id_str, "phone");
            }

            if let Some(ui) = ui_weak.upgrade() {
                ui.set_active_app(app_id_or_name);
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
            if let Some(target_app) = apps_lookup.iter().find(|a| a.id == id_str || a.name.eq_ignore_ascii_case(id_str)) {
                if let Err(e) = launch_app(target_app, Some("desktop")) {
                    tracing::warn!("Failed to launch XDG app '{}' on desktop: {}", target_app.name, e);
                }
            } else {
                spawn_real_app(id_str, "desktop");
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
        ui.on_clear_all_recents(move || {
            info!("Overview: clear all tasks requested");
            send_shell_cmd("close-all");
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_active_mode(0);
                sync_shell_state("home", ui.get_drawer_open(), 0);
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
        ui.on_nav_gesture_invoked(move |action| {
            info!("Nav bar direct action: {action}");
            if let Some(ui) = ui_weak.upgrade() {
                match action.as_str() {
                    "home" => {
                        ui.set_active_mode(0);
                        sync_shell_state("home", ui.get_drawer_open(), 0);
                    }
                    "recents" => {
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
        ui.on_app_pinned_to_homescreen(move |app_id_slint| {
            let app_id = app_id_slint.as_str().to_string();
            let mut cfg = layout_config.borrow_mut();
            if !cfg.homescreen_apps.iter().any(|e| e.id == app_id) {
                info!("Pinning app '{}' to homescreen", app_id);
                let count = cfg.homescreen_apps.len() as i32;
                cfg.homescreen_apps.push(HomescreenAppEntry {
                    id: app_id,
                    x: count % 4,
                    y: count / 4,
                });
                save_layout_config(&cfg);
                drop(cfg);
                update_ui();
            }
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
            if let Some(pos) = cfg.homescreen_apps.iter().position(|e| e.id == app_id) {
                info!("Removing app '{}' from homescreen", app_id);
                cfg.homescreen_apps.remove(pos);
                save_layout_config(&cfg);
                drop(cfg);
                update_ui();
            }
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

    info!("Starting Slint UI main event loop (412x915 @ 20:9)...");
    ui.run()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
