//! Native DRM/libseat backend entry point.

use std::error::Error;
use std::time::{Duration, Instant};

use smithay::{
    backend::{
        allocator::{
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{DrmDevice, DrmDeviceFd, GbmBufferedSurface},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::utils::CropRenderElement,
            gles::GlesRenderer,
            utils::draw_render_elements,
            Bind, Color32F, Frame, ImportDma, Renderer,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::UdevBackend,
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        drm::control::{connector, Device as DrmControlDevice},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{Client, Display, ListeningSocket},
    },
    utils::{DeviceFd, Physical, Rectangle, Size, Transform},
    wayland::{
        compositor::CompositorState,
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::{Layer, WlrLayerShellState},
            xdg::XdgShellState,
        },
        shm::ShmState,
    },
};

use crate::{
    native_input::{self, InputState},
    phone::{
        self, card_transform, runtime_path, write_atomic, PhoneMode, ShellState, APP_H, APP_Y,
        PHONE_H, PHONE_W,
    },
    server::{
        cropped, send_frames_surface_tree, surface_elements, ClientState, PenumbraCompositor,
        PenumbraServerState, SurfaceElement,
    },
};

type NativeSurface = GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, ()>;

/// Logical phone canvas to physical panel mapping: uniform scale plus letterbox offset.
#[derive(Clone, Copy)]
pub(crate) struct Canvas {
    pub(crate) scale: f64,
    /// Logical offset that centers the canvas on the panel.
    offset: (i32, i32),
}

impl Canvas {
    fn new(physical: (u32, u32), logical: (u32, u32)) -> Self {
        let scale = phone::native_output_scale(physical, logical);
        let offset = phone::letterbox_offset(physical, logical, scale);
        Self { scale, offset }
    }

    /// Physical pixel position of a logical point.
    pub(crate) fn loc(&self, x: i32, y: i32) -> (i32, i32) {
        (
            ((x + self.offset.0) as f64 * self.scale).round() as i32,
            ((y + self.offset.1) as f64 * self.scale).round() as i32,
        )
    }

    /// Physical rectangle covering a logical rectangle.
    pub(crate) fn rect(&self, x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        let (x0, y0) = self.loc(x, y);
        let (x1, y1) = self.loc(x + w, y + h);
        Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into())
    }

    /// Logical position of a physical panel pixel. May fall outside the canvas on letterbox bars.
    pub(crate) fn to_logical(&self, x: f64, y: f64) -> (f64, f64) {
        (
            x / self.scale - self.offset.0 as f64,
            y / self.scale - self.offset.1 as f64,
        )
    }
}

struct NativeState {
    surface: NativeSurface,
    renderer: GlesRenderer,
    canvas: Canvas,
    crtc: smithay::reexports::drm::control::crtc::Handle,
    listener: ListeningSocket,
    display: Display<PenumbraServerState>,
    server: PenumbraServerState,
    clients: Vec<Client>,
    input: InputState,
    started: Instant,
    last_ipc_poll: Instant,
}

/// Layer-shell surfaces on `layer` of Output A, placed on the logical canvas.
fn layer_elements(
    renderer: &mut GlesRenderer,
    output: &Output,
    canvas: Canvas,
    layer: Layer,
) -> Vec<SurfaceElement> {
    let layer_map = layer_map_for_output(output);
    let mut elements = Vec::new();
    for surface in layer_map.layers().filter(|surface| surface.layer() == layer) {
        let Some(geometry) = layer_map.layer_geometry(surface) else {
            continue;
        };
        // Background surfaces are always anchored to the canvas origin.
        let (x, y) = if layer == Layer::Background {
            (0, 0)
        } else {
            (geometry.loc.x, geometry.loc.y)
        };
        elements.extend(surface_elements(
            renderer,
            surface.wl_surface(),
            canvas.loc(x, y),
            canvas.scale,
        ));
    }
    elements
}

/// Active phone application, cropped to the app work area.
fn app_elements(
    renderer: &mut GlesRenderer,
    server: &PenumbraServerState,
    canvas: Canvas,
) -> Vec<CropRenderElement<SurfaceElement>> {
    if !server.phone_app_visible() {
        return Vec::new();
    }
    let Some(surface) = server.active_phone_surface() else {
        return Vec::new();
    };
    cropped(
        surface_elements(renderer, &surface, canvas.loc(0, APP_Y), canvas.scale),
        canvas.rect(0, APP_Y, PHONE_W, APP_H),
    )
}

