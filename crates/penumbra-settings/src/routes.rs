//! # Settings Route & Page Navigation Architecture
//!
//! Provides the primary navigation state machine and URL/CLI routing enum for `penumbra-settings`.
//!
//! ## Architectural Role
//! `penumbra-settings` adheres to a single-window, multi-page deep-linking design.
//! Pages can be targeted directly from:
//! 1. Command-line flags (e.g. `penumbra-settings --page wifi`).
//! 2. Inter-process D-Bus calls via `org.penumbra.Settings.OpenPage("bluetooth")`.
//! 3. System chrome long-press gestures (such as Quick Settings tiles).
//!
//! [`Page`] acts as the canonical representation for all subpage targets and implements
//! lenient string parsing with alias resolution to ensure compatibility with various caller formats.

use std::fmt;
use std::str::FromStr;

/// Supported subpages and navigation targets across the settings application.
///
/// Each variant represents a distinct functional section rendered as a dedicated
/// Slint page component with slide-in navigation transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Page {
    /// Root directory catalog containing top-level setting category cards.
    #[default]
    Main,
    /// Wireless local area network management, scanning, and connection setup.
    Wifi,
    /// Bluetooth adapter power toggle, active discovery, and paired device management.
    Bluetooth,
    /// Screen brightness sliders, auto-brightness, dark theme, and display metrics.
    Display,
    /// Battery health diagnostics, charge telemetry, and power saving modes.
    Battery,
    /// Volume channels (Media, Call, Ring) and Do Not Disturb controls.
    Sound,
    /// System details, hardware specifications, kernel release, and license info.
    About,
}

impl Page {
    /// Returns the canonical kebab-case string identifier exchanged with Slint UI properties.
    ///
    /// # Returns
    /// A static string slice matching the `active-page` property in `settings_window.slint`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Page::Main => "main",
            Page::Wifi => "wifi",
            Page::Bluetooth => "bluetooth",
            Page::Display => "display",
            Page::Battery => "battery",
            Page::Sound => "sound",
            Page::About => "about",
        }
    }

    /// User-facing display title formatted for top app bars and accessibility labels.
    ///
    /// # Returns
    /// An internationalized or localized static title string for the given page.
    #[allow(dead_code)]
    pub fn title(&self) -> &'static str {
        match self {
            Page::Main => "Settings",
            Page::Wifi => "Network & internet",
            Page::Bluetooth => "Connected devices",
            Page::Display => "Display",
            Page::Battery => "Battery",
            Page::Sound => "Sound & vibration",
            Page::About => "About phone",
        }
    }
}

impl fmt::Display for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Page {
    type Err = std::convert::Infallible;

    /// Parses an incoming string into a [`Page`] variant, accepting common aliases.
    ///
    /// Unknown or empty inputs safely fall back to [`Page::Main`].
    ///
    /// # Aliases
    /// * `"wifi"`, `"wi-fi"`, `"network"`, `"internet"`, `"wlan"` $\to$ [`Page::Wifi`]
    /// * `"bluetooth"`, `"bt"` $\to$ [`Page::Bluetooth`]
    /// * `"display"`, `"brightness"`, `"screen"` $\to$ [`Page::Display`]
    /// * `"battery"`, `"power"`, `"battery_saver"` $\to$ [`Page::Battery`]
    /// * `"sound"`, `"audio"`, `"volume"` $\to$ [`Page::Sound`]
    /// * `"about"`, `"system"`, `"device"`, `"info"` $\to$ [`Page::About`]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        let page = match normalized.as_str() {
            "wifi" | "wi-fi" | "network" | "internet" | "wlan" => Page::Wifi,
            "bluetooth" | "bt" => Page::Bluetooth,
            "display" | "brightness" | "screen" => Page::Display,
            "battery" | "power" | "battery_saver" => Page::Battery,
            "sound" | "audio" | "volume" => Page::Sound,
            "about" | "system" | "device" | "info" => Page::About,
            _ => Page::Main,
        };
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_routing_aliases() {
        assert_eq!(Page::from_str("wifi").unwrap(), Page::Wifi);
        assert_eq!(Page::from_str("internet").unwrap(), Page::Wifi);
        assert_eq!(Page::from_str("bluetooth").unwrap(), Page::Bluetooth);
        assert_eq!(Page::from_str("brightness").unwrap(), Page::Display);
        assert_eq!(Page::from_str("battery_saver").unwrap(), Page::Battery);
        assert_eq!(Page::from_str("main").unwrap(), Page::Main);
        assert_eq!(Page::from_str("unknown_foo").unwrap(), Page::Main);
    }
}
