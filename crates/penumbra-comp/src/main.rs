//! Penumbra Compositor binary entrypoint.

use clap::Parser;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use penumbra_comp::{CliArgs, PenumbraCompositor};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let args = CliArgs::parse();
    let mut compositor = PenumbraCompositor::new(args);
    compositor.run()?;

    Ok(())
}
