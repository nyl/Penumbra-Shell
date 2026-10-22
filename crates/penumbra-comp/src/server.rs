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
            AbsolutePositionEvent, Axis, InputEvent, KeyboardKeyEvent, PointerAxisEvent,
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
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_seat, delegate_shm, delegate_xdg_shell,
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
    },
};

use crate::args::CliArgs;
use crate::phone::{
    self, card_transform, parse_commands, runtime_path, task_line, write_atomic, PhoneMode,
    ShellCommand, ShellState, APP_H, APP_Y, NAV_BAR_H, NAV_BAR_Y, PHONE_H, PHONE_W, STATUS_BAR_H,
};
use crate::policy::{AsymmetricOutputManager, OutputRole};
use penumbra_gestures::{GestureStateMachine, NavigationGesture, Spring};

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

    pub seat: Seat<Self>,
    pub space: Space<Window>,
    pub root_shell: Option<Window>,
    pub output_a: Output,
    pub output_b: Option<Output>,
    pub is_convergence_active: bool,
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
}

/// A Wayland toplevel living on the phone output.
pub struct PhoneTask {
    pub id: u32,
    pub window: Window,
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
    pub(crate) fn phone_app_visible(&self) -> bool {
        self.shell_state.mode == PhoneMode::App && !self.phone_tasks.is_empty()
    }

    /// Resolves the surface receiving input at `pos` on the phone output.
    /// Returns the surface and its origin in global compositor coordinates.
    pub(crate) fn phone_focus_under(
        &self,
        pos: Point<f64, smithay::utils::Logical>,
    ) -> Option<(WlSurface, Point<f64, smithay::utils::Logical>)> {
        let map_a = layer_map_for_output(&self.output_a);

        // 1. Overlay layer (highest priority - active when drawer is open)
        if self.shell_state.drawer_open {
            if let Some(l) = map_a.layer_under(Layer::Overlay, pos) {
                let geo = map_a.layer_geometry(l).unwrap_or_default();
                return Some((
                    l.wl_surface().clone(),
                    (geo.loc.x as f64, geo.loc.y as f64).into(),
                ));
            }
        }

        // 2. Top layer (e.g. status bar: penumbra-top-bar)
        if let Some(l) = map_a.layer_under(Layer::Top, pos) {
            let geo = map_a.layer_geometry(l).unwrap_or_default();
            return Some((
                l.wl_surface().clone(),
                (geo.loc.x as f64, geo.loc.y as f64).into(),
            ));
        }

        // 3. Normal window (running phone app in the inset)
        let in_app_inset = pos.y >= APP_Y as f64 && pos.y < NAV_BAR_Y as f64;
        if self.phone_app_visible() && !self.shell_state.drawer_open && in_app_inset {
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

    /// Eases the animated overview carousel towards the shell's target index.
    /// While the user drags, it tracks the finger 1:1; on release it decelerates
    /// into a magnetic lock.
    pub(crate) fn advance_overview_scroll(&mut self) {
        let scroll_target = self.shell_state.overview_scroll;
        if self.shell_state.is_dragging {
            self.overview_scroll = scroll_target;
        } else {
            let scroll_diff = scroll_target - self.overview_scroll;
            self.overview_scroll += scroll_diff * 0.35;
            if scroll_diff.abs() < 0.002 {
                self.overview_scroll = scroll_target;
            }
        }
    }

    /// Reads shell state + commands and publishes the phone task list.
    pub(crate) fn poll_shell_ipc(&mut self) {
        if let Ok(text) = std::fs::read_to_string(runtime_path(phone::SHELL_STATE_FILE)) {
            let new_state = ShellState::parse(&text);
            if new_state != self.shell_state {
                info!("Shell state changed: {:?}", new_state);
                self.shell_state = new_state;
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

        if self.phone_tasks.is_empty() && self.shell_state.mode == PhoneMode::App {
            info!("All phone tasks closed -> returning to Home mode");
            self.shell_state.mode = PhoneMode::Home;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
        }

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

    fn apply_shell_command(&mut self, cmd: ShellCommand) {
        info!("Shell command: {:?}", cmd);
        match cmd {
            ShellCommand::Focus(id) => {
                if let Some(idx) = self.phone_tasks.iter().position(|t| t.id == id) {
                    let task = self.phone_tasks.remove(idx);
                    self.phone_tasks.insert(0, task);
                    // Carousel indices shift; recenter on the newly active task.
                    self.overview_scroll = 0.0;
                }
            }
            ShellCommand::Close(id) => {
                if let Some(task) = self.phone_tasks.iter().find(|t| t.id == id) {
                    if let Some(t) = task.window.toplevel() {
                        t.send_close();
                    }
                }
            }
            ShellCommand::CloseAll => {
                for task in &self.phone_tasks {
                    if let Some(t) = task.window.toplevel() {
                        t.send_close();
                    }
                }
            }
        }
    }
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
            self.phone_tasks.insert(0, PhoneTask { id, window });

            // Automatically switch to App mode and save state
            self.shell_state.mode = PhoneMode::App;
            self.shell_state.drawer_open = false;
            let _ = write_atomic(
                &runtime_path(phone::SHELL_STATE_FILE),
                &self.shell_state.serialize(),
            );
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

delegate_xdg_shell!(PenumbraServerState);
delegate_layer_shell!(PenumbraServerState);
delegate_compositor!(PenumbraServerState);
delegate_shm!(PenumbraServerState);
delegate_seat!(PenumbraServerState);
delegate_data_device!(PenumbraServerState);
delegate_output!(PenumbraServerState);

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
            seat,
            space,
            root_shell: None,
            output_a,
            output_b,
            is_convergence_active: self.args.convergence,
            pointer_location: (0.0, 0.0).into(),
            phone_tasks: Vec::new(),
            next_task_id: 1,
            shell_state: ShellState::default(),
            overview_scroll: 0.0,
            last_tasks_published: String::from("\u{0}"),
            phone_logical_size: (mode_a.size.w, mode_a.size.h),
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
            state.space.refresh();
            let is_overview = state.shell_state.mode == PhoneMode::Overview;
            if is_overview || last_ipc_poll.elapsed() >= Duration::from_millis(30) {
                state.poll_shell_ipc();
                last_ipc_poll = Instant::now();
            }
            display.flush_clients()?;

            // Track overview carousel scroll (1:1 while dragging, eased on release).
            state.advance_overview_scroll();

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
                        map.layers()
                            .filter(|l| l.layer() == layer_kind)
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

                // Phone Layer 1: active application inside the system insets
                let mut app_elements = Vec::new();
                if state.phone_app_visible() {
                    if let Some(surface) = state.active_phone_surface() {
                        let app_rect = phys_rect(0, APP_Y, PHONE_W, APP_H);
                        app_elements = cropped(
                            surface_elements(renderer, &surface, (0, APP_Y), 1.0),
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
                            .extend(cropped(surface_elements(renderer, s, (0, 0), 1.0), rect));
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
                // Phone Layer 1: applications workspace
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
    rect: Rectangle<i32, Physical>,
) -> Vec<CropRenderElement<SurfaceElement>> {
    elements
        .into_iter()
        .filter_map(|e| CropRenderElement::from_element(e, 1.0, rect))
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
