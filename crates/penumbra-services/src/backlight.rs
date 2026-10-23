//! Backlight display brightness control via sysfs (`/sys/class/backlight`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_BACKLIGHT_DIR: &str = "/sys/class/backlight";

fn read_first_line(path: &Path) -> io::Result<String> {
    let content = fs::read_to_string(path)?;
    Ok(content.trim().to_string())
}

pub fn discover_backlight_dir() -> io::Result<PathBuf> {
    let dir = Path::new(DEFAULT_BACKLIGHT_DIR);
    if !dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist", DEFAULT_BACKLIGHT_DIR),
        ));
    }

    let mut entries = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect::<Vec<_>>();
    entries.sort();
    entries.into_iter().next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no backlight device found under {}", DEFAULT_BACKLIGHT_DIR),
        )
    })
}

pub fn read_backlight_ratio() -> io::Result<f64> {
    let dir = discover_backlight_dir()?;
    let brightness_path = dir.join("brightness");
    let max_path = dir.join("max_brightness");
    let max_brightness = read_first_line(&max_path)?.parse::<u32>().unwrap_or(255);
    let cur_brightness = read_first_line(&brightness_path)?.parse::<u32>().unwrap_or(max_brightness);
    if max_brightness == 0 {
        return Ok(1.0);
    }
    Ok((cur_brightness as f64 / max_brightness as f64).clamp(0.0, 1.0))
}

pub fn apply_backlight_ratio(value: f64) -> io::Result<()> {
    let dir = discover_backlight_dir()?;
    let brightness_path = dir.join("brightness");
    let max_path = dir.join("max_brightness");
    let max_brightness = read_first_line(&max_path)?.parse::<u32>().unwrap_or(255);
    let ratio = value.clamp(0.0, 1.0);
    let raw = (ratio * max_brightness as f64).round() as u32;
    fs::write(brightness_path, raw.to_string())?;
    Ok(())
}
