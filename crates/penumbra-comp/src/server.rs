//! Smithay-based Wayland compositor engine with real dual-output policy routing.

use std::os::unix::io::OwnedFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info};

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, InputEvent, Keycode, KeyboardKeyEvent, PointerAxisEvent,
            PointerButtonEvent,
        },
        renderer::{
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                utils::CropRenderElement,
                Kind,
            },
            gles::GlesRenderer,
            utils::{draw_render_elements, on_commit_buffer_handler},
            Color32F, Frame, Renderer,
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_data_device, delegate_input_method_manager,
    delegate_layer_shell, delegate_output, delegate_seat, delegate_shm,
    delegate_text_input_manager, delegate_virtual_keyboard_manager, delegate_xdg_shell,
    desktop::{layer_map_for_output, space::Space, LayerSurface, Window},
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
        Seat, SeatHandler, SeatState,
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{
                wl_buffer,
                wl_output::WlOutput,
                wl_seat,
                wl_surface::{self, WlSurface},
            },
            Client, Display, ListeningSocket,
        },
        winit::platform::pump_events::PumpStatus,
    },
    utils::{IsAlive, Physical, Point, Rectangle, Serial, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            with_states, with_surface_tree_downward, CompositorClientState, CompositorHandler,
            CompositorState, SurfaceAttributes, TraversalAction,
        },
        input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface as ImPopupSurface},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::{
            wlr_layer::{
                Layer, LayerSurface as WlrLayerSurface, WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::{
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
                XdgToplevelSurfaceData,
            },
        },
        shm::{ShmHandler, ShmState},
        text_input::{TextInputManagerState, TextInputSeat},
        virtual_keyboard::VirtualKeyboardManagerState,
    },
};

use crate::args::CliArgs;
use crate::phone::{
    self, card_transform, parse_commands, resolve_app_icon_target, runtime_path, task_line,
    write_atomic, PhoneMode, ShellCommand, ShellState, APP_H, APP_Y, CARD_H, CARD_STEP, NAV_BAR_H,
    NAV_BAR_Y, PHONE_H, PHONE_W, STATUS_BAR_H,
};
use crate::policy::{AsymmetricOutputManager, OutputRole};
use penumbra_gestures::{
    arbitrate_horizontal_release, arbitrate_release, compute_home_collapse_scale,
    compute_scrim_alpha, compute_window_scale, GestureStateMachine, NavigationGesture,
    NavigationTarget, Spring, SpringSolver,
};

/// Client-specific compositor state.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        info!("Wayland client connection initialized");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        info!("Wayland client disconnected");
    }
}

/// Server state managing Wayland protocols, dual outputs, and client surfaces.
pub struct PenumbraServerState {
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub output_manager_state: OutputManagerState,
    pub text_input_state: TextInputManagerState,
    pub input_method_state: InputMethodManagerState,
    pub virtual_keyboard_state: VirtualKeyboardManagerState,
    pub last_text_input_active: bool,

    pub seat: Seat<Self>,
    pub space: Space<Window>,
    pub root_shell: Option<Window>,
    pub output_a: Output,
    pub output_b: Option<Output>,
    pub is_convergence_active: bool,
    pub is_native: bool,
    pub pointer_location: Point<f64, smithay::utils::Logical>,

    /// Phone (Output A) tasks in MRU order. `[0]` is the active app.
    pub phone_tasks: Vec<PhoneTask>,
    pub next_task_id: u32,
    /// Latest UI state published by penumbra-shell.
    pub shell_state: ShellState,
    /// Animated overview carousel position (lerps towards `shell_state.overview_index`).
    pub overview_scroll: f64,
    /// Last task list written to the shell, to avoid redundant writes.
    pub last_tasks_published: String,
    /// Logical canvas for Output A; layer surfaces are sized to this, not the raw mode.
    pub phone_logical_size: (i32, i32),
    pub screen_blanked: bool,
    pub saved_brightness: u32,

    /// Whether a bottom nav bar gesture is currently active (finger down and dragging).
    pub nav_gesture_active: bool,
    /// Normalized gesture progress t in [0.0, 1.05].
    pub nav_gesture_progress: f64,
    /// Vertical drag displacement (pixels, negative = dragging up).
    pub nav_gesture_dy: f64,
    /// Horizontal rubber-band displacement while pulling up (pixels).
    pub nav_gesture_dx: f64,
    /// Instantaneous vertical velocity (pixels/second).
    pub nav_gesture_vy: f64,
    /// Whether user is currently swiping horizontally along nav bar (Quick Switch).
    pub nav_switch_active: bool,
    /// Horizontal offset during Quick Switch drag or spring animation (pixels).
    pub nav_switch_dx: f64,
    /// Instantaneous horizontal velocity (pixels/second).
    pub nav_switch_vx: f64,
    /// Spring solver animating vertical progress towards 1.0 (Overview/Home) or 0.0 (Fullscreen).
    pub nav_spring: Option<SpringSolver>,
    /// Target state when nav_spring settles.
    pub nav_target: Option<NavigationTarget>,
    /// Spring solver animating horizontal offset dx during lateral card switching.
    pub nav_switch_spring: Option<SpringSolver>,
    /// Target state when nav_switch_spring settles.
    pub nav_switch_target: Option<NavigationTarget>,
    /// Spring solver animating horizontal carousel scroll position with Material 3 physics.
    pub overview_scroll_spring: Option<SpringSolver>,
    /// Last timestamp the nav spring was updated.
    pub last_nav_spring_tick: Option<Instant>,
    /// Pre-blurred wallpaper cache for fast zero-cost background compositing.
    pub blurred_wallpaper: Option<crate::wallpaper_blur::BlurredWallpaperCache>,
    /// GPU memory render buffer for blurred wallpaper.
    pub blurred_wallpaper_buffer: Option<smithay::backend::renderer::element::memory::MemoryRenderBuffer>,
    /// Pre-computed 28px Material 3 squircle corner masks.
    pub card_corner_masks: crate::card_rounding::CardCornerMasks,
}

/// A Wayland toplevel living on the phone output.
pub struct PhoneTask {
    pub id: u32,
    pub window: Window,
}

impl PhoneTask {
    pub fn app_id(&self) -> String {
        self.window
            .toplevel()
            .map(|t| {
                with_states(t.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|d| d.lock().ok())
                        .and_then(|d| d.app_id.clone())
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default()
    }
}

impl PenumbraServerState {
    fn root_surface(&self) -> Option<WlSurface> {
        self.root_shell
            .as_ref()
            .and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()))
    }

    pub(crate) fn active_phone_surface(&self) -> Option<WlSurface> {
        self.phone_tasks
            .first()
            .and_then(|t| t.window.toplevel().map(|t| t.wl_surface().clone()))
    }

    fn is_phone_task(&self, window: &Window) -> bool {
        self.phone_tasks.iter().any(|t| &t.window == window)
    }

    /// Whether an app currently occupies the phone inset (Layer 1 is visible).
    /// Also true during a drag-down navigation gesture from Overview → App (fullscreen restore).
    pub(crate) fn phone_app_visible(&self) -> bool {
        if self.phone_tasks.is_empty() {
            return false;
        }
        self.shell_state.mode == PhoneMode::App
            // During Overview drag-down or settle animation, we render the app
            // so it animates smoothly expanding back to fullscreen.
            || (self.shell_state.mode == PhoneMode::Overview && (self.nav_gesture_active || self.is_nav_animating()))
    }

    /// Resolves the surface receiving input at `pos` on the phone output.
    /// Returns the surface and its origin in global compositor coordinates.
    pub(crate) fn phone_focus_under(
        &self,
        pos: Point<f64, smithay::utils::Logical>,
    ) -> Option<(WlSurface, Point<f64, smithay::utils::Logical>)> {
        let map_a = layer_map_for_output(&self.output_a);

        // 1. Overlay layer (highest priority - active when drawer, power dialog, or volume modal is open)
        if self.shell_state.drawer_open
            || self.shell_state.power_dialog_open
            || self.shell_state.volume_modal_open
        {
            if let Some(l) = map_a.layer_under(Layer::Overlay, pos) {
                let geo = map_a.layer_geometry(l).unwrap_or_default();
                return Some((
                    l.wl_surface().clone(),
                    (geo.loc.x as f64, geo.loc.y as f64).into(),
                ));
            }
        }

        // 2. Top layer (e.g. status bar: penumbra-top-bar, nav bar, keyboard)
        if let Some(l) = map_a.layer_under(Layer::Top, pos) {
            let ns = l.namespace();
            let is_penumbra_osk = ns == "penumbra-keyboard";
            if is_penumbra_osk {
                let keyboard_open = {
                    let path = runtime_path(phone::SHELL_STATE_FILE).with_file_name("penumbra-keyboard-state");
                    std::fs::read_to_string(path).map(|c| c.contains("open=1")).unwrap_or(false)
                };
                if self.shell_state.drawer_open || !keyboard_open {
                    // Do not focus keyboard when drawer is open or keyboard is hidden
                } else {
                    let geo = map_a.layer_geometry(l).unwrap_or_default();
                    return Some((
                        l.wl_surface().clone(),
                        (geo.loc.x as f64, geo.loc.y as f64).into(),
                    ));
                }
            } else {
                let geo = map_a.layer_geometry(l).unwrap_or_default();
                return Some((
                    l.wl_surface().clone(),
                    (geo.loc.x as f64, geo.loc.y as f64).into(),
                ));
            }
        }

        // 3. Normal window (running phone app in the inset)
        let in_app_inset = pos.y >= APP_Y as f64 && pos.y < NAV_BAR_Y as f64;
        if self.phone_app_visible()
            && !self.shell_state.drawer_open
            && !self.shell_state.power_dialog_open
            && in_app_inset
        {
            if let Some(surface) = self.active_phone_surface() {
                return Some((surface, (0.0, APP_Y as f64).into()));
            }
        }

        // 4. Bottom layer
        if let Some(l) = map_a.layer_under(Layer::Bottom, pos) {
            let geo = map_a.layer_geometry(l).unwrap_or_default();
            return Some((
                l.wl_surface().clone(),
                (geo.loc.x as f64, geo.loc.y as f64).into(),
            ));
        }

        // 5. Background layer (e.g. penumbra-homescreen)
        if let Some(l) = map_a.layer_under(Layer::Background, pos) {
            return Some((l.wl_surface().clone(), (0.0, 0.0).into()));
        }

        // 6. Legacy root_shell (if running legacy monolithic shell)
        self.root_surface().map(|s| (s, (0.0, 0.0).into()))
    }

