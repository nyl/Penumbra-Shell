//! Standalone Penumbra Quick Settings & Notification Shade binary.
//!
//! A dedicated system control panel and notification center for Penumbra Mobile DE,
//! built with Slint and `layer-shell-slint` on Wayland `zwlr_layer_shell_v1`
//! (`Layer::Overlay`, `exclusive_zone: 0`, `size: 412x915`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Local;
use slint::{ComponentHandle, Timer, TimerMode};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use layer_shell_slint::{Anchor, KeyboardInteractivity, Layer, LayerConfig};
use penumbra_gestures::Spring;
use penumbra_services::NetworkService;
use penumbra_shell::ipc::ShellState;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    info!("Initializing Penumbra Quick Settings via layer-shell-slint...");

    // 1. Configure Wayland Layer Shell Platform backend
    let config = LayerConfig::default()
        .with_namespace("penumbra-quick-settings")
        .with_layer(Layer::Overlay)
        .with_anchor(Anchor::Top | Anchor::Left | Anchor::Right)
        .with_size(412, 915)
        .with_exclusive_zone(0)
        .with_keyboard_interactivity(KeyboardInteractivity::None);

    layer_shell_slint::init(config)?;

    // 2. Instantiate Slint QuickSettingsWindow component
    let qs = QuickSettingsWindow::new()?;
    let ui_weak = qs.as_weak();

    // Initially retracted off-screen: y = -915px
    qs.set_slide_offset_y(-915.0);
    qs.set_expansion_progress(0.0);

    // 3. Underdamped Spring Physics Engine for slider deployment
    // Formula: F = -k * (y - y_target) - c * v with stiffness: 300.0, damping: 28.0
    let spring_state = Rc::new(RefCell::new(
        Spring::from_physics(300.0, 28.0, 1.0)
            .with_position(-915.0)
            .with_target(-915.0),
    ));
    let spring_timer = Rc::new(RefCell::new(Timer::default()));
    let last_spring_time = Rc::new(RefCell::new(Instant::now()));

    let animate_to = {
        let spring_state = Rc::clone(&spring_state);
        let spring_timer = Rc::clone(&spring_timer);
        let last_spring_time = Rc::clone(&last_spring_time);
        let ui_weak = ui_weak.clone();

        move |target: f64, initial_vel: f64| {
            {
                let mut sp = spring_state.borrow_mut();
                sp.target = target;
                sp.velocity = initial_vel;
            }
            *last_spring_time.borrow_mut() = Instant::now();

            let spring_state = Rc::clone(&spring_state);
            let spring_timer_inner = Rc::clone(&spring_timer);
            let last_spring_time = Rc::clone(&last_spring_time);
            let ui_weak = ui_weak.clone();

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
                    let new_y = sp.step(dt);
                    let progress = ((new_y + 915.0) / 915.0).clamp(0.0, 1.0) as f32;

                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_slide_offset_y(new_y as f32);
                        ui.set_expansion_progress(progress);
                    }

                    if sp.snap_if_settled(1.0, 5.0) {
                        spring_timer_inner.borrow().stop();
                        let target = sp.target;
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_slide_offset_y(target as f32);
                            ui.set_expansion_progress(if target >= -1.0 { 1.0 } else { 0.0 });
                        }
                        let mut state = ShellState::load();
                        state.drawer_open = target >= -1.0;
                        let _ = state.save();
                    }
                },
            );
        }
    };

    let animate_to = Rc::new(animate_to);

    // 4. Telemetry Clock (Time + Date)
    let clock_timer = Timer::default();
    {
        let ui_weak = ui_weak.clone();
        let update_time = move || {
            let now = Local::now();
            let time_str = now.format("%H:%M").to_string();
            let date_str = now.format("%A, %b %-d").to_string();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_time_text(time_str.into());
                ui.set_date_text(date_str.into());
            }
        };
        update_time();
        clock_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(1000),
            update_time,
        );
    }

    // 5. Network / Carrier Telemetry via Tokio
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let network_service = rt.block_on(async {
        NetworkService::new().await
    });

    {
        let net_info = network_service.current();
        qs.set_carrier_text(net_info.ssid.into());
        qs.set_tile_internet(net_info.is_connected);
    }

    // 6. User Interaction & Drag Callbacks
    {
        let animate_to = Rc::clone(&animate_to);
        qs.on_close_requested(move || {
            info!("Quick Settings: Close requested -> snapping to -915px");
            animate_to(-915.0, 0.0);
        });
    }

    qs.on_power_action(|| {
        info!("Quick Settings: Power button tapped");
    });

    {
        let animate_to = Rc::clone(&animate_to);
        qs.on_settings_action(move || {
            info!("Quick Settings: Settings button tapped -> retracting");
            animate_to(-915.0, 0.0);
        });
    }

    qs.on_brightness_changed(|val| {
        info!("Quick Settings: Brightness set to {:.2}", val);
    });

    qs.on_volume_changed(|val| {
        info!("Quick Settings: Volume set to {:.2}", val);
    });

    // 1:1 Direct Touch Dragging on the Slider
    {
        let spring_timer = Rc::clone(&spring_timer);
        let spring_state = Rc::clone(&spring_state);
        let ui_weak = ui_weak.clone();
        qs.on_drag_delta(move |dy| {
            spring_timer.borrow().stop();
            if let Some(ui) = ui_weak.upgrade() {
                let cur = ui.get_slide_offset_y() as f64;
                let new_y = (cur + dy as f64).clamp(-915.0, 0.0);
                ui.set_slide_offset_y(new_y as f32);
                let progress = ((new_y + 915.0) / 915.0).clamp(0.0, 1.0) as f32;
                ui.set_expansion_progress(progress);

                let mut sp = spring_state.borrow_mut();
                sp.position = new_y;
                sp.velocity = 0.0;

                let mut state = ShellState::load();
                if !state.drawer_open && new_y > -910.0 {
                    state.drawer_open = true;
                    let _ = state.save();
                }
            }
        });
    }

    {
        let animate_to = Rc::clone(&animate_to);
        qs.on_drag_released(move |cur_y, vel| {
            let y = cur_y as f64;
            let v = vel as f64;
            // Displacement from fully retracted (-915.0):
            let disp = y + 915.0;

            // Snap logic per requirement:
            // - If release displacement < 120px and velocity is low: snaps back to y = -915px (closed)
            // - If flicked downward (v > 300) or pulled past 180px: snaps smoothly to y = 0px (fully deployed)
            let target = if v < -300.0 {
                -915.0
            } else if v > 300.0 || disp >= 180.0 {
                0.0
            } else if disp < 120.0 {
                -915.0
            } else {
                0.0
            };

            info!("Quick Settings: Drag released at y={:.1}, vel={:.1} -> target={:.1}", y, v, target);
            animate_to(target, v);
        });
    }

    // 7. Watch for external drawer_open trigger from top-bar
    let ipc_timer = Timer::default();
    {
        let animate_to = Rc::clone(&animate_to);
        let spring_state = Rc::clone(&spring_state);
        let last_known_open = Rc::new(RefCell::new(false));

        ipc_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(32),
            move || {
                let state = ShellState::load();
                let mut last = last_known_open.borrow_mut();
                if state.drawer_open != *last {
                    *last = state.drawer_open;
                    let cur_target = spring_state.borrow().target;
                    if state.drawer_open && cur_target != 0.0 {
                        info!("External trigger: opening Quick Settings shade");
                        animate_to(0.0, 400.0);
                    } else if !state.drawer_open && cur_target != -915.0 {
                        info!("External trigger: closing Quick Settings shade");
                        animate_to(-915.0, -400.0);
                    }
                }
            },
        );
    }

    info!("Penumbra Quick Settings running event loop.");
    qs.run()?;

    Ok(())
}
