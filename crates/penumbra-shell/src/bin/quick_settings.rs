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
    // Formula: F = -k * (y - y_target) - c * v with stiffness: 300.0, damping: 26.0
    let spring_state = Rc::new(RefCell::new(
        Spring::from_physics(300.0, 26.0, 1.0)
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
                            let final_progress = ((target + 915.0) / 915.0).clamp(0.0, 1.0) as f32;
                            ui.set_expansion_progress(final_progress);
                        }
                        let mut state = ShellState::load();
                        state.drawer_open = target > -900.0;
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

    // Android 17 Kinetic Tracking Model:
    // Direct Touch Tracking:
    // y_touch is displacement from top bar.
    // For y_touch <= 320px: y_shade = y_touch (1:1)
    // For y_touch > 320px: y_shade = 320 + (y_touch - 320) * 0.85
    // In our coordinate space: y_touch = cur_y + 915.0
    {
        let spring_timer = Rc::clone(&spring_timer);
        let spring_state = Rc::clone(&spring_state);
        let ui_weak = ui_weak.clone();
        qs.on_drag_delta(move |dy| {
            spring_timer.borrow().stop();
            if let Some(ui) = ui_weak.upgrade() {
                let cur_offset = ui.get_slide_offset_y() as f64; // in [-915, 0]
                let delta = dy as f64;

                // Recover raw touch displacement y_touch from current offset:
                let cur_disp = cur_offset + 915.0;
                let cur_touch = if cur_disp <= 320.0 {
                    cur_disp
                } else {
                    320.0 + (cur_disp - 320.0) / 0.85
                };

                let new_touch = (cur_touch + delta).clamp(0.0, 915.0 / 0.85);

                // Compute updated shade displacement with progressive resistance
                let new_disp = if new_touch <= 320.0 {
                    new_touch
                } else {
                    320.0 + (new_touch - 320.0) * 0.85
                }.clamp(0.0, 915.0);

                let new_y = new_disp - 915.0; // [-915.0, 0.0]

                ui.set_slide_offset_y(new_y as f32);
                let progress = (new_disp / 915.0).clamp(0.0, 1.0) as f32;
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
            let y_offset = cur_y as f64;
            let v_y = vel as f64;
            // Current shade extension displacement from top bar: y_current in [0, 915]
            let y_current = y_offset + 915.0;

            // Touch Release & Snap Logic:
            // - Abort Snap (Closed): If y_current < 140px and v_y <= 0: target y_target = -915px (offset -915)
            // - Compact Shade Snap: If 140px <= y_current < 450px: target y_target = 320px (offset 320 - 915 = -595px)
            // - Full Expansion Snap: If y_current >= 450px or positive downward flick (v_y > 600px/s): target y_target = 915px (offset 0px)
            let target_offset = if v_y < -300.0 {
                -915.0
            } else if v_y > 600.0 {
                0.0
            } else if y_current < 140.0 && v_y <= 0.0 {
                -915.0
            } else if y_current < 140.0 {
                // Low positive velocity near top: snap to compact shade if pulling down, else closed
                if v_y > 100.0 {
                    -595.0 // 320px shade height
                } else {
                    -915.0
                }
            } else if y_current < 450.0 {
                -595.0 // 320px compact shade
            } else {
                0.0 // 915px fully expanded stage
            };

            info!("Quick Settings: Drag released at y_current={:.1}px, vel={:.1}px/s -> target_offset={:.1}", y_current, v_y, target_offset);
            animate_to(target_offset, v_y);
        });
    }

    // 7. Watch for external drawer_open / drawer_pull_dy trigger from top-bar
    let ipc_timer = Timer::default();
    {
        let animate_to = Rc::clone(&animate_to);
        let spring_state = Rc::clone(&spring_state);
        let last_known_open = Rc::new(RefCell::new(false));
        let last_known_pull_dy = Rc::new(RefCell::new(0.0));
        let ui_weak = ui_weak.clone();

        ipc_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                let state = ShellState::load();
                let mut last_open = last_known_open.borrow_mut();
                let mut last_pull = last_known_pull_dy.borrow_mut();

                // Check pull-down gesture transfer from Top Bar
                if state.drawer_open && state.drawer_pull_dy != *last_pull {
                    *last_pull = state.drawer_pull_dy;
                    let pull = state.drawer_pull_dy;

                    // If user is actively dragging down from the top bar:
                    // Transfer the displacement to the quick settings overlay smoothly
                    if pull > 4.0 {
                        let cur_touch = pull;
                        let shade_disp = if cur_touch <= 320.0 {
                            cur_touch
                        } else {
                            320.0 + (cur_touch - 320.0) * 0.85
                        }.clamp(0.0, 915.0);

                        let new_y = shade_disp - 915.0; // [-915, 0]

                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_slide_offset_y(new_y as f32);
                            let progress = (shade_disp / 915.0).clamp(0.0, 1.0) as f32;
                            ui.set_expansion_progress(progress);
                        }

                        let mut sp = spring_state.borrow_mut();
                        sp.position = new_y;
                        sp.velocity = 0.0;
                    }
                }

                // Check open/close toggle state changes
                if state.drawer_open != *last_open {
                    *last_open = state.drawer_open;
                    let cur_target = spring_state.borrow().target;
                    if state.drawer_open {
                        if cur_target < -600.0 {
                            info!("External trigger: opening Quick Settings shade (compact target: -595px / 320px)");
                            animate_to(-595.0, 400.0);
                        }
                    } else if cur_target > -900.0 {
                        info!("External trigger: closing Quick Settings shade (target: -915px)");
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
