use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_BACKLIGHT_DIR: &str = "/sys/class/backlight";

pub fn brightness_ratio_to_raw(value: f64, max_brightness: u32) -> u32 {
    let ratio = value.clamp(0.0, 1.0);
    ((ratio * max_brightness as f64).round()) as u32
}

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

pub fn apply_backlight_ratio(value: f64) -> io::Result<()> {
    let backlight_dir = discover_backlight_dir()?;
    let brightness_path = backlight_dir.join("brightness");
    let max_path = backlight_dir.join("max_brightness");
    let max_brightness = read_first_line(&max_path)?
        .parse::<u32>()
        .unwrap_or(255);
    let raw = brightness_ratio_to_raw(value, max_brightness);
    fs::write(brightness_path, raw.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_map_clamps_and_scales() {
        assert_eq!(brightness_ratio_to_raw(0.0, 255), 0);
        assert_eq!(brightness_ratio_to_raw(1.0, 255), 255);
        assert_eq!(brightness_ratio_to_raw(0.5, 255), 128);
        assert_eq!(brightness_ratio_to_raw(1.5, 255), 255);
        assert_eq!(brightness_ratio_to_raw(-0.5, 255), 0);
    }
}
