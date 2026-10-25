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
        input_method::InputMethodManagerState,
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::{Layer, WlrLayerShellState},
            xdg::XdgShellState,
        },
        shm::ShmState,
        text_input::TextInputManagerState,
        virtual_keyboard::VirtualKeyboardManagerState,
    },
};

use crate::{
    native_input::{self, InputState},
    phone::{
        self, resolve_app_icon_target, runtime_path, write_atomic, PhoneMode,
        ShellState, APP_H, APP_Y, CARD_H, CARD_STEP, CARD_Y, OVERVIEW_SCALE, PHONE_H, PHONE_W,
    },
    server::{
        cropped, send_frames_surface_tree, surface_elements, ClientState, PenumbraCompositor,
        PenumbraServerState, SurfaceElement,
    },
};
use penumbra_gestures::compute_home_collapse_scale;

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
    let keyboard_open = if layer == Layer::Top {
        let path = runtime_path(phone::SHELL_STATE_FILE).with_file_name("penumbra-keyboard-state");
        std::fs::read_to_string(path).map(|c| c.contains("open=1")).unwrap_or(false)
    } else {
        true
    };

    for surface in layer_map.layers().filter(|surface| surface.layer() == layer) {
        if surface.namespace().contains("keyboard") && !keyboard_open {
            continue;
        }
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

/// Active phone application, cropped to the app work area and transformed interactively during gestures.
fn app_elements(
    renderer: &mut GlesRenderer,
    server: &PenumbraServerState,
    canvas: Canvas,
) -> Vec<CropRenderElement<SurfaceElement>> {
    if !server.phone_app_visible() {
        return Vec::new();
    }
    let mut elements = Vec::new();
    let phone_clip = canvas.rect(0, 0, PHONE_W, PHONE_H);
    let switch_dx = server.nav_switch_dx;

    if server.nav_switch_active || server.nav_switch_spring.is_some() || switch_dx.abs() > 0.5 {
        // Quick Switch: 0.94x subtle card scale, 16px inter-card gutter
        let cur_scale = if server.nav_switch_spring.is_none() && !server.nav_switch_active {
            1.0
        } else {
            (1.0 - (switch_dx.abs() / (PHONE_W as f64 * 4.0))).clamp(0.94, 1.0)
        };
        let cur_w = (PHONE_W as f64 * cur_scale).round() as i32;
        let cur_h = (APP_H as f64 * cur_scale).round() as i32;
        let cur_y = APP_Y + (APP_H - cur_h) / 2;
        let cur_x = (switch_dx + ((PHONE_W - cur_w) / 2) as f64).round() as i32;
        let draw_scale = cur_scale * canvas.scale;

        if let Some(cur_surface) = server.active_phone_surface() {
            let rect = canvas.rect(cur_x, cur_y, cur_w, cur_h);
            if let Some(clip) = rect.intersection(phone_clip) {
                elements.extend(cropped(
                    surface_elements(renderer, &cur_surface, canvas.loc(cur_x, cur_y), draw_scale),
                    draw_scale,
                    clip,
                ));
            }
        }

        if server.phone_tasks.len() > 1 {
            if switch_dx > 0.0 {
                // Dragging right -> previous task enters from left with 16px gutter
                if let Some(prev_task) = server.phone_tasks.last() {
                    if let Some(toplevel) = prev_task.window.toplevel() {
                        let prev_x = cur_x - cur_w - 16;
                        let rect = canvas.rect(prev_x, cur_y, cur_w, cur_h);
                        if let Some(clip) = rect.intersection(phone_clip) {
                            elements.extend(cropped(
                                surface_elements(renderer, toplevel.wl_surface(), canvas.loc(prev_x, cur_y), draw_scale),
                                draw_scale,
                                clip,
                            ));
                        }
                    }
                }
            } else {
                // Dragging left -> next task enters from right with 16px gutter
                if let Some(next_task) = server.phone_tasks.get(1) {
                    if let Some(toplevel) = next_task.window.toplevel() {
                        let next_x = cur_x + cur_w + 16;
                        let rect = canvas.rect(next_x, cur_y, cur_w, cur_h);
                        if let Some(clip) = rect.intersection(phone_clip) {
                            elements.extend(cropped(
                                surface_elements(renderer, toplevel.wl_surface(), canvas.loc(next_x, cur_y), draw_scale),
                                draw_scale,
                                clip,
                            ));
                        }
                    }
                }
            }
        }
        return elements;
    }

    if server.shell_state.mode == PhoneMode::Overview
        && server.nav_target != Some(penumbra_gestures::NavigationTarget::Fullscreen)
        && !server.nav_gesture_active
    {
        return Vec::new();
    }

    let expanding_task = if server.shell_state.mode == PhoneMode::Overview {
        server.phone_tasks.get(server.shell_state.overview_index).or_else(|| server.phone_tasks.first())
    } else {
        server.phone_tasks.first()
    };
    let Some(surface) = expanding_task.and_then(|t| t.window.toplevel()).map(|tl| tl.wl_surface().clone()) else {
        return Vec::new();
    };

    let nav_progress = if server.is_nav_animating() {
        server.nav_gesture_progress
    } else {
        0.0
    };

    if nav_progress > 0.001 {
        if server.nav_target == Some(penumbra_gestures::NavigationTarget::Home) {
            // Dismiss to home: track and collapse window into target icon coordinate
            let active_app_id = server.phone_tasks.first().map(|t| t.app_id()).unwrap_or_default();
            let (target_icon_x, target_icon_y) = resolve_app_icon_target(&active_app_id);
            let t = nav_progress.clamp(0.0, 1.0);
            let scale_factor = compute_home_collapse_scale(t);
            let w = (PHONE_W as f64 * scale_factor).round() as i32;
            let h = (APP_H as f64 * scale_factor).round() as i32;

            let initial_cx = (PHONE_W as f64) / 2.0;
            let initial_cy = (APP_Y as f64) + (APP_H as f64) / 2.0;
            let cx = initial_cx * (1.0 - t) + (target_icon_x as f64) * t;
            let cy = initial_cy * (1.0 - t) + (target_icon_y as f64) * t;

            let x = (cx - (w as f64) / 2.0).round() as i32;
            let y = (cy - (h as f64) / 2.0).round() as i32;

            let draw_scale = scale_factor * canvas.scale;
            let card_clip = canvas.rect(x, y, w, h);
            if let Some(clip) = card_clip.intersection(phone_clip) {
                elements.extend(cropped(
                    surface_elements(renderer, &surface, canvas.loc(x, y), draw_scale),
                    draw_scale,
                    clip,
                ));
            }
        } else if server.nav_target == Some(penumbra_gestures::NavigationTarget::Overview)
            || (server.nav_gesture_active && server.shell_state.mode != PhoneMode::Overview)
        {
            // Handled smoothly by overview_elements carousel (all cards active and equal distance)
            return elements;
        } else {
            // Active window scaling down to card position during gesture pull-up
            let scale_factor = penumbra_gestures::compute_window_scale(nav_progress);
            let w = (PHONE_W as f64 * scale_factor).round() as i32;
            let h = (APP_H as f64 * scale_factor).round() as i32;
            let x = ((PHONE_W - w) / 2).max(0);
            let target_y = ((PHONE_H - CARD_H) / 2) as f64;
            let y = (APP_Y as f64 * (1.0 - nav_progress) + target_y * nav_progress).round() as i32;

            let draw_scale = scale_factor * canvas.scale;
            let card_clip = canvas.rect(x, y, w, h);
            if let Some(clip) = card_clip.intersection(phone_clip) {
                elements.extend(cropped(
                    surface_elements(renderer, &surface, canvas.loc(x, y), draw_scale),
                    draw_scale,
                    clip,
                ));
            }
        }
    } else {
        elements.extend(cropped(
            surface_elements(renderer, &surface, canvas.loc(0, APP_Y), canvas.scale),
            canvas.scale,
            canvas.rect(0, APP_Y, PHONE_W, APP_H),
        ));
    }
    elements
}

/// Computes the uniform scale and base Y coordinate for the overview carousel.
/// As the user pulls up higher towards the top of the screen (free play),
/// all cards decrease in size together and lift up, keeping equal distance.
fn compute_carousel_transform(server: &PenumbraServerState) -> (f64, i32) {
    if server.shell_state.mode == PhoneMode::Overview {
        if server.nav_gesture_active && server.nav_gesture_dy < 0.0 {
            // Dragging higher up in Overview: elastic zoom-out
            let drag_up = (-server.nav_gesture_dy).max(0.0);
            let p_up = (drag_up / (PHONE_H as f64 * 0.7)).clamp(0.0, 1.0);
            let scale = OVERVIEW_SCALE - p_up * 0.12; // 0.70 down to 0.58
            let y_lift = (p_up * 70.0).round() as i32;
            (scale, CARD_Y - y_lift)
        } else {
            (OVERVIEW_SCALE, CARD_Y)
        }
    } else if (server.is_nav_animating() || server.nav_gesture_active)
        && server.nav_target != Some(penumbra_gestures::NavigationTarget::Home)
    {
        // Swiping up from an app towards Overview
        let p = server.nav_gesture_progress;
        if p <= 1.0 {
            let scale = 1.0 - p * (1.0 - OVERVIEW_SCALE); // 1.0 down to 0.70
            let y = (APP_Y as f64 * (1.0 - p) + CARD_Y as f64 * p).round() as i32;
            (scale, y)
        } else {
            // Pulled all the way up: zoom out from 0.70 down to ~0.58 and lift
            let over = (p - 1.0).clamp(0.0, 1.0);
            let scale = OVERVIEW_SCALE - over * 0.12;
            let y = CARD_Y - (over * 70.0).round() as i32;
            (scale, y)
        }
    } else {
        (OVERVIEW_SCALE, CARD_Y)
    }
}

/// Overview carousel cards, clipped to the phone canvas.
fn overview_elements(
    renderer: &mut GlesRenderer,
    server: &PenumbraServerState,
    canvas: Canvas,
) -> Vec<CropRenderElement<SurfaceElement>> {
    let mut cards = Vec::new();
    let is_spring_to_overview = server.nav_target == Some(penumbra_gestures::NavigationTarget::Overview);
    let is_dragging_to_overview = server.nav_gesture_active
        && server.nav_gesture_progress > 0.01
        && server.nav_target != Some(penumbra_gestures::NavigationTarget::Home);

    if (server.shell_state.mode != PhoneMode::Overview && !is_spring_to_overview && !is_dragging_to_overview)
        || server.shell_state.drawer_open
    {
        return cards;
    }
    let phone_clip = canvas.rect(0, 0, PHONE_W, PHONE_H);
    let (scale, base_y) = compute_carousel_transform(server);
    let card_scale = scale * canvas.scale;
    let w = (PHONE_W as f64 * scale).round() as i32;
    let h = (APP_H as f64 * scale).round() as i32;
    let step = ((CARD_STEP as f64) * (scale / OVERVIEW_SCALE)).round() as i32;
    let center_x = ((PHONE_W - w) / 2) as f64;

    let is_expanding_to_fullscreen = server.shell_state.mode == PhoneMode::Overview
        && ((server.nav_gesture_active && server.nav_gesture_dy > 0.0)
            || server.nav_target == Some(penumbra_gestures::NavigationTarget::Fullscreen));

    // Expansion progress: 0.0 at rest (Overview), 1.0 when expanded (Fullscreen)
    let p_expand = if is_expanding_to_fullscreen {
        (1.0 - server.nav_gesture_progress).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // If active card has expanded into near-fullscreen (>= 85%), all adjacent cards are offscreen
    if is_expanding_to_fullscreen && p_expand >= 0.85 {
        return cards;
    }

    for (index, task) in server.phone_tasks.iter().enumerate() {
        if is_expanding_to_fullscreen && index == server.shell_state.overview_index {
            continue;
        }

        let dist = (index as f64) - server.overview_scroll;
        let mut x = (center_x - dist * (step as f64)).round() as i32;

        let y = if index == server.shell_state.overview_index && server.shell_state.mode == PhoneMode::Overview {
            base_y + server.shell_state.active_card_dy.round() as i32
        } else {
            base_y
        };

        // Disperse adjacent cards horizontally away from expanding center card
        if is_expanding_to_fullscreen {
            let push_dist = (p_expand * (PHONE_W as f64 * 0.95)).round() as i32;
            if index < server.shell_state.overview_index {
                x -= push_dist;
            } else if index > server.shell_state.overview_index {
                x += push_dist;
            }
        }

        if x + w <= 0 || x >= PHONE_W {
            continue;
        }
        let Some(toplevel) = task.window.toplevel() else {
            continue;
        };
        let card = canvas.rect(x, y, w, h);
        if let Some(clip) = card.intersection(phone_clip) {
            let elements = cropped(
                surface_elements(renderer, toplevel.wl_surface(), canvas.loc(x, y), card_scale),
                card_scale,
                clip,
            );
            cards.extend(elements);
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

    // Phone Layer 0.5: Dual-Kawase Blurred Wallpaper & Dimming Scrim modulated by gesture progress
    let _nav_progress = if server.is_nav_animating() {
        server.nav_gesture_progress
    } else if server.shell_state.mode == PhoneMode::Overview {
        1.0
    } else {
        0.0
    };

    // Blurred wallpaper overlay is disabled during swiping to maintain crisp background and eliminate tear artifacts
    let blurred_bg: Vec<smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>> = Vec::new();

    let scrim: Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement> = if server.shell_state.mode == PhoneMode::Overview {
        let scrim_alpha = 0.22f32;
        let scrim_buf = smithay::backend::renderer::element::solid::SolidColorBuffer::new(
            (PHONE_W, PHONE_H),
            Color32F::new(0.0431, 0.0392, 0.0549, scrim_alpha),
        );
        smithay::backend::renderer::element::AsRenderElements::<GlesRenderer>::render_elements(
            &scrim_buf,
            renderer,
            canvas.loc(0, 0).into(),
            smithay::utils::Scale::from(canvas.scale),
            1.0,
        )
    } else {
        Vec::new()
    };

    // Phone Layer 1: overview carousel thumbnails.
    let overview = overview_elements(renderer, server, canvas);
    // Phone Layer 2: active application inside the system insets (drawn above carousel so expanding card sits on top).
    let apps = app_elements(renderer, server, canvas);

    // (Card corner masks removed — they were causing UI breakage. Flat card rendering is used instead.)

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
    if !blurred_bg.is_empty() {
        draw_render_elements::<GlesRenderer, _, _>(&mut frame, scale, &blurred_bg, &[damage])?;
    }
    if !scrim.is_empty() {
        draw_render_elements::<GlesRenderer, _, _>(&mut frame, scale, &scrim, &[damage])?;
    }
    let (carousel_s, _) = compute_carousel_transform(server);
    let overview_scale = carousel_s * canvas.scale;
    draw_render_elements(&mut frame, overview_scale, &overview, &[damage])?;

    let active_app_scale = if (server.is_nav_animating() || server.nav_gesture_active)
        && server.nav_target == Some(penumbra_gestures::NavigationTarget::Home)
    {
        let t = server.nav_gesture_progress.clamp(0.0, 1.0);
        compute_home_collapse_scale(t) * canvas.scale
    } else if (server.is_nav_animating() || server.nav_gesture_active) && server.nav_gesture_progress > 0.001 {
        let scale_factor = penumbra_gestures::compute_window_scale(server.nav_gesture_progress);
        scale_factor * canvas.scale
    } else {
        canvas.scale
    };
    draw_render_elements(&mut frame, active_app_scale, &apps, &[damage])?;

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
    let text_input_state = TextInputManagerState::new::<PenumbraServerState>(&dh);
    let input_method_state = InputMethodManagerState::new::<PenumbraServerState, _>(&dh, |_client| true);
    let virtual_keyboard_state = VirtualKeyboardManagerState::new::<PenumbraServerState, _>(&dh, |_client| true);
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
        text_input_state,
        input_method_state,
        virtual_keyboard_state,
        last_text_input_active: false,
        seat,
        space,
        root_shell: None,
        output_a,
        output_b: None,
        is_convergence_active: false,
        is_native: true,
        pointer_location: (0.0, 0.0).into(),
        phone_tasks: Vec::new(),
        next_task_id: 1,
        shell_state: ShellState::default(),
        overview_scroll: 0.0,
        last_tasks_published: String::from("\u{0}"),
        phone_logical_size: (phone::PHONE_W, phone::PHONE_H),
        screen_blanked: false,
        saved_brightness: 2500,
        nav_gesture_active: false,
        nav_gesture_progress: 0.0,
        nav_gesture_dy: 0.0,
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
                phone::PHONE_W,
                phone::PHONE_H,
            );
            Some(cache)
        },
        blurred_wallpaper_buffer: {
            let cache = crate::wallpaper_blur::BlurredWallpaperCache::load_and_blur(
                None,
                phone::PHONE_W,
                phone::PHONE_H,
            );
            Some(cache.to_render_buffer())
        },
        card_corner_masks: crate::card_rounding::CardCornerMasks::default(),
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
        state.server.update_text_input_state();
        state.server.space.refresh();

        // Shell state arrives through files; poll faster while the overview animates or gesture is active.
        let is_overview = state.server.shell_state.mode == PhoneMode::Overview;
        let is_animating = state.server.is_nav_animating();
        if is_overview || is_animating || state.last_ipc_poll.elapsed() >= Duration::from_millis(16) {
            state.server.poll_shell_ipc();
            state.last_ipc_poll = Instant::now();
        }
        state.server.advance_nav_spring();
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
