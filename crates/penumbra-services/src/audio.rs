//! PulseAudio / ALSA audio volume integration via `pactl`.

use std::process::Command;

/// Reads the current default sink volume as a ratio in `0.0..=1.0`.
pub fn read_volume_ratio() -> f64 {
    let output = Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output();

    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(pct_idx) = text.find('%') {
            let prefix = &text[..pct_idx];
            let start = prefix
                .rfind(|c: char| !c.is_ascii_digit())
                .map(|i| i + 1)
                .unwrap_or(0);
            if let Ok(pct) = prefix[start..].trim().parse::<f64>() {
                return (pct / 100.0).clamp(0.0, 1.0);
            }
        }
    }
    0.65
}

/// Sets the volume of the default sink to `ratio` in `0.0..=1.0`.
pub fn apply_volume_ratio(ratio: f64) {
    let pct = (ratio.clamp(0.0, 1.0) * 100.0).round() as u32;
    let _ = Command::new("pactl")
        .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{pct}%")])
        .spawn();
}

/// Changes the default sink volume by relative step (e.g. `+5` or `-5`).
pub fn adjust_volume_step(step_pct: i32) {
    let arg = if step_pct >= 0 {
        format!("+{step_pct}%")
    } else {
        format!("{step_pct}%")
    };
    let _ = Command::new("pactl")
        .args(["set-sink-volume", "@DEFAULT_SINK@", &arg])
        .spawn();
}