/// Overview carousel cards, clipped to the phone canvas.
///
/// Each card is returned with its own draw scale: element size is resolved at draw time,
/// so the card scale (carousel scale times canvas scale) must be passed to the draw call.
fn overview_elements(
    renderer: &mut GlesRenderer,
    server: &PenumbraServerState,
    canvas: Canvas,
) -> Vec<(f64, Vec<CropRenderElement<SurfaceElement>>)> {
    let mut cards = Vec::new();
    if server.shell_state.mode != PhoneMode::Overview || server.shell_state.drawer_open {
        return cards;
    }
    let phone_clip = canvas.rect(0, 0, PHONE_W, PHONE_H);
    for (index, task) in server.phone_tasks.iter().enumerate() {
        let (x, y, w, h, scale) = card_transform(
            index,
            server.overview_scroll,
            server.shell_state.overview_index,
            server.shell_state.active_card_dy,
        );
        if x + w <= 0 || x >= PHONE_W {
            continue;
        }
        let Some(toplevel) = task.window.toplevel() else {
            continue;
        };
        let card = canvas.rect(x, y, w, h);
        if let Some(clip) = card.intersection(phone_clip) {
            let draw_scale = scale * canvas.scale;
            let elements = cropped(
                surface_elements(renderer, toplevel.wl_surface(), canvas.loc(x, y), draw_scale),
                clip,
            );
            cards.push((draw_scale, elements));
        }
    }
    cards
}

/// Composes one frame in paint order (bottom to top) and queues it on the panel.
fn render_frame(state: &mut NativeState, buffer: &mut Dmabuf) -> Result<(), Box<dyn Error>> {
    let NativeState {
        surface,
        renderer,
        canvas,
        server,
        started,
        ..
    } = state;
    let canvas = *canvas;
    let (width, height) = surface.current_mode().size();
    let size: Size<i32, Physical> = (width as i32, height as i32).into();
    let damage = Rectangle::from_size(size);

    // Phone Layer 0: homescreen and wallpaper on the background layer.
    let background = layer_elements(renderer, &server.output_a, canvas, Layer::Background);
    let bottom = layer_elements(renderer, &server.output_a, canvas, Layer::Bottom);
    // Phone Layer 1: active application inside the system insets.
    let apps = app_elements(renderer, server, canvas);
    // Phone Layer 2: overview carousel thumbnails.
    let overview = overview_elements(renderer, server, canvas);
    // Phone Layer 3: status bar and nav bar.
    let top = layer_elements(renderer, &server.output_a, canvas, Layer::Top);
    // Quick settings, Power dialog, and Volume modal sit on the Overlay layer.
    let overlay = if server.shell_state.drawer_open
        || server.shell_state.power_dialog_open
        || server.shell_state.volume_modal_open
    {
        layer_elements(renderer, &server.output_a, canvas, Layer::Overlay)
    } else {
        Vec::new()
    };

    // Canvas-space elements are drawn at the canvas scale so they fill the panel mapping.
    let scale = canvas.scale;
    let mut framebuffer = renderer.bind(buffer)?;
    let mut frame = renderer.render(&mut framebuffer, size, Transform::Normal)?;
    frame.clear(Color32F::new(0.02, 0.02, 0.03, 1.0), &[damage])?;
    draw_render_elements(&mut frame, scale, &background, &[damage])?;
    draw_render_elements(&mut frame, scale, &bottom, &[damage])?;
    draw_render_elements(&mut frame, scale, &apps, &[damage])?;
    for (card_scale, card) in &overview {
        draw_render_elements(&mut frame, *card_scale, card, &[damage])?;
    }
    draw_render_elements(&mut frame, scale, &top, &[damage])?;
    draw_render_elements(&mut frame, scale, &overlay, &[damage])?;
    frame.finish()?.wait()?;

    // Frame callbacks let clients schedule their next buffer.
    let time = started.elapsed().as_millis() as u32;
    for window in server.space.elements() {
        if let Some(toplevel) = window.toplevel() {
            send_frames_surface_tree(toplevel.wl_surface(), time);
        }
    }
    for layer in layer_map_for_output(&server.output_a).layers() {
        send_frames_surface_tree(layer.wl_surface(), time);
    }

    surface.queue_buffer(None, None, ())?;
    Ok(())
}