    /// Advances all navigation and overview spring animations in lockstep.
    pub(crate) fn advance_animations(&mut self) {
        let now = Instant::now();
        let dt = if let Some(last) = self.last_nav_spring_tick {
            now.duration_since(last).as_secs_f64().clamp(0.001, 0.05)
        } else {
            0.016
        };
        self.last_nav_spring_tick = Some(now);
        self.advance_nav_spring(dt);
        self.advance_overview_scroll(dt);
    }

    /// Eases the animated overview carousel towards the shell's target index.
    /// While the user drags, it tracks the finger 1:1; on release it decelerates
    /// using Material 3 Expressive spring physics into a magnetic lock.
    pub(crate) fn advance_overview_scroll(&mut self, dt: f64) {
        if self.shell_state.is_dragging {
            self.overview_scroll = self.shell_state.overview_scroll;
            self.overview_scroll_spring = None;
        } else if let Some(ref mut spring) = self.overview_scroll_spring {
            spring.step_analytical(dt);
            self.overview_scroll = spring.position;
            let is_settled = spring.is_settled(0.005, 0.05);
            let target = spring.target;
            if is_settled {
                self.overview_scroll = target;
                self.overview_scroll_spring = None;
                self.shell_state.overview_scroll = target;
                self.shell_state.overview_index = target.round().max(0.0) as usize;
                let _ = write_atomic(
                    &runtime_path(phone::SHELL_STATE_FILE),
                    &self.shell_state.serialize(),
                );
            }
        } else {
            let target = self.shell_state.overview_scroll;
            let scroll_diff = target - self.overview_scroll;
            if scroll_diff.abs() > 0.001 {
                self.overview_scroll += scroll_diff * 0.18;
            } else {
                self.overview_scroll = target;
            }
        }
    }

    /// Returns true if a navigation gesture or spring settling animation is currently active.
    pub(crate) fn is_nav_animating(&self) -> bool {
        self.nav_gesture_active
            || self.nav_spring.is_some()
            || self.overview_scroll_spring.is_some()
            || self.nav_gesture_progress > 0.001
            || self.nav_switch_active
            || self.nav_switch_spring.is_some()
            || self.nav_switch_dx.abs() > 0.5
            || (self.shell_state.overview_scroll - self.overview_scroll).abs() > 0.001
    }

    /// Advances the inertial spring solver governing Overview / Home / Fullscreen
    /// and horizontal Quick Switch window cycling transitions.
    pub(crate) fn advance_nav_spring(&mut self, dt: f64) {
        // 1. Advance vertical gesture spring
        if let Some(ref mut spring) = self.nav_spring {
            spring.step_analytical(dt);
            self.nav_gesture_progress = spring.position.clamp(0.0, 1.15);

            if spring.is_settled_default() {
                let target = self.nav_target.take();
                self.nav_spring = None;
                match target {
                    Some(NavigationTarget::Overview) => {
                        self.nav_gesture_progress = 0.0;
                        self.shell_state.mode = PhoneMode::Overview;
                        let _ = write_atomic(
                            &runtime_path(phone::SHELL_STATE_FILE),
                            &self.shell_state.serialize(),
                        );
                    }
                    Some(NavigationTarget::Home) => {
                        self.nav_gesture_progress = 0.0;
                        self.shell_state.mode = PhoneMode::Home;
                        self.shell_state.overview_index = 0;
                        self.shell_state.overview_scroll = 0.0;
                        self.overview_scroll = 0.0;
                        let _ = write_atomic(
                            &runtime_path(phone::SHELL_STATE_FILE),
                            &self.shell_state.serialize(),
                        );
                    }
                    Some(NavigationTarget::Fullscreen) | _ => {
                        let target_id = self.phone_tasks.get(self.shell_state.overview_index).map(|t| t.id);
                        self.nav_gesture_progress = 0.0;
                        self.nav_gesture_dy = 0.0;
                        self.shell_state.mode = PhoneMode::App;
                        self.shell_state.overview_index = 0;
                        self.shell_state.overview_scroll = 0.0;
                        self.overview_scroll = 0.0;
                        if let Some(id) = target_id {
                            self.focus_task_by_id(id);
                        }
                        let _ = write_atomic(
                            &runtime_path(phone::SHELL_STATE_FILE),
                            &self.shell_state.serialize(),
                        );
                    }
                }
            }
        }

        // 2. Advance horizontal Quick Switch card transition spring
        if let Some(ref mut spring) = self.nav_switch_spring {
            spring.step_analytical(dt);
            self.nav_switch_dx = spring.position;

            if spring.is_settled(2.0, 20.0) {
                let target = self.nav_switch_target.take();
                self.nav_switch_spring = None;
                self.nav_switch_dx = 0.0;
                match target {
                    Some(NavigationTarget::NextApp) => {
                        if self.phone_tasks.len() > 1 {
                            let task = self.phone_tasks.remove(0);
                            self.phone_tasks.push(task);
                        }
                    }
                    Some(NavigationTarget::PrevApp) => {
                        if self.phone_tasks.len() > 1 {
                            let task = self.phone_tasks.pop().unwrap();
                            self.phone_tasks.insert(0, task);
                        }
                    }
                    _ => {}
                }
                self.shell_state.mode = PhoneMode::App;
                self.shell_state.overview_index = 0;
                self.shell_state.overview_scroll = 0.0;
                self.overview_scroll = 0.0;
                let _ = write_atomic(
                    &runtime_path(phone::SHELL_STATE_FILE),
                    &self.shell_state.serialize(),
                );
                self.publish_phone_tasks();
                if let Some(first) = self.phone_tasks.first() {
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    if let Some(toplevel) = first.window.toplevel() {
                        toplevel.with_pending_state(|state| {
                            state.states.set(xdg_toplevel::State::Activated);
                        });
                        toplevel.send_configure();
                        let surf = toplevel.wl_surface().clone();
                        if let Some(keyboard) = self.seat.get_keyboard() {
                            keyboard.set_focus(self, Some(surf), serial);
                        }
                    }
                }
            }
        }
    }

    /// Reads shell state + commands and publishes the phone task list.
    pub(crate) fn poll_shell_ipc(&mut self) {
        if let Ok(text) = std::fs::read_to_string(runtime_path(phone::SHELL_STATE_FILE)) {
            let new_state = ShellState::parse(&text);

            if !self.is_native {
                if new_state.nav_switch_active && new_state.nav_gesture_active {
                    // Real-time horizontal Quick Switch drag
                    self.nav_switch_active = true;
                    self.nav_switch_dx = new_state.nav_gesture_dx;
                    self.nav_switch_vx = new_state.nav_gesture_vx;
                    self.nav_switch_spring = None;
                    self.nav_gesture_active = false;
                    self.nav_spring = None;
                } else if self.nav_switch_active && !new_state.nav_gesture_active {
                    // Horizontal contact released -> launch lateral switch spring
                    self.nav_switch_active = false;
                    let dx = new_state.nav_gesture_dx;
                    let vx = new_state.nav_gesture_vx;
                    let target = arbitrate_horizontal_release(dx, vx, PHONE_W as f64);
                    let target_dx = match target {
                        NavigationTarget::NextApp => -(PHONE_W as f64),
                        NavigationTarget::PrevApp => PHONE_W as f64,
                        _ => 0.0,
                    };
                    let mut spring = SpringSolver::horizontal_switch();
                    spring.reset(dx);
                    spring.set_target(target_dx);
                    spring.set_velocity(vx.clamp(-3000.0, 3000.0));
                    self.nav_switch_spring = Some(spring);
                    self.nav_switch_target = Some(target);
                    self.last_nav_spring_tick = Some(Instant::now());
                } else if new_state.nav_gesture_active {
                    // Real-time vertical drag
                    self.nav_gesture_active = true;
                    self.nav_switch_active = false;
                    self.nav_gesture_progress = new_state.nav_gesture_progress;
                    self.nav_gesture_dy = new_state.nav_gesture_dy;
                    self.nav_gesture_vy = new_state.nav_gesture_vy;
                    self.nav_spring = None;
                } else if self.nav_gesture_active {
                    // Vertical contact released -> arbitrate into Home, Overview, or Fullscreen
                    self.nav_gesture_active = false;
                    let target = arbitrate_release(
                        self.nav_gesture_progress,
                        self.nav_gesture_vy,
                    );
                    let target_pos = match target {
                        NavigationTarget::Overview | NavigationTarget::Home => 1.0,
                        _ => 0.0,
                    };
                    let mut spring = match target {
                        NavigationTarget::Home => SpringSolver::home_dismiss(),
                        _ => SpringSolver::overview(),
                    };
                    spring.reset(self.nav_gesture_progress);
                    spring.set_target(target_pos);
                    let denom = (PHONE_H as f64) * 0.45;
                    let v_norm = if denom > 0.0 { (-self.nav_gesture_vy / denom).clamp(-15.0, 15.0) } else { 0.0 };
                    spring.set_velocity(v_norm);
                    self.nav_spring = Some(spring);
                    self.nav_target = Some(target);
                    self.last_nav_spring_tick = Some(Instant::now());
                }
            }

            if new_state != self.shell_state {
                // If compositor is actively tracking touch dragging in Overview, do not overwrite in-memory gesture coordinates
                if self.shell_state.mode == PhoneMode::Overview && new_state.mode == PhoneMode::Overview && self.shell_state.is_dragging {
                    // Retain in-memory drag position
                } else {
                    info!("Shell state changed: {:?}", new_state);
                    self.shell_state = new_state;
                }
            }
        }

        // Consume one-shot commands (rename is atomic w.r.t. concurrent appends).
        let cmd_path = runtime_path(phone::SHELL_CMD_FILE);
        let processing = cmd_path.with_extension("processing");
        if std::fs::rename(&cmd_path, &processing).is_ok() {
            let text = std::fs::read_to_string(&processing).unwrap_or_default();
            let _ = std::fs::remove_file(&processing);
            for cmd in parse_commands(&text) {
                self.apply_shell_command(cmd);
            }
        }

        self.phone_tasks.retain(|t| t.window.alive());

        if self.phone_tasks.is_empty() && (self.shell_state.mode == PhoneMode::App || self.shell_state.mode == PhoneMode::Overview) {
            info!("All phone tasks closed -> returning to Home mode");
            self.shell_state.mode = PhoneMode::Home;
            self.shell_state.overview_index = 0;
            self.shell_state.overview_scroll = 0.0;
            self.overview_scroll = 0.0;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
        }

        self.publish_phone_tasks();
    }

