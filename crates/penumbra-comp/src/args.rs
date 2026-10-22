//! Command-line argument parsing for penumbra-comp.

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "penumbra-comp",
    about = "Smithay-based Mobile Wayland Compositor with Convergence and Output Policies",
    version = "0.1.0"
)]
pub struct CliArgs {
    /// Launch compositor inside a desktop window for nested simulation.
    /// Default to false so the native DRM phone session does not silently keep the
    /// desktop-window simulation enabled.
    #[arg(short, long, default_value_t = false)]
    pub nested: bool,

    /// Explicitly disable any nested desktop-window fallback.
    #[arg(long = "no-nested", default_value_t = false)]
    pub no_nested: bool,

    /// Enable convergence mode with simulated secondary external HDMI monitor (1280x720).
    #[arg(short, long, default_value_t = false)]
    pub convergence: bool,

    /// Wayland socket name for clients to connect.
    #[arg(short, long, default_value = "wayland-penumbra-0")]
    pub socket: String,

    /// Headless execution mode for CI and automated tests.
    #[arg(long, default_value_t = false)]
    pub headless: bool,

    /// Use the phone's native DRM/libseat backend instead of Winit.
    #[arg(long, default_value_t = false)]
    pub native: bool,
}

impl CliArgs {
    pub fn nested_enabled(&self) -> bool {
        self.nested && !self.no_nested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_mode_does_not_enable_nested_simulation() {
        let args = CliArgs::parse_from(["penumbra-comp", "--native"]);
        assert!(!args.nested_enabled());
        assert!(args.native);
    }

    #[test]
    fn explicit_no_nested_flag_disables_nested_mode() {
        let args = CliArgs::parse_from(["penumbra-comp", "--no-nested"]);
        assert!(!args.nested_enabled());
    }
}