pub fn run(_compositor: &mut PenumbraCompositor) -> Result<(), Box<dyn Error>> {
    let (mut session, session_notifier) = LibSeatSession::new()?;
    let udev = UdevBackend::new(session.seat())?;
    let (_, path) = udev
        .device_list()
        .next()
        .ok_or("libseat reported no DRM devices")?;
    tracing::info!(device = ?path, "Opening native DRM device");
    let fd = session.open(
        path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (mut device, _drm_notifier) = DrmDevice::new(drm_fd.clone(), false)
        .map_err(|error| format!("failed to initialize DRM device {:?}: {error}", path))?;

    let resources = drm_fd.resource_handles()?;
    let connector_handle = resources
        .connectors()
        .iter()
        .find_map(|handle| {
            let info = drm_fd.get_connector(*handle, true).ok()?;
            (info.state() == connector::State::Connected && !info.modes().is_empty())
                .then_some((*handle, info))
        })
        .ok_or("no connected DRM connector with a mode")?;
    let (connector_handle, connector_info) = connector_handle;
    let mode = connector_info
        .modes()
        .iter()
        .max_by_key(|mode| mode.size().0 * mode.size().1)
        .cloned()
        .ok_or("connected DRM connector has no usable mode")?;

    let compatible_crtcs = connector_info
        .encoders()
        .iter()
        .filter_map(|encoder| drm_fd.get_encoder(*encoder).ok())
        .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
        .collect::<Vec<_>>();
    let current_crtc = connector_info
        .current_encoder()
        .and_then(|encoder| drm_fd.get_encoder(encoder).ok())
        .and_then(|encoder| encoder.crtc())
        .filter(|crtc| compatible_crtcs.contains(crtc));
    let crtc = current_crtc
        .or_else(|| {
            compatible_crtcs.iter().copied().find(|crtc| {
                drm_fd
                    .get_crtc(*crtc)
                    .map(|info| info.mode().is_none())
                    .unwrap_or(false)
            })
        })
        .or_else(|| compatible_crtcs.first().copied())
        .ok_or("connected connector has no compatible CRTC")?;
    tracing::info!(
        connector = ?connector_handle,
        compatible_crtcs = ?compatible_crtcs,
        current_crtc = ?current_crtc,
        selected_crtc = ?crtc,
        "Selected connector-compatible DRM CRTC"
    );

    let drm_surface = device.create_surface(crtc, mode, &[connector_handle])?;
    let gbm = GbmDevice::new(drm_fd.clone())?;
    let egl_device = EGLDevice::enumerate()?
        .find(|candidate| {
            candidate.drm_device_path().ok().as_deref() == Some(path)
                || candidate.render_device_path().ok().as_deref() == Some(path)
        })
        .ok_or("no EGL device matches the DRM node")?;
    let egl_display = unsafe { EGLDisplay::new(egl_device)? };
    let egl_context = EGLContext::new(&egl_display)?;
    let renderer = unsafe { GlesRenderer::new(egl_context)? };
    let renderer_formats = renderer.dmabuf_formats().into_iter().collect::<Vec<_>>();
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let surface = GbmBufferedSurface::<_, ()>::new(
        drm_surface,
        allocator,
        &[Fourcc::Abgr8888, Fourcc::Argb8888],
        renderer_formats,
    )?;

    let socket_name = &_compositor.args.socket;
    let listener = ListeningSocket::bind(socket_name)?;
    let display: Display<PenumbraServerState> = Display::new()?;
    let dh = display.handle();
    let compositor_state = CompositorState::new::<PenumbraServerState>(&dh);
    let shm_state = ShmState::new::<PenumbraServerState>(&dh, vec![]);
    let mut seat_state = smithay::input::SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "penumbra-seat");
    // The pointer capability carries libinput mouse input and emulated touch.
    let pointer = seat.add_pointer();
    let _keyboard = seat.add_keyboard(Default::default(), 200, 200)?;
    let xdg_shell_state = XdgShellState::new::<PenumbraServerState>(&dh);
    let layer_shell_state = WlrLayerShellState::new::<PenumbraServerState>(&dh);
    let data_device_state = DataDeviceState::new::<PenumbraServerState>(&dh);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<PenumbraServerState>(&dh);
    let output_a = Output::new(
        "DSI-1".to_string(),
        PhysicalProperties {
            size: (65, 145).into(),
            subpixel: Subpixel::Unknown,
            make: "Penumbra".to_string(),
            model: "Phone DSI Mobile".to_string(),
        },
    );
    // Output A advertises the logical phone canvas so layer-shell arrangement matches the design
    // (nav bar at y 883, full-width chrome). The panel itself keeps its DRM mode for scanout.
    let mode_a = Mode {
        size: (PHONE_W, PHONE_H).into(),
        refresh: mode.vrefresh() as i32 * 1000,
    };
    output_a.create_global::<PenumbraServerState>(&dh);
    output_a.set_preferred(mode_a);
    output_a.change_current_state(
        Some(mode_a),
        Some(smithay::utils::Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    let mut space = smithay::desktop::space::Space::default();
    space.map_output(&output_a, (0, 0));
    let server = PenumbraServerState {
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
        output_b: None,
        is_convergence_active: false,
        pointer_location: (0.0, 0.0).into(),
        phone_tasks: Vec::new(),
        next_task_id: 1,
        shell_state: ShellState::default(),
        overview_scroll: 0.0,
        last_tasks_published: String::from("\u{0}"),
        phone_logical_size: (phone::PHONE_W, phone::PHONE_H),
        screen_blanked: false,
        saved_brightness: 2500,
    };
    let _ = std::fs::remove_file(runtime_path(phone::SHELL_STATE_FILE));
    let _ = std::fs::remove_file(runtime_path(phone::SHELL_CMD_FILE));
    let _ = write_atomic(&runtime_path(phone::WINDOWS_FILE), "");

    let mut event_loop = smithay::reexports::calloop::EventLoop::<NativeState>::try_new()?;
    let loop_handle = event_loop.handle();
    loop_handle.insert_source(_drm_notifier, |event, _, state| {
        if let smithay::backend::drm::DrmEvent::VBlank(crtc) = event {
            if crtc == state.crtc {
                if let Err(error) = state.surface.frame_submitted() {
                    tracing::error!(?error, "Failed to retire native DRM frame");
                }
            }
        }
    })?;
    loop_handle.insert_source(session_notifier, |event, _, _state| match event {
        SessionEvent::ActivateSession => tracing::info!("Native seat activated"),
        SessionEvent::PauseSession => tracing::warn!("Native seat paused"),
    })?;

    // libinput delivers touch and pointer events from the session seat.
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|()| "libinput could not be assigned the session seat")?;
    loop_handle.insert_source(LibinputInputBackend::new(libinput), |event, _, state| {
        native_input::handle(&mut state.server, &mut state.input, event);
    })?;

    // Shell surfaces are laid out on the fixed logical canvas; the panel is a uniform scale of it.
    let canvas = Canvas::new(
        (mode.size().0 as u32, mode.size().1 as u32),
        (PHONE_W as u32, PHONE_H as u32),
    );
    let panel = (mode.size().0 as i32, mode.size().1 as i32);
    let now = Instant::now();
    let mut state = NativeState {
        surface,
        renderer,
        canvas,
        crtc,
        listener,
        display,
        server,
        clients: Vec::new(),
        input: InputState::new(pointer, canvas, panel),
        started: now,
        last_ipc_poll: now,
    };
    tracing::info!("Native DRM backend is presenting; waiting for device events");
    event_loop.run(Some(Duration::from_millis(16)), &mut state, |state| {
        if let Ok(Some(stream)) = state.listener.accept() {
            match state
                .display
                .handle()
                .insert_client(stream, std::sync::Arc::new(ClientState::default()))
            {
                Ok(client) => state.clients.push(client),
                Err(error) => tracing::warn!(?error, "Failed to accept Wayland client"),
            }
        }
        if let Err(error) = state.display.dispatch_clients(&mut state.server) {
            tracing::error!(?error, "Wayland client dispatch failed");
        }
        state.server.space.refresh();

        // Shell state arrives through files; poll faster while the overview animates.
        let is_overview = state.server.shell_state.mode == PhoneMode::Overview;
        if is_overview || state.last_ipc_poll.elapsed() >= Duration::from_millis(30) {
            state.server.poll_shell_ipc();
            state.last_ipc_poll = Instant::now();
        }
        state.server.advance_overview_scroll();

        if let Some(press_time) = state.input.power_press_time {
            if !state.input.power_dialog_triggered && press_time.elapsed() >= Duration::from_millis(500) {
                state.input.power_dialog_triggered = true;
                tracing::info!("Hardware power button hold (>= 500ms) -> trigger Power Dialog");
                state.server.show_power_dialog();
            }
        }

        if let Err(error) = state.display.flush_clients() {
            tracing::error!(?error, "Wayland client flush failed");
        }

        // Rendering waits until the previous frame has been retired by DRM vblank.
        let Ok((mut buffer, _)) = state.surface.next_buffer() else {
            return;
        };
        if let Err(error) = render_frame(state, &mut buffer) {
            tracing::error!(error = %error, "Native frame render failed");
        }
    })?;
    Ok(())
}