    pub fn publish_phone_tasks(&mut self) {
        let mut listing = String::new();
        for task in &self.phone_tasks {
            let (app_id, title) = task
                .window
                .toplevel()
                .map(|t| {
                    with_states(t.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|d| d.lock().ok())
                            .map(|d| {
                                (
                                    d.app_id.clone().unwrap_or_default(),
                                    d.title.clone().unwrap_or_default(),
                                )
                            })
                            .unwrap_or_default()
                    })
                })
                .unwrap_or_default();
            listing.push_str(&task_line(task.id, &app_id, &title));
        }
        if listing != self.last_tasks_published {
            let _ = write_atomic(&runtime_path(phone::WINDOWS_FILE), &listing);
            self.last_tasks_published = listing;
        }
    }
    pub fn set_phone_home(&mut self) {
        self.shell_state.mode = PhoneMode::Home;
        self.shell_state.overview_index = 0;
        self.shell_state.overview_scroll = 0.0;
        self.overview_scroll = 0.0;
        let _ = write_atomic(
            &runtime_path(phone::SHELL_STATE_FILE),
            &self.shell_state.serialize(),
        );
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, serial);
        }
    }

    /// Checks if any connected Wayland application currently has an active text input focus.
    pub fn is_text_input_active(&self) -> bool {
        let mut active = false;
        self.seat.text_input().with_active_text_input(|_, _| {
            active = true;
        });
        active
    }

    /// Reactively detects transitions in text input focus and notifies virtual keyboard daemons.
    pub fn update_text_input_state(&mut self) {
        let active = self.is_text_input_active();
        if active != self.last_text_input_active {
            self.last_text_input_active = active;
            let cmd = if active { "show" } else { "hide" };
            let cmd_path = runtime_path(phone::SHELL_STATE_FILE).with_file_name("penumbra-keyboard-cmd");
            let _ = std::fs::write(cmd_path, cmd);
            info!(active, "Text input active state changed -> notified OSK daemon ({})", cmd);
        }
    }

    pub fn focus_task_by_id(&mut self, id: u32) {
        if let Some(idx) = self.phone_tasks.iter().position(|t| t.id == id) {
            let task = self.phone_tasks.remove(idx);
            self.phone_tasks.insert(0, task);
            self.overview_scroll = 0.0;
            self.shell_state.overview_scroll = 0.0;
            self.shell_state.overview_index = 0;
            self.shell_state.mode = PhoneMode::App;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
            self.publish_phone_tasks();

            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            if let Some(first) = self.phone_tasks.first() {
                let window = first.window.clone();
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.states.set(xdg_toplevel::State::Activated);
                    });
                    toplevel.send_configure();
                    let surf = toplevel.wl_surface().clone();
                    if let Some(keyboard) = self.seat.get_keyboard() {
                        keyboard.set_focus(self, Some(surf), serial);
                    }
                }
            }
        }
    }

    pub fn close_task_by_id(&mut self, id: u32) {
        if let Some(pos) = self.phone_tasks.iter().position(|t| t.id == id) {
            let task = self.phone_tasks.remove(pos);
            if let Some(t) = task.window.toplevel() {
                t.send_close();
            }
            if self.phone_tasks.is_empty() {
                self.shell_state.mode = PhoneMode::Home;
                self.shell_state.overview_index = 0;
                self.shell_state.overview_scroll = 0.0;
                self.overview_scroll = 0.0;
            } else {
                self.shell_state.overview_index =
                    self.shell_state.overview_index.min(self.phone_tasks.len() - 1);
                self.shell_state.overview_scroll = self.shell_state.overview_index as f64;
                self.overview_scroll = self.shell_state.overview_scroll;
            }
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
            self.publish_phone_tasks();
        }
    }

    fn apply_shell_command(&mut self, cmd: ShellCommand) {
        info!("Shell command: {:?}", cmd);
        match cmd {
            ShellCommand::Focus(id) => {
                self.focus_task_by_id(id);
            }
            ShellCommand::Close(id) => {
                self.close_task_by_id(id);
            }
            ShellCommand::CloseAll => {
                for task in self.phone_tasks.drain(..) {
                    if let Some(t) = task.window.toplevel() {
                        t.send_close();
                    }
                }
                self.shell_state.mode = PhoneMode::Home;
                self.shell_state.overview_index = 0;
                self.shell_state.overview_scroll = 0.0;
                self.overview_scroll = 0.0;
                let _ = write_atomic(
                    &runtime_path(phone::SHELL_STATE_FILE),
                    &self.shell_state.serialize(),
                );
                self.publish_phone_tasks();
            }
            ShellCommand::Back => {
                self.trigger_back();
            }
            ShellCommand::PowerDialog => {
                self.show_power_dialog();
            }
            ShellCommand::ScreenToggle => {
                self.toggle_screen_blanking();
            }
        }
    }

    pub fn trigger_back(&mut self) {
        info!("Executing system-wide back action");
        if self.shell_state.power_dialog_open {
            self.shell_state.power_dialog_open = false;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
            return;
        }

        if self.shell_state.drawer_open {
            self.shell_state.drawer_open = false;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
            return;
        }

        if self.shell_state.mode == PhoneMode::Overview {
            self.shell_state.mode = PhoneMode::Home;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
            return;
        }

        if self.shell_state.mode == PhoneMode::App {
            self.send_escape_to_active();
        }
    }

    pub fn send_escape_to_active(&mut self) {
        if let Some(keyboard) = self.seat.get_keyboard() {
            if let Some(_focus) = keyboard.current_focus() {
                let serial: Serial = 1u32.into();
                let time = 0;
                // Evdev 1 is KEY_ESC
                keyboard.input::<(), _>(
                    self,
                    Keycode::new(1),
                    smithay::backend::input::KeyState::Pressed,
                    serial,
                    time,
                    |_, _, _| FilterResult::Forward,
                );
                keyboard.input::<(), _>(
                    self,
                    Keycode::new(1),
                    smithay::backend::input::KeyState::Released,
                    serial,
                    time + 10,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }
    }

    pub fn show_power_dialog(&mut self) {
        info!("Showing system Power Dialog");
        self.shell_state.power_dialog_open = true;
        let _ = write_atomic(
            &runtime_path(phone::SHELL_STATE_FILE),
            &self.shell_state.serialize(),
        );
    }

    pub fn adjust_volume(&mut self, delta: f32) {
        let cur = read_volume_pactl().unwrap_or(self.shell_state.volume_level);
        let new_vol = (cur + delta).clamp(0.0, 1.0);
        let pct = (new_vol * 100.0).round() as u32;
        info!("Adjusting volume by {:.2} -> {:.2} ({}%)", delta, new_vol, pct);
        let _ = std::process::Command::new("pactl")
            .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{pct}%")])
            .spawn();
        self.shell_state.volume_level = new_vol;
        self.shell_state.volume_modal_open = true;
        let _ = write_atomic(
            &runtime_path(phone::SHELL_STATE_FILE),
            &self.shell_state.serialize(),
        );
    }

    pub fn toggle_screen_blanking(&mut self) {
        let backlight_file = "/sys/class/backlight/backlight/brightness";
        if self.screen_blanked {
            info!("Unblanking screen, restoring brightness to {}", self.saved_brightness);
            let _ = std::fs::write(backlight_file, self.saved_brightness.to_string());
            self.screen_blanked = false;
        } else {
            if let Ok(cur_str) = std::fs::read_to_string(backlight_file) {
                if let Ok(cur) = cur_str.trim().parse::<u32>() {
                    if cur > 0 {
                        self.saved_brightness = cur;
                    }
                }
            }
            info!("Blanking screen (turning off backlight)");
            let _ = std::fs::write(backlight_file, "0");
            self.screen_blanked = true;
        }
    }
}

fn read_volume_pactl() -> Option<f32> {
    let output = std::process::Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let pct_idx = text.find('%')?;
    let prefix = &text[..pct_idx];
    let start = prefix
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);
    let pct = prefix[start..].trim().parse::<f32>().ok()?;
    Some((pct / 100.0).clamp(0.0, 1.0))
}

