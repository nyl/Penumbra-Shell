//! # Physical Hardware Keyboard Detection Engine
//!
//! Scans Linux input devices (`/proc/bus/input/devices`) to reliably distinguish
//! genuine physical typing keyboards from virtual uinput devices, volume buttons,
//! power keys, and lid switches.
//!
//! ## Detection Criteria
//! 1. **Non-Virtual Bus:** Bustype is NOT `0006` (BUS_VIRTUAL) and device name does not
//!    contain `"uinput"`, `"virtual"`, or `"penumbra"`.
//! 2. **Keyboard Handler & Key Event Mask:** Declares `kbd` in `H: Handlers` and bit 1 (`EV_KEY`) in `B: EV`.
//! 3. **Alphanumeric Key Verification:** Inspects the `B: KEY` bitmask to ensure standard typing
//!    keys (`KEY_A`, `KEY_ENTER`, or `KEY_SPACE`) are physically registered. This eliminates false
//!    positives from hardware buttons (power, volume up/down, switches).

use std::fs;
use tracing::debug;

/// Telemetry describing a connected physical keyboard device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalKeyboardInfo {
    /// Human-readable device name reported by the kernel or USB/Bluetooth descriptor.
    pub name: String,
    /// Hardware bus identifier (e.g. `0003` for USB, `0005` for Bluetooth, `0011` for PS/2).
    pub bus: String,
    /// Sysfs path relative to `/sys`.
    pub sysfs: String,
}

/// Scans `/proc/bus/input/devices` and returns all connected physical keyboards.
pub fn scan_physical_keyboards() -> Vec<PhysicalKeyboardInfo> {
    let content = match fs::read_to_string("/proc/bus/input/devices") {
        Ok(c) => c,
        Err(err) => {
            debug!("Unable to read /proc/bus/input/devices: {:?}", err);
            return Vec::new();
        }
    };

    let mut keyboards = Vec::new();

    // Each device entry in /proc/bus/input/devices is separated by blank lines
    for block in content.split("\n\n") {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut bus = String::new();
        let mut name = String::new();
        let mut phys = String::new();
        let mut sysfs = String::new();
        let mut handlers = String::new();
        let mut ev_mask = 0u64;
        let mut key_words: Vec<u64> = Vec::new();

        for line in trimmed.lines() {
            if let Some(rest) = line.strip_prefix("I: ") {
                for token in rest.split_whitespace() {
                    if let Some(val) = token.strip_prefix("Bus=") {
                        bus = val.to_string();
                    }
                }
            } else if let Some(rest) = line.strip_prefix("N: Name=") {
                name = rest.trim_matches('"').to_string();
            } else if let Some(rest) = line.strip_prefix("P: Phys=") {
                phys = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("S: Sysfs=") {
                sysfs = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
                handlers = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("B: EV=") {
                ev_mask = u64::from_str_radix(rest.trim(), 16).unwrap_or(0);
            } else if let Some(rest) = line.strip_prefix("B: KEY=") {
                // Key words are hexadecimal numbers ordered from highest word down to word 0
                key_words = rest
                    .split_whitespace()
                    .filter_map(|w| u64::from_str_radix(w, 16).ok())
                    .collect();
            }
        }

        // Criterion 1: Exclude virtual / uinput devices
        if bus == "0006" {
            continue;
        }
        let name_lower = name.to_lowercase();
        if name_lower.contains("uinput")
            || name_lower.contains("penumbra")
            || name_lower.contains("virtual")
            || name_lower.contains("ydotool")
        {
            continue;
        }
        if phys.to_lowercase().contains("uinput") {
            continue;
        }

        // Criterion 2: Requires kbd handler and EV_KEY bit (bit 1: 0x2)
        if !handlers.split_whitespace().any(|h| h == "kbd") {
            continue;
        }
        if (ev_mask & 0x2) == 0 {
            continue;
        }

        // Criterion 3: Must have alphanumeric keys.
        // The last element of key_words is word 0 (covering keys 0..=63).
        // KEY_ENTER = 28, KEY_A = 30, KEY_SPACE = 57.
        if let Some(&word0) = key_words.last() {
            let has_enter = (word0 & (1u64 << 28)) != 0;
            let has_a = (word0 & (1u64 << 30)) != 0;
            let has_space = (word0 & (1u64 << 57)) != 0;

            if has_enter || (has_a && has_space) {
                keyboards.push(PhysicalKeyboardInfo { name, bus, sysfs });
            }
        }
    }

    keyboards
}

/// Returns the primary active physical keyboard name if connected, or `None`.
pub fn detect_active_physical_keyboard() -> Option<String> {
    scan_physical_keyboards().first().map(|k| k.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_physical_keyboards_does_not_panic() {
        let keyboards = scan_physical_keyboards();
        // Should execute without errors regardless of host hardware configuration
        for kb in keyboards {
            assert!(!kb.name.is_empty());
            assert_ne!(kb.bus, "0006");
        }
    }
}
