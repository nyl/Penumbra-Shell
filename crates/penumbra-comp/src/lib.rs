//! Penumbra Compositor library.

pub mod args;
#[cfg(feature = "native-drm")]
pub mod native;
#[cfg(feature = "native-drm")]
mod native_input;
pub mod phone;
pub mod policy;
pub mod server;

pub use args::CliArgs;
pub use policy::{AsymmetricOutputManager, OutputConfig, OutputRole, SurfaceDisplayMode};
pub use server::PenumbraCompositor;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phone_dsi_policy_invariants() {
        let mut manager = AsymmetricOutputManager::new();
        let config = manager.output_config(OutputRole::PhoneDsi).unwrap();

        assert_eq!(config.width, 412);
        assert_eq!(config.height, 915);
        assert!(!config.allows_floating);
        assert!(config.enforces_gestures);

        let s1 = manager.register_surface(1, "phone.app", "Phone App");
        assert_eq!(s1.assigned_output, OutputRole::PhoneDsi);
        assert_eq!(s1.display_mode, SurfaceDisplayMode::FullscreenExclusive);
    }

    #[test]
    fn test_convergence_hotplug_and_migration() {
        let mut manager = AsymmetricOutputManager::new();
        manager.register_surface(1, "phone.app", "Phone App");

        // Hotplug external HDMI
        manager.hotplug_external_display();
        assert!(manager.is_convergence_active());

        // Second surface should route to HDMI as floating window
        let s2 = manager.register_surface(2, "desktop.app", "Desktop App");
        assert_eq!(s2.assigned_output, OutputRole::ExternalDesktopHdmi);
        assert!(matches!(
            s2.display_mode,
            SurfaceDisplayMode::FloatingWindow { .. }
        ));

        // Unplug external display
        manager.unplug_external_display();
        assert!(!manager.is_convergence_active());

        let s2_migrated = manager
            .surfaces_for_output(OutputRole::PhoneDsi)
            .into_iter()
            .find(|s| s.id == 2)
            .unwrap();
        assert_eq!(
            s2_migrated.display_mode,
            SurfaceDisplayMode::FullscreenExclusive
        );
    }
}