impl BufferHandler for PenumbraServerState {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl XdgShellHandler for PenumbraServerState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window_count = self.space.elements().count();

        // 1. Root Shell Window (penumbra-shell legacy monolithic) - only if app_id matches
        let is_legacy_shell = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok())
                .map(|d| d.app_id.as_deref() == Some("penumbra-shell"))
                .unwrap_or(false)
        });

        if is_legacy_shell && self.root_shell.is_none() {
            let shell_width = if self.is_convergence_active {
                1692
            } else {
                412
            };
            info!(
                "Routing Penumbra Shell root surface: {}x915 across canvas",
                shell_width
            );
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Activated);
                state.size = Some((shell_width, 915).into());
            });
            surface.send_configure();

            let window = Window::new_wayland_window(surface);
            self.space.map_element(window.clone(), (0, 0), true);
            self.root_shell = Some(window);
            return;
        }

        // 2. Client Application Windows (routed to Phone or Desktop depending on launcher)
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        let target_file = Path::new(&runtime_dir).join("penumbra-target-output");
        let target_output = if target_file.exists() {
            let content = std::fs::read_to_string(&target_file).unwrap_or_default();
            let _ = std::fs::remove_file(&target_file);
            content.trim().to_string()
        } else {
            "phone".to_string()
        };

        if target_output == "phone" || !self.is_convergence_active {
            info!(
                "Routing application to Output A [Phone Screen]: {}x{} at (0, {}) inside system insets",
                PHONE_W, APP_H, APP_Y
            );
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Activated);
                // Maximized makes GTK/Qt drop CSD shadows so content fills the inset exactly.
                state.states.set(xdg_toplevel::State::Maximized);
                state.size = Some((PHONE_W, APP_H).into());
            });
            surface.send_configure();

            let window = Window::new_wayland_window(surface);
            // Kept in the space for commit/refresh bookkeeping; rendering and input on
            // the phone output are handled explicitly by the layer passes.
            self.space.map_element(window.clone(), (0, APP_Y), false);
            let id = self.next_task_id;
            self.next_task_id += 1;
            let window_clone = window.clone();
            self.phone_tasks.insert(0, PhoneTask { id, window });

            // Automatically switch to App mode and save state
            self.shell_state.mode = PhoneMode::App;
            self.shell_state.overview_index = 0;
            self.shell_state.overview_scroll = 0.0;
            self.overview_scroll = 0.0;
            self.shell_state.drawer_open = false;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
            self.publish_phone_tasks();

            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            if let Some(toplevel) = window_clone.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Activated);
                });
                toplevel.send_configure();
                let surf = toplevel.wl_surface().clone();
                if let Some(keyboard) = self.seat.get_keyboard() {
                    keyboard.set_focus(self, Some(surf), serial);
                }
            }
            return;
        } else {
            info!(
                "Routing application to Output B [External Desktop]: floating inside usable zone"
            );
            let mut offset_x = 414 + 60;
            let mut offset_y = 60;
            let mut win_w = 800;
            let mut win_h = 600;

            if let Some(ref out_b) = self.output_b {
                let map_b = layer_map_for_output(out_b);
                let usable_zone = map_b.non_exclusive_zone();
                info!(
                    "Output B layer-shell usable non-exclusive zone: {:?}",
                    usable_zone
                );

                let max_w = (usable_zone.size.w - 80).max(400);
                let max_h = (usable_zone.size.h - 80).max(300);
                win_w = win_w.min(max_w);
                win_h = win_h.min(max_h);

                let app_idx = window_count.saturating_sub(1);
                offset_x = 412 + usable_zone.loc.x + 40 + ((app_idx as i32 * 35) % 300);
                offset_y = usable_zone.loc.y + 30 + ((app_idx as i32 * 30) % 150);
            }

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Activated);
                state.size = Some((win_w, win_h).into());
            });
            surface.send_configure();

            let window = Window::new_wayland_window(surface);
            self.space.map_element(window, (offset_x, offset_y), true);
        }
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl WlrLayerShellHandler for PenumbraServerState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        info!(
            "New wlr-layer-shell surface: namespace='{}', layer={:?}, output={:?}",
            namespace, layer, output
        );

        // Output Routing Logic:
        // Desktop components (desktop panel, dock, external monitor applets) map to Output B.
        // Mobile shell layer surfaces (top-bar, status-bar, nav, osk) default to Output A (Phone Screen).
        let ns = namespace.to_lowercase();
        let target_output = if ns.contains("desktop") || ns.contains("panel") || ns.contains("dock")
        {
            self.output_b.as_ref().unwrap_or(&self.output_a)
        } else if let Some(ref wl_out) = output {
            let resolved = Output::from_resource(wl_out);
            if resolved.as_ref() == self.output_b.as_ref() {
                self.output_b.as_ref().unwrap_or(&self.output_a)
            } else {
                &self.output_a
            }
        } else {
            &self.output_a
        };

        let mut map = layer_map_for_output(target_output);
        let layer_surface = LayerSurface::new(surface, namespace.clone());
        // Phone chrome is sized to the logical canvas; the compositor scales it to the panel.
        let output_size: smithay::utils::Size<i32, smithay::utils::Logical> =
            if target_output == &self.output_a {
                (self.phone_logical_size.0, self.phone_logical_size.1).into()
            } else {
                target_output
                    .current_mode()
                    .map(|m| (m.size.w, m.size.h).into())
                    .unwrap_or((1280, 720).into())
            };
        let explicit_mobile_size = if ns.contains("homescreen") {
            Some((output_size.w.max(1), output_size.h.max(1)))
        } else if ns.contains("nav") {
            Some((output_size.w.max(1), NAV_BAR_H))
        } else if ns.contains("top") || ns.contains("status") {
            Some((output_size.w.max(1), STATUS_BAR_H))
        } else if ns.contains("quick") {
            Some((output_size.w.max(1), output_size.h.max(1)))
        } else if ns.contains("keyboard") {
            Some((output_size.w.max(1), 280))
        } else {
            None
        };
        if let Some((width, height)) = explicit_mobile_size {
            debug!(
                "Using explicit mobile layer size: {}x{} for namespace '{}'",
                width, height, namespace
            );
            layer_surface.layer_surface().with_pending_state(|state| {
                state.size = Some((width, height).into());
            });
        } else if layer == Layer::Background {
            layer_surface.layer_surface().with_pending_state(|state| {
                state.size = Some((output_size.w, output_size.h).into());
            });
        }

        // Map and arrange only after the requested size is pending, so the initial configure carries it.
        let _ = map.map_layer(&layer_surface);
        map.arrange();
        if explicit_mobile_size.is_none() && layer != Layer::Background {
            if let Some(geo) = map.layer_geometry(&layer_surface) {
                debug!("Layer surface mapped geometry: {:?}", geo);
                layer_surface.layer_surface().with_pending_state(|state| {
                    state.size = Some(geo.size);
                });
            }
        }
        layer_surface.layer_surface().send_configure();
        debug!("Sent send_configure() to layer surface '{}'", namespace);
    }
}

impl SelectionHandler for PenumbraServerState {
    type SelectionUserData = ();
}

impl DataDeviceHandler for PenumbraServerState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for PenumbraServerState {}
impl ServerDndGrabHandler for PenumbraServerState {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl CompositorHandler for PenumbraServerState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        for window in self.space.elements() {
            if window
                .toplevel()
                .map(|t| t.wl_surface() == surface)
                .unwrap_or(false)
            {
                window.on_commit();
            }
        }

        // Update layer surfaces on Output A & Output B
        {
            let mut map_a = layer_map_for_output(&self.output_a);
            if map_a.layers().any(|l| l.wl_surface() == surface) {
                debug!("Layer surface committed on Output A");
                map_a.arrange();
            }
        }

        if let Some(ref out_b) = self.output_b {
            let mut map_b = layer_map_for_output(out_b);
            if map_b.layers().any(|l| l.wl_surface() == surface) {
                map_b.arrange();
            }
        }
    }
}

impl OutputHandler for PenumbraServerState {}

impl ShmHandler for PenumbraServerState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for PenumbraServerState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

impl InputMethodHandler for PenumbraServerState {
    fn new_popup(&mut self, _surface: ImPopupSurface) {}
    fn dismiss_popup(&mut self, _surface: ImPopupSurface) {}
    fn popup_repositioned(&mut self, _surface: ImPopupSurface) {}
    fn parent_geometry(&self, _parent: &WlSurface) -> Rectangle<i32, smithay::utils::Logical> {
        Rectangle::default()
    }
}

delegate_xdg_shell!(PenumbraServerState);
delegate_layer_shell!(PenumbraServerState);
delegate_compositor!(PenumbraServerState);
delegate_shm!(PenumbraServerState);
delegate_seat!(PenumbraServerState);
delegate_data_device!(PenumbraServerState);
delegate_output!(PenumbraServerState);
delegate_text_input_manager!(PenumbraServerState);
delegate_input_method_manager!(PenumbraServerState);
delegate_virtual_keyboard_manager!(PenumbraServerState);

