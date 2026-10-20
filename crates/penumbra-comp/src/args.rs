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
    #[arg(short, long, default_value_t = true)]
    pub nested: bool,

    /// Enable convergence mode with simulated secondary external HDMI monitor (1280x720).
    #[arg(short, long, default_value_t = false)]
    pub convergence: bool,

    /// Wayland socket name for clients to connect.
    #[arg(short, long, default_value = "wayland-penumbra-0")]
    pub socket: String,

    /// Headless execution mode for CI and automated tests.
    #[arg(long, default_value_t = false)]
    pub headless: bool,
}
