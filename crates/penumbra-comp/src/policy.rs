//! Dual Output Policies: Asymmetric routing for mobile phone (DSI) and external desktop (HDMI).

use std::collections::HashMap;

/// Physical output role in the Penumbra convergence model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputRole {
    /// Output A: Primary Phone Display (DSI, 412x915 @ 20:9).
    /// Enforces strict single-window fullscreen layout, gesture interception, and layer-shell overlay.
    PhoneDsi,
    /// Output B: Secondary External Monitor (HDMI/Type-C, 1920x1080 @ 16:9).
    /// Enforces desktop convergence policy with floating windows, desktop dock, and mouse pointer.
    ExternalDesktopHdmi,
}

/// Surface display mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceDisplayMode {
    FullscreenExclusive,
    FloatingWindow {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    LayerShellOverlay,
}

/// Output display configuration and enforced constraints.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub role: OutputRole,
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub refresh_rate_mhz: u32,
    pub scale: f64,
    pub allows_floating: bool,
    pub enforces_gestures: bool,
    pub pointer_cursor_enabled: bool,
}

impl OutputConfig {
    /// Mobile Phone DSI output policy: 412x915 @ 120Hz.
    pub fn phone_dsi() -> Self {
        Self {
            role: OutputRole::PhoneDsi,
            name: "DSI-1 (Pixel Mobile)".to_string(),
            width: 412,
            height: 915,
            refresh_rate_mhz: 120_000,
            scale: 1.0,
            allows_floating: false,
            enforces_gestures: true,
            pointer_cursor_enabled: false,
        }
    }

    /// External HDMI Desktop output policy: 1920x1080 @ 60Hz.
    pub fn external_hdmi() -> Self {
        Self {
            role: OutputRole::ExternalDesktopHdmi,
            name: "HDMI-A-1 (Convergence Desktop)".to_string(),
            width: 1280,
            height: 720,
            refresh_rate_mhz: 60_000,
            scale: 1.0,
            allows_floating: true,
            enforces_gestures: false,
            pointer_cursor_enabled: true,
        }
    }
}

/// Surface metadata tracked by the compositor policy manager.
#[derive(Debug, Clone)]
pub struct ManagedSurface {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub assigned_output: OutputRole,
    pub display_mode: SurfaceDisplayMode,
    pub is_focused: bool,
}

/// Asymmetric output router managing convergence, hotplug, and surface layouts.
#[derive(Debug, Default)]
pub struct AsymmetricOutputManager {
    outputs: HashMap<OutputRole, OutputConfig>,
    surfaces: Vec<ManagedSurface>,
    active_dsi_surface_id: Option<u64>,
}

impl AsymmetricOutputManager {
    pub fn new() -> Self {
        let mut manager = Self::default();
        // Phone DSI is always present as the primary mobile display
        manager
            .outputs
            .insert(OutputRole::PhoneDsi, OutputConfig::phone_dsi());
        manager
    }

    /// Hotplugs the secondary external monitor (HDMI).
    pub fn hotplug_external_display(&mut self) -> &OutputConfig {
        self.outputs
            .entry(OutputRole::ExternalDesktopHdmi)
            .or_insert_with(OutputConfig::external_hdmi)
    }

    /// Unplugs the external monitor, migrating all floating desktop surfaces back to mobile.
    pub fn unplug_external_display(&mut self) {
        self.outputs.remove(&OutputRole::ExternalDesktopHdmi);

        // Migrate surfaces previously on HDMI back to DSI (fullscreen)
        for surface in &mut self.surfaces {
            if surface.assigned_output == OutputRole::ExternalDesktopHdmi {
                surface.assigned_output = OutputRole::PhoneDsi;
                surface.display_mode = SurfaceDisplayMode::FullscreenExclusive;
            }
        }
    }

    /// Checks whether external desktop convergence is currently active.
    pub fn is_convergence_active(&self) -> bool {
        self.outputs.contains_key(&OutputRole::ExternalDesktopHdmi)
    }

    /// Returns configuration for an output role if active.
    pub fn output_config(&self, role: OutputRole) -> Option<&OutputConfig> {
        self.outputs.get(&role)
    }

    /// Registers a new surface and computes its layout according to output policy.
    pub fn register_surface(&mut self, id: u64, app_id: &str, title: &str) -> &ManagedSurface {
        // If external HDMI is connected and app is desktop-oriented (or second surface), route to HDMI
        let (assigned_output, display_mode) =
            if self.is_convergence_active() && self.active_dsi_surface_id.is_some() {
                (
                    OutputRole::ExternalDesktopHdmi,
                    SurfaceDisplayMode::FloatingWindow {
                        x: 100 + ((id as i32 * 40) % 600),
                        y: 80 + ((id as i32 * 30) % 400),
                        width: 900,
                        height: 600,
                    },
                )
            } else {
                // Strict single-window fullscreen layout for mobile phone DSI
                self.active_dsi_surface_id = Some(id);
                (
                    OutputRole::PhoneDsi,
                    SurfaceDisplayMode::FullscreenExclusive,
                )
            };

        let surface = ManagedSurface {
            id,
            app_id: app_id.to_string(),
            title: title.to_string(),
            assigned_output,
            display_mode,
            is_focused: true,
        };

        self.surfaces.push(surface);
        self.surfaces.last().unwrap()
    }

    /// Switches the active fullscreen app on Phone DSI.
    pub fn switch_active_dsi_surface(&mut self, surface_id: u64) -> bool {
        if self
            .surfaces
            .iter()
            .any(|s| s.id == surface_id && s.assigned_output == OutputRole::PhoneDsi)
        {
            self.active_dsi_surface_id = Some(surface_id);
            for s in &mut self.surfaces {
                s.is_focused = s.id == surface_id;
            }
            true
        } else {
            false
        }
    }

    pub fn surfaces_for_output(&self, role: OutputRole) -> Vec<&ManagedSurface> {
        self.surfaces
            .iter()
            .filter(|s| s.assigned_output == role)
            .collect()
    }

    pub fn active_phone_surface(&self) -> Option<&ManagedSurface> {
        let active_id = self.active_dsi_surface_id?;
        self.surfaces.iter().find(|s| s.id == active_id)
    }
}