/// Compositor server coordinator and runner.
pub struct PenumbraCompositor {
    pub args: CliArgs,
    pub output_manager: AsymmetricOutputManager,
    pub gesture_sm: GestureStateMachine,
    pub drawer_spring: Spring,
    pub is_running: Arc<AtomicBool>,
}

impl PenumbraCompositor {
    pub fn new(args: CliArgs) -> Self {
        let mut output_manager = AsymmetricOutputManager::new();

        if args.convergence {
            info!("Enabling secondary external desktop monitor (HDMI 1280x720)...");
            output_manager.hotplug_external_display();
        }

        Self {
            args,
            output_manager,
            gesture_sm: GestureStateMachine::default(),
            drawer_spring: Spring::expressive(),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Initializes and runs the compositor instance.
    pub fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.is_running.store(true, Ordering::SeqCst);

        info!("============================================================");
        info!("    PENUMBRA WAYLAND COMPOSITOR (Material 3 Expressive)     ");
        info!("============================================================");
        info!("Wayland socket: {}", self.args.socket);
        let nested_enabled = self.args.nested_enabled();
        info!("Nested simulation window: {}", nested_enabled);
        info!(
            "External HDMI convergence: {}",
            self.output_manager.is_convergence_active()
        );
        if self.args.native {
            info!("Backend: native DRM/libseat device");
        } else if nested_enabled {
            info!("Backend: nested Winit desktop window");
        } else {
            info!("Backend: Winit host display");
        }

        if self.args.headless {
            info!("Running in headless validation mode. Executing policy assertions...");
            self.run_validation_loop()?;
            info!("Headless validation completed successfully.");
            return Ok(());
        }

        if self.args.native {
            #[cfg(feature = "native-drm")]
            return crate::native::run(self);
            #[cfg(not(feature = "native-drm"))]
            return Err(
                "native backend unavailable: rebuild penumbra-comp with --features native-drm"
                    .into(),
            );
        }

        self.run_wayland_server()
    }

    fn run_validation_loop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let dsi_config = self
            .output_manager
            .output_config(OutputRole::PhoneDsi)
            .unwrap();
        assert_eq!(dsi_config.width, PHONE_W);
        assert_eq!(dsi_config.height, PHONE_H);
        assert!(dsi_config.enforces_gestures);

        if self.output_manager.is_convergence_active() {
            let hdmi_config = self
                .output_manager
                .output_config(OutputRole::ExternalDesktopHdmi)
                .unwrap();
            assert_eq!(hdmi_config.width, 1280);
            assert_eq!(hdmi_config.height, 720);
            assert!(hdmi_config.allows_floating);
        }

        // Test gesture interception and state machine step
        self.gesture_sm
            .on_down(200.0, 890.0, Duration::from_millis(100));
        self.gesture_sm
            .on_move(200.0, 700.0, Duration::from_millis(150));
        let gesture = self
            .gesture_sm
            .on_up(200.0, 600.0, Duration::from_millis(200), PHONE_H as f64);
        assert_eq!(gesture, Some(NavigationGesture::ReturnHome));

        Ok(())
    }

    /// Primary Smithay Wayland compositor event and rendering loop.
    fn run_wayland_server(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let socket_name = &self.args.socket;

        // Clean up any stale socket and lock files before binding
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        let socket_path = Path::new(&runtime_dir).join(socket_name);
        let lock_path = Path::new(&runtime_dir).join(format!("{}.lock", socket_name));
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
            info!("Removed stale Wayland socket: {:?}", socket_path);
        }
        if lock_path.exists() {
            let _ = std::fs::remove_file(&lock_path);
            info!("Removed stale Wayland socket lock: {:?}", lock_path);
        }

        let listener = ListeningSocket::bind(socket_name.as_str())?;
        info!(
            "Listening for Wayland clients on socket: {} ({:?})",
            socket_name, socket_path
        );

        let mut display: Display<PenumbraServerState> = Display::new()?;
        let dh = display.handle();

        // Initialize Wayland protocol handlers
        let compositor_state = CompositorState::new::<PenumbraServerState>(&dh);
        let shm_state = ShmState::new::<PenumbraServerState>(&dh, vec![]);
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(&dh, "penumbra-seat");
        let xdg_shell_state = XdgShellState::new::<PenumbraServerState>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<PenumbraServerState>(&dh);
        let data_device_state = DataDeviceState::new::<PenumbraServerState>(&dh);
        let output_manager_state =
            OutputManagerState::new_with_xdg_output::<PenumbraServerState>(&dh);
        let text_input_state = TextInputManagerState::new::<PenumbraServerState>(&dh);
        let input_method_state =
            InputMethodManagerState::new::<PenumbraServerState, _>(&dh, |_client| true);
        let virtual_keyboard_state =
            VirtualKeyboardManagerState::new::<PenumbraServerState, _>(&dh, |_client| true);

        let mut space = Space::<Window>::default();

        // 1. Output A: Phone DSI (412x915 @ 120Hz)
        let output_a = Output::new(
            "DSI-1".to_string(),
            PhysicalProperties {
                size: (65, 145).into(),
                subpixel: Subpixel::Unknown,
                make: "Penumbra".to_string(),
                model: "Phone DSI Mobile".to_string(),
            },
        );
        let _global_a = output_a.create_global::<PenumbraServerState>(&dh);
        let mode_a = Mode {
            size: (412, 915).into(),
            refresh: 120_000,
        };
        output_a.set_preferred(mode_a);
        output_a.change_current_state(
            Some(mode_a),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        space.map_output(&output_a, (0, 0));
        info!("Mapped Output A: DSI-1 (412x915 @ (0, 0))");

        // 2. Output B: External HDMI Desktop (1280x720 @ 60Hz)
        let output_b = if self.args.convergence {
            let output_b = Output::new(
                "HDMI-A-1".to_string(),
                PhysicalProperties {
                    size: (530, 298).into(),
                    subpixel: Subpixel::Unknown,
                    make: "Penumbra".to_string(),
                    model: "Convergence Desktop HDMI".to_string(),
                },
            );
            let _global_b = output_b.create_global::<PenumbraServerState>(&dh);
            let mode_b = Mode {
                size: (1280, 720).into(),
                refresh: 60_000,
            };
            output_b.set_preferred(mode_b);
            output_b.change_current_state(
                Some(mode_b),
                Some(Transform::Normal),
                Some(Scale::Integer(1)),
                Some((412, 0).into()),
            );
            space.map_output(&output_b, (412, 0));
            info!("Mapped Output B: HDMI-A-1 (1280x720 @ (412, 0))");
            Some(output_b)
        } else {
            None
        };

        let mut state = PenumbraServerState {
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            output_manager_state,
            text_input_state,
            input_method_state,
            virtual_keyboard_state,
            last_text_input_active: false,
            seat,
            space,
            root_shell: None,
            output_a,
            output_b,
            is_convergence_active: self.args.convergence,
            is_native: false,
            pointer_location: (0.0, 0.0).into(),
            phone_tasks: Vec::new(),
            next_task_id: 1,
            shell_state: ShellState::default(),
            overview_scroll: 0.0,
            last_tasks_published: String::from("\u{0}"),
            phone_logical_size: (mode_a.size.w, mode_a.size.h),
            screen_blanked: false,
            saved_brightness: 2500,
            nav_gesture_active: false,
            nav_gesture_progress: 0.0,
            nav_gesture_dy: 0.0,
            nav_gesture_dx: 0.0,
            nav_gesture_vy: 0.0,
            nav_switch_active: false,
            nav_switch_dx: 0.0,
            nav_switch_vx: 0.0,
            nav_spring: None,
            nav_target: None,
            nav_switch_spring: None,
            nav_switch_target: None,
            overview_scroll_spring: None,
            last_nav_spring_tick: None,
            blurred_wallpaper: {
                let cache = crate::wallpaper_blur::BlurredWallpaperCache::load_and_blur(
                    None,
                    mode_a.size.w,
                    mode_a.size.h,
                );
                Some(cache)
            },
            blurred_wallpaper_buffer: {
                let cache = crate::wallpaper_blur::BlurredWallpaperCache::load_and_blur(
                    None,
                    mode_a.size.w,
                    mode_a.size.h,
                );
                Some(cache.to_render_buffer())
            },
            card_corner_masks: crate::card_rounding::CardCornerMasks::default(),
        };

        // Reset IPC files from any previous session.
        let _ = std::fs::remove_file(runtime_path(phone::SHELL_STATE_FILE));
        let _ = std::fs::remove_file(runtime_path(phone::SHELL_CMD_FILE));
        let _ = write_atomic(&runtime_path(phone::WINDOWS_FILE), "");

        let keyboard = state.seat.add_keyboard(Default::default(), 200, 200)?;
        let pointer = state.seat.add_pointer();

        // Root multi-output viewport sizing:
        // Output A: 412x915
        // Output B: 1280x720
        // Combined root canvas: (412 + 1280) x 915 = 1692 x 915
        let canvas_w = if self.args.convergence {
            412 + 1280
        } else {
            412
        };
        let canvas_h = 915;

        info!(
            "Creating Smithay Winit root multi-output viewport: {}x{}",
            canvas_w, canvas_h
        );

        // Ensure WAYLAND_DISPLAY does not point to our own listening socket when creating host window
        if let Ok(cur_wl) = std::env::var("WAYLAND_DISPLAY") {
            if cur_wl == *socket_name {
                if let Ok(host_wl) = std::env::var("HOST_WAYLAND_DISPLAY") {
                    std::env::set_var("WAYLAND_DISPLAY", host_wl);
                } else {
                    std::env::remove_var("WAYLAND_DISPLAY");
                }
            }
        }

        let (mut backend, mut winit_loop) =
            winit::init_from_attributes::<GlesRenderer>(
                smithay::reexports::winit::window::Window::default_attributes()
                    .with_inner_size(smithay::reexports::winit::dpi::LogicalSize::new(
                        canvas_w as f64,
                        canvas_h as f64,
                    ))
                    .with_title("Penumbra Wayland Compositor (Output A: Phone 412x915 | Output B: External 1280x720)")
                    .with_visible(true),
            )?;

        let mut clients = Vec::new();
        let start_time = Instant::now();
        let mut serial_counter = 0u32;
        let mut last_ipc_poll = Instant::now();

        info!("Entering Penumbra Wayland Compositor presentation loop...");

        loop {
            // 1. Process host Winit window input events
            let status = winit_loop.dispatch_new_events(|event| match event {
                WinitEvent::Resized { .. } => {}
                WinitEvent::Input(input_event) => match input_event {
                    InputEvent::Keyboard { event } => {
                        serial_counter = serial_counter.wrapping_add(1);
                        keyboard.input::<(), _>(
                            &mut state,
                            event.key_code(),
                            event.state(),
                            serial_counter.into(),
                            start_time.elapsed().as_millis() as u32,
                            |_, _, _| FilterResult::Forward,
                        );
                    }
                    InputEvent::PointerMotionAbsolute { event } => {
                        serial_counter = serial_counter.wrapping_add(1);
                        let w_size = backend
                            .window_size()
                            .to_f64()
                            .to_logical(backend.scale_factor())
                            .to_i32_round();
                        let pos = event.position_transformed(w_size);
                        state.pointer_location = pos;

                        // Output A (phone): inset-aware routing, independent of space z-order.
                        if pos.x < PHONE_W as f64 {
                            let focus = state.phone_focus_under(pos);
                            let kb_target =
                                if state.phone_app_visible() && !state.shell_state.drawer_open {
                                    state.active_phone_surface()
                                } else if let Some((ref surf, _)) = focus {
                                    Some(surf.clone())
                                } else {
                                    state.root_surface()
                                };
                            if keyboard.current_focus() != kb_target {
                                keyboard.set_focus(&mut state, kb_target, serial_counter.into());
                            }
                            pointer.motion(
                                &mut state,
                                focus,
                                &MotionEvent {
                                    location: pos,
                                    serial: serial_counter.into(),
                                    time: start_time.elapsed().as_millis() as u32,
                                },
                            );
                            pointer.frame(&mut state);
                            return;
                        }

                        // Check layer surfaces on Output B (Overlay, Top) first
                        let layer_under = if let Some(ref out_b) = state.output_b {
                            if pos.x >= 412.0 {
                                let map_b = layer_map_for_output(out_b);
                                let rel_p: Point<f64, smithay::utils::Logical> =
                                    (pos.x - 412.0, pos.y).into();
                                map_b
                                    .layer_under(Layer::Overlay, rel_p)
                                    .or_else(|| map_b.layer_under(Layer::Top, rel_p))
                                    .map(|l| {
                                        (
                                            l.wl_surface().clone(),
                                            map_b.layer_geometry(l).unwrap_or_default(),
                                        )
                                    })
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let Some((surf, geo)) = layer_under {
                            let loc: Point<f64, smithay::utils::Logical> =
                                ((412 + geo.loc.x) as f64, geo.loc.y as f64).into();
                            pointer.motion(
                                &mut state,
                                Some((surf.clone(), loc)),
                                &MotionEvent {
                                    location: pos,
                                    serial: serial_counter.into(),
                                    time: start_time.elapsed().as_millis() as u32,
                                },
                            );
                            keyboard.set_focus(&mut state, Some(surf), serial_counter.into());
                        } else if let Some((window, rel_pos)) = state.space.element_under(pos) {
                            if let Some(toplevel) = window.toplevel() {
                                let surface = toplevel.wl_surface().clone();
                                pointer.motion(
                                    &mut state,
                                    Some((surface.clone(), rel_pos.to_f64())),
                                    &MotionEvent {
                                        location: pos,
                                        serial: serial_counter.into(),
                                        time: start_time.elapsed().as_millis() as u32,
                                    },
                                );
                                keyboard.set_focus(
                                    &mut state,
                                    Some(surface),
                                    serial_counter.into(),
                                );
                            }
                        } else {
                            // Check bottom/background layers
                            let bg_layer = if let Some(ref out_b) = state.output_b {
                                if pos.x >= 412.0 {
                                    let map_b = layer_map_for_output(out_b);
                                    let rel_p: Point<f64, smithay::utils::Logical> =
                                        (pos.x - 412.0, pos.y).into();
                                    map_b
                                        .layer_under(Layer::Bottom, rel_p)
                                        .or_else(|| map_b.layer_under(Layer::Background, rel_p))
                                        .map(|l| {
                                            (
                                                l.wl_surface().clone(),
                                                map_b.layer_geometry(l).unwrap_or_default(),
                                            )
                                        })
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                            if let Some((surf, geo)) = bg_layer {
                                let loc: Point<f64, smithay::utils::Logical> =
                                    ((412 + geo.loc.x) as f64, geo.loc.y as f64).into();
                                pointer.motion(
                                    &mut state,
                                    Some((surf.clone(), loc)),
                                    &MotionEvent {
                                        location: pos,
                                        serial: serial_counter.into(),
                                        time: start_time.elapsed().as_millis() as u32,
                                    },
                                );
                                keyboard.set_focus(&mut state, Some(surf), serial_counter.into());
                            } else {
                                pointer.motion(
                                    &mut state,
                                    None,
                                    &MotionEvent {
                                        location: pos,
                                        serial: serial_counter.into(),
                                        time: start_time.elapsed().as_millis() as u32,
                                    },
                                );
                            }
                        }
                        pointer.frame(&mut state);
                    }
                    InputEvent::PointerButton { event } => {
                        serial_counter = serial_counter.wrapping_add(1);

                        // If not clicking on top layer surface, check window raise
                        let is_on_top_layer = if let Some(ref out_b) = state.output_b {
                            if state.pointer_location.x >= 412.0 {
                                let map_b = layer_map_for_output(out_b);
                                let rel_p: Point<f64, smithay::utils::Logical> =
                                    (state.pointer_location.x - 412.0, state.pointer_location.y)
                                        .into();
                                map_b
                                    .layer_under(Layer::Overlay, rel_p)
                                    .or_else(|| map_b.layer_under(Layer::Top, rel_p))
                                    .is_some()
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if !is_on_top_layer && state.pointer_location.x < PHONE_W as f64 && state.shell_state.mode == PhoneMode::Overview {
                            if event.state() == smithay::backend::input::ButtonState::Released {
                                let px = state.pointer_location.x;
                                let py = state.pointer_location.y;
                                let mut tapped_id = None;
                                for (i, task) in state.phone_tasks.iter().enumerate() {
                                    let (cx, cy, cw, ch, _) = card_transform(
                                        i,
                                        state.overview_scroll,
                                        state.shell_state.overview_index,
                                        0.0,
                                    );
                                    if px >= cx as f64
                                        && px <= (cx + cw) as f64
                                        && py >= cy as f64
                                        && py <= (cy + ch) as f64
                                    {
                                        tapped_id = Some(task.id);
                                        break;
                                    }
                                }
                                if let Some(id) = tapped_id {
                                    state.focus_task_by_id(id);
                                    return;
                                } else if py >= STATUS_BAR_H as f64 && py < NAV_BAR_Y as f64 {
                                    state.set_phone_home();
                                    return;
                                }
                            }
                        }

                        if !is_on_top_layer && state.pointer_location.x >= PHONE_W as f64 {
                            let clicked_window = state
                                .space
                                .element_under(state.pointer_location)
                                .map(|(w, _)| w.clone());
                            if let Some(window) = clicked_window {
                                if Some(&window) != state.root_shell.as_ref() {
                                    state.space.raise_element(&window, true);
                                }
                            }
                        }

                        pointer.button(
                            &mut state,
                            &ButtonEvent {
                                button: event.button_code(),
                                state: event.state(),
                                serial: serial_counter.into(),
                                time: start_time.elapsed().as_millis() as u32,
                            },
                        );
                        pointer.frame(&mut state);
                    }
                    InputEvent::PointerAxis { event } => {
                        let horizontal_amount = event.amount(Axis::Horizontal).unwrap_or(0.0);
                        let vertical_amount = event.amount(Axis::Vertical).unwrap_or(0.0);

                        if state.pointer_location.x < PHONE_W as f64 && state.shell_state.mode == PhoneMode::Overview {
                            let amount = if horizontal_amount.abs() > vertical_amount.abs() {
                                horizontal_amount
                            } else {
                                vertical_amount
                            };
                            let delta = amount / (CARD_STEP as f64);
                            let max_idx = state.phone_tasks.len().saturating_sub(1) as f64;
                            let new_scroll = (state.overview_scroll + delta).clamp(0.0, max_idx);
                            state.overview_scroll = new_scroll;
                            state.shell_state.overview_scroll = new_scroll;
                            state.shell_state.overview_index = new_scroll.round() as usize;
                            let _ = write_atomic(
                                &runtime_path(phone::SHELL_STATE_FILE),
                                &state.shell_state.serialize(),
                            );
                            return;
                        }

                        let mut frame = AxisFrame::new(start_time.elapsed().as_millis() as u32)
                            .source(event.source());
                        if horizontal_amount != 0.0 {
                            frame = frame.value(Axis::Horizontal, horizontal_amount);
                        }
                        if vertical_amount != 0.0 {
                            frame = frame.value(Axis::Vertical, vertical_amount);
                        }
                        pointer.axis(&mut state, frame);
                        pointer.frame(&mut state);
                    }
                    _ => {}
                },
                _ => {}
            });

            if let PumpStatus::Exit(_) = status {
                info!("Compositor window closed by user. Exiting...");
                break;
            }

            // 2. Accept incoming Wayland clients
            if let Some(stream) = listener.accept()? {
                info!("Accepting new client stream on {}", socket_name);
                if let Ok(client) = display
                    .handle()
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    clients.push(client);
                }
            }

            // 3. Dispatch & flush clients
            display.dispatch_clients(&mut state)?;
            state.update_text_input_state();
            state.space.refresh();
            let is_overview = state.shell_state.mode == PhoneMode::Overview;
            let is_animating = state.is_nav_animating();
            if is_overview || is_animating || last_ipc_poll.elapsed() >= Duration::from_millis(16) {
                state.poll_shell_ipc();
                last_ipc_poll = Instant::now();
            }
            display.flush_clients()?;

            // Advance gesture spring physics and overview carousel scroll
            state.advance_animations();

            // 4. Render.
            //
            // NOTE: `draw_render_elements` expects elements FRONT-TO-BACK within one
            // call, while each successive call paints on top of the previous ones.
            // `Space::elements()` iterates BACK-TO-FRONT, which previously caused the
            // root shell (mapped first) to be painted over every application.
            //
            // Paint order (bottom -> top):
            //   Background / Bottom layer-shell
            //   Phone Layer 0: penumbra-shell (home screen, wallpaper, desktop on Output B)
            //   Output B floating desktop windows
            //   Phone Layer 1: active app at (0, 42) cropped to 412x841
            //   Phone Layer 2: overview task cards (scaled live surfaces)
            //   Phone Layer 3: system chrome (status bar, nav bar, quick settings drawer)
            //   Top / Overlay layer-shell
            let window_size = backend.window_size();
            let damage = Rectangle::from_size(window_size);

            {
                let (renderer, mut framebuffer) = backend.bind()?;

                // Helper to collect render elements for a specific layer on an output
                let render_layer_elements =
                    |renderer: &mut GlesRenderer,
                     output: &Output,
                     origin_x: i32,
                     layer_kind: Layer|
                     -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
                        let map = layer_map_for_output(output);
                        let keyboard_open = if layer_kind == Layer::Top {
                            let path = runtime_path(phone::SHELL_STATE_FILE).with_file_name("penumbra-keyboard-state");
                            std::fs::read_to_string(path).map(|c| c.contains("open=1")).unwrap_or(false)
                        } else {
                            true
                        };
                        map.layers()
                            .filter(|l| l.layer() == layer_kind)
                            .filter(|l| l.namespace() != "penumbra-keyboard" || keyboard_open)
                            .flat_map(|l| {
                                if let Some(geo) = map.layer_geometry(l) {
                                    let (loc_x, loc_y) = if layer_kind == Layer::Background {
                                        (0, 0)
                                    } else {
                                        (geo.loc.x, geo.loc.y)
                                    };
                                    render_elements_from_surface_tree(
                                        renderer,
                                        l.wl_surface(),
                                        (origin_x + loc_x, loc_y),
                                        1.0,
                                        1.0,
                                        Kind::Unspecified,
                                    )
                                } else {
                                    Vec::new()
                                }
                            })
                            .collect()
                    };

                let mut bg_elements =
                    render_layer_elements(renderer, &state.output_a, 0, Layer::Background);
                if let Some(ref out_b) = state.output_b {
                    bg_elements.extend(render_layer_elements(
                        renderer,
                        out_b,
                        412,
                        Layer::Background,
                    ));
                }

                let mut bottom_elements =
                    render_layer_elements(renderer, &state.output_a, 0, Layer::Bottom);
                if let Some(ref out_b) = state.output_b {
                    bottom_elements.extend(render_layer_elements(
                        renderer,
                        out_b,
                        412,
                        Layer::Bottom,
                    ));
                }

                let root_surface = state.root_surface();
                let phone_rect = phys_rect(0, 0, PHONE_W, PHONE_H);
                let drawer_open = state.shell_state.drawer_open;

                // Phone Layer 0: shell base (home screen + Output B desktop UI)
                let shell_base = root_surface
                    .as_ref()
                    .map(|s| surface_elements(renderer, s, (0, 0), 1.0))
                    .unwrap_or_default();

                // Output B floating windows, front-to-back (reverse of space order)
                let mut desktop_elements = Vec::new();
                for window in state.space.elements().rev() {
                    if Some(window) == state.root_shell.as_ref() || state.is_phone_task(window) {
                        continue;
                    }
                    if let Some(toplevel) = window.toplevel() {
                        let loc = state.space.element_location(window).unwrap_or_default()
                            - window.geometry().loc;
                        desktop_elements.extend(surface_elements(
                            renderer,
                            toplevel.wl_surface(),
                            (loc.x, loc.y),
                            1.0,
                        ));
                    }
                }

                // Phone Layer 0.5: Dimming Scrim (#0b0a0e, max 45% alpha) modulated by gesture progress
                let nav_progress = if state.is_nav_animating() {
                    state.nav_gesture_progress
                } else {
                    0.0
                };
                let mut scrim_elements: Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement> = Vec::new();
                if nav_progress > 0.001 {
                    let max_scrim = if state.nav_target == Some(NavigationTarget::Home) {
                        0.25 * (1.0 - nav_progress).max(0.0)
                    } else {
                        compute_scrim_alpha(nav_progress)
                    };
                    let scrim_alpha = max_scrim as f32;
                    if scrim_alpha > 0.001 {
                        let scrim_buf = smithay::backend::renderer::element::solid::SolidColorBuffer::new(
                            (PHONE_W, PHONE_H),
                            Color32F::new(0.0431, 0.0392, 0.0549, scrim_alpha),
                        );
                        scrim_elements = smithay::backend::renderer::element::AsRenderElements::<GlesRenderer>::render_elements(
                            &scrim_buf,
                            renderer,
                            (0, 0).into(),
                            smithay::utils::Scale::from(1.0),
                            1.0,
                        );
                    }
                }

                // Phone Layer 1: active application inside the system insets (transformed dynamically during gesture)
                let mut app_elements = Vec::new();
                if state.phone_app_visible() {
                    let switch_dx = state.nav_switch_dx;
                    if state.nav_switch_active || state.nav_switch_spring.is_some() || switch_dx.abs() > 0.5 {
                        // Quick Switch: 0.94x subtle card scale, 16px inter-card gutter
                        let cur_scale = if state.nav_switch_spring.is_none() && !state.nav_switch_active {
                            1.0
                        } else {
                            (1.0 - (switch_dx.abs() / (PHONE_W as f64 * 4.0))).clamp(0.94, 1.0)
                        };
                        let cur_w = (PHONE_W as f64 * cur_scale).round() as i32;
                        let cur_h = (APP_H as f64 * cur_scale).round() as i32;
                        let cur_y = APP_Y + (APP_H - cur_h) / 2;
                        let cur_x = (switch_dx + ((PHONE_W - cur_w) / 2) as f64).round() as i32;

                        if let Some(cur_surface) = state.active_phone_surface() {
                            let rect = phys_rect(cur_x, cur_y, cur_w, cur_h);
                            if let Some(clip) = rect.intersection(phone_rect) {
                                app_elements.extend(cropped(
                                    surface_elements(renderer, &cur_surface, (cur_x, cur_y), cur_scale),
                                    cur_scale,
                                    clip,
                                ));
                            }
                        }
                        if state.phone_tasks.len() > 1 {
                            if switch_dx > 0.0 {
                                // Dragging right -> previous task enters from left with 16px gutter
                                if let Some(prev_task) = state.phone_tasks.last() {
                                    if let Some(toplevel) = prev_task.window.toplevel() {
                                        let prev_x = cur_x - cur_w - 16;
                                        let rect = phys_rect(prev_x, cur_y, cur_w, cur_h);
                                        if let Some(clip) = rect.intersection(phone_rect) {
                                            app_elements.extend(cropped(
                                                surface_elements(renderer, toplevel.wl_surface(), (prev_x, cur_y), cur_scale),
                                                cur_scale,
                                                clip,
                                            ));
                                        }
                                    }
                                }
                            } else {
                                // Dragging left -> next task enters from right with 16px gutter
                                if let Some(next_task) = state.phone_tasks.get(1) {
                                    if let Some(toplevel) = next_task.window.toplevel() {
                                        let next_x = cur_x + cur_w + 16;
                                        let rect = phys_rect(next_x, cur_y, cur_w, cur_h);
                                        if let Some(clip) = rect.intersection(phone_rect) {
                                            app_elements.extend(cropped(
                                                surface_elements(renderer, toplevel.wl_surface(), (next_x, cur_y), cur_scale),
                                                cur_scale,
                                                clip,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    } else if nav_progress > 0.001 {
                        if let Some(surface) = state.active_phone_surface() {
                            if state.nav_target == Some(NavigationTarget::Home) {
                                // Dismiss to home: track and collapse window into target icon coordinate
                                let active_app_id = state.phone_tasks.first().map(|t| t.app_id()).unwrap_or_default();
                                let (target_icon_x, target_icon_y) = resolve_app_icon_target(&active_app_id);
                                let t = nav_progress.clamp(0.0, 1.0);
                                let scale = compute_home_collapse_scale(t);
                                let w = (PHONE_W as f64 * scale).round() as i32;
                                let h = (APP_H as f64 * scale).round() as i32;

                                let initial_cx = (PHONE_W as f64) / 2.0;
                                let initial_cy = (APP_Y as f64) + (APP_H as f64) / 2.0;
                                let cx = initial_cx * (1.0 - t) + (target_icon_x as f64) * t;
                                let cy = initial_cy * (1.0 - t) + (target_icon_y as f64) * t;

                                let x = (cx - (w as f64) / 2.0).round() as i32;
                                let y = (cy - (h as f64) / 2.0).round() as i32;
                                let app_rect = phys_rect(x, y, w, h);
                                if let Some(clip) = app_rect.intersection(phone_rect) {
                                    app_elements.extend(cropped(
                                        surface_elements(renderer, &surface, (x, y), scale),
                                        scale,
                                        clip,
                                    ));
                                }
                            } else {
                                // Overview card transform: scale to 0.68, center at y = 172
                                let scale = compute_window_scale(nav_progress);
                                let w = (PHONE_W as f64 * scale).round() as i32;
                                let h = (APP_H as f64 * scale).round() as i32;
                                let x = ((PHONE_W - w) / 2).max(0);
                                let target_y = ((PHONE_H - CARD_H) / 2) as f64;
                                let y = (APP_Y as f64 * (1.0 - nav_progress) + target_y * nav_progress).round() as i32;
                                let app_rect = phys_rect(x, y, w, h);
                                if let Some(clip) = app_rect.intersection(phone_rect) {
                                    app_elements.extend(cropped(
                                        surface_elements(renderer, &surface, (x, y), scale),
                                        scale,
                                        clip,
                                    ));
                                }

                                // Adjacent cards peek into viewport when nav_progress > 0.35
                                if nav_progress > 0.35 && state.phone_tasks.len() > 1 {
                                    let p_peek = ((nav_progress - 0.35) / 0.65).clamp(0.0, 1.0);
                                    if let Some(prev_task) = state.phone_tasks.get(1) {
                                        if let Some(toplevel) = prev_task.window.toplevel() {
                                            let (dest_x, dest_y, dest_w, dest_h, card_scale) =
                                                card_transform(1, 0.0, 0, 0.0);
                                            let start_x = -dest_w;
                                            let peek_x = (start_x as f64 * (1.0 - p_peek) + dest_x as f64 * p_peek).round() as i32;
                                            let peek_rect = phys_rect(peek_x, dest_y, dest_w, dest_h);
                                            if let Some(clip) = peek_rect.intersection(phone_rect) {
                                                app_elements.extend(cropped(
                                                    surface_elements(renderer, toplevel.wl_surface(), (peek_x, dest_y), card_scale),
                                                    card_scale,
                                                    clip,
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if let Some(surface) = state.active_phone_surface() {
                        let app_rect = phys_rect(0, APP_Y, PHONE_W, APP_H);
                        app_elements = cropped(
                            surface_elements(renderer, &surface, (0, APP_Y), 1.0),
                            1.0,
                            app_rect,
                        );
                    }
                }

                // Phone Layer 2: overview carousel thumbnails
                let mut overview_elements = Vec::new();
                if state.shell_state.mode == PhoneMode::Overview && !drawer_open {
                    for (i, task) in state.phone_tasks.iter().enumerate() {
                        let (x, y, w, h, scale) = card_transform(
                            i,
                            state.overview_scroll,
                            state.shell_state.overview_index,
                            state.shell_state.active_card_dy,
                        );
                        if x + w <= 0 || x >= PHONE_W {
                            continue;
                        }
                        let Some(toplevel) = task.window.toplevel() else {
                            continue;
                        };
                        let card = phys_rect(x, y, w, h);
                        if let Some(clip) = card.intersection(phone_rect) {
                            overview_elements.extend(cropped(
                                surface_elements(renderer, toplevel.wl_surface(), (x, y), scale),
                                scale,
                                clip,
                            ));
                        }
                    }
                }

                // Phone Layer 3: persistent system chrome re-composited from the shell surface
                let chrome_rects: Vec<Rectangle<i32, Physical>> = if drawer_open {
                    vec![phone_rect]
                } else if state.phone_app_visible() || state.shell_state.mode == PhoneMode::Overview
                {
                    vec![
                        phys_rect(0, 0, PHONE_W, STATUS_BAR_H),
                        phys_rect(0, NAV_BAR_Y, PHONE_W, NAV_BAR_H),
                    ]
                } else {
                    Vec::new()
                };
                let mut chrome_elements = Vec::new();
                if let Some(ref s) = root_surface {
                    for rect in chrome_rects {
                        chrome_elements
                            .extend(cropped(surface_elements(renderer, s, (0, 0), 1.0), 1.0, rect));
                    }
                }

                let mut top_elements =
                    render_layer_elements(renderer, &state.output_a, 0, Layer::Top);
                if let Some(ref out_b) = state.output_b {
                    top_elements.extend(render_layer_elements(renderer, out_b, 412, Layer::Top));
                }

                let mut overlay_elements = if state.shell_state.drawer_open {
                    render_layer_elements(renderer, &state.output_a, 0, Layer::Overlay)
                } else {
                    Vec::new()
                };
                if let Some(ref out_b) = state.output_b {
                    overlay_elements.extend(render_layer_elements(
                        renderer,
                        out_b,
                        412,
                        Layer::Overlay,
                    ));
                }

                let mut frame =
                    renderer.render(&mut framebuffer, window_size, Transform::Flipped180)?;

                // Base canvas background: deep slate (#0a080e)
                frame.clear(Color32F::new(0.038, 0.030, 0.055, 1.0), &[damage])?;

                // Accent divider between Phone Display (Output A) and External Monitor (Output B)
                if state.is_convergence_active {
                    let divider = Rectangle::new((412, 0).into(), (2, window_size.h).into());
                    frame.clear(Color32F::new(0.35, 0.25, 0.50, 1.0), &[divider])?;
                }

                // Background / Bottom layer-shell (wallpaper, background surfaces)
                draw_render_elements(&mut frame, 1.0, &bg_elements, &[damage])?;
                draw_render_elements(&mut frame, 1.0, &bottom_elements, &[damage])?;

                // Phone Layer 0 + Output B desktop shell
                draw_render_elements(&mut frame, 1.0, &shell_base, &[damage])?;
                // Output B floating windows
                draw_render_elements(&mut frame, 1.0, &desktop_elements, &[damage])?;
                // Phone Layer 0.5: Dimming scrim modulated by gesture progress
                draw_render_elements::<GlesRenderer, _, _>(&mut frame, 1.0, &scrim_elements, &[damage])?;
                // Phone Layer 1: applications workspace (fullscreen or transformed card)
                draw_render_elements(&mut frame, 1.0, &app_elements, &[damage])?;
                // Phone Layer 2: recents / overview
                draw_render_elements(&mut frame, 1.0, &overview_elements, &[damage])?;
                // Phone Layer 3: persistent system chrome
                draw_render_elements(&mut frame, 1.0, &chrome_elements, &[damage])?;

                // Top layer-shell (panels, docks, applets)
                draw_render_elements(&mut frame, 1.0, &top_elements, &[damage])?;

                // LAYER 5: Overlay (e.g. popups / lock / overview overlay)
                draw_render_elements(&mut frame, 1.0, &overlay_elements, &[damage])?;

                let _ = frame.finish()?;

                // Send frame callbacks for smooth animation
                let elapsed = start_time.elapsed().as_millis() as u32;
                for window in state.space.elements() {
                    if let Some(toplevel) = window.toplevel() {
                        send_frames_surface_tree(toplevel.wl_surface(), elapsed);
                    }
                }
                for l in layer_map_for_output(&state.output_a).layers() {
                    send_frames_surface_tree(l.wl_surface(), elapsed);
                }
                if let Some(ref out_b) = state.output_b {
                    for l in layer_map_for_output(out_b).layers() {
                        send_frames_surface_tree(l.wl_surface(), elapsed);
                    }
                }
            }

            backend.submit(Some(&[damage]))?;

            // Avoid pegging CPU when idle, but ensure stutter-free 60Hz/120Hz tracking during active drag
            let is_animating = state.shell_state.is_dragging
                || (state.shell_state.mode == PhoneMode::Overview
                    && (state.shell_state.overview_scroll - state.overview_scroll).abs() > 0.001);
            if is_animating {
                std::thread::sleep(Duration::from_millis(1));
            } else {
                std::thread::sleep(Duration::from_millis(8));
            }
        }

        Ok(())
    }
}

pub(crate) type SurfaceElement = WaylandSurfaceRenderElement<GlesRenderer>;

/// Collects render elements (front-to-back) for a surface tree at `loc`, scaled by `scale`.
pub(crate) fn surface_elements(
    renderer: &mut GlesRenderer,
    surface: &WlSurface,
    loc: (i32, i32),
    scale: f64,
) -> Vec<SurfaceElement> {
    render_elements_from_surface_tree(renderer, surface, loc, scale, 1.0, Kind::Unspecified)
}

/// Clips render elements to `rect` (elements fully outside are dropped).
pub(crate) fn cropped(
    elements: Vec<SurfaceElement>,
    scale: impl Into<smithay::utils::Scale<f64>>,
    rect: Rectangle<i32, Physical>,
) -> Vec<CropRenderElement<SurfaceElement>> {
    let scale = scale.into();
    elements
        .into_iter()
        .filter_map(|e| CropRenderElement::from_element(e, scale, rect))
        .collect()
}

fn phys_rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
    Rectangle::new((x, y).into(), (w, h).into())
}

/// Dispatches frame callbacks down the surface tree.
pub fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}
