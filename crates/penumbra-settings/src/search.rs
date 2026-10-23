//! # Settings Search Index & Query Engine
//!
//! Provides fast, in-memory, zero-dependency search filtering for Penumbra Settings,
//! adhering to Android 17 / Pixel Material You search behavior.
//!
//! Features:
//! - Multi-token case-insensitive matching across titles, breadcrumbs, and keywords.
//! - Automatic category grouping and header computation.
//! - Deep-link target resolution to dedicated subpages.

use std::rc::Rc;
use slint::{ModelRc, VecModel};
use crate::SearchResultData;

/// Static definition of an indexable settings entry.
#[derive(Debug, Clone)]
pub struct SearchEntry {
    /// Category header displayed in accent color (#a8c7fa).
    pub category: &'static str,
    /// Primary title of the setting item (e.g. "Bluetooth").
    pub title: &'static str,
    /// Hierarchical breadcrumb path (e.g. "Connected devices > Bluetooth").
    pub breadcrumb: &'static str,
    /// Target subpage route identifier ("wifi", "bluetooth", "display", etc.).
    pub target_page: &'static str,
    /// Searchable keywords, synonyms, and localization tokens.
    pub keywords: &'static str,
}

/// Global settings search catalog indexed for fast retrieval across all UI sections.
pub static SEARCH_CATALOG: &[SearchEntry] = &[
    // =========================================================================
    // Network & internet
    // =========================================================================
    SearchEntry {
        category: "Network & internet",
        title: "Wi-Fi",
        breadcrumb: "Network & internet > Wi-Fi",
        target_page: "wifi",
        keywords: "wifi wi-fi internet wireless network ssid wlan retea hotspot scan connect wireless radio",
    },
    SearchEntry {
        category: "Network & internet",
        title: "Available networks",
        breadcrumb: "Network & internet > Available networks",
        target_page: "wifi",
        keywords: "wifi networks scan ap access points signal ssid security wpa wpa2 wpa3",
    },
    SearchEntry {
        category: "Network & internet",
        title: "Scan for networks",
        breadcrumb: "Network & internet > Network preferences",
        target_page: "wifi",
        keywords: "scan beacon discovery refresh search wireless networks",
    },
    SearchEntry {
        category: "Network & internet",
        title: "Wi-Fi privacy & MAC",
        breadcrumb: "Network & internet > Wi-Fi preferences > Privacy",
        target_page: "wifi",
        keywords: "mac address randomize hardware privacy security anonymize",
    },
    SearchEntry {
        category: "Network & internet",
        title: "Hotspot & tethering",
        breadcrumb: "Network & internet > Hotspot",
        target_page: "wifi",
        keywords: "hotspot tethering portable share internet usb bluetooth",
    },
    SearchEntry {
        category: "Network & internet",
        title: "Airplane mode",
        breadcrumb: "Network & internet > Airplane mode",
        target_page: "wifi",
        keywords: "airplane flight mode offline disconnect radios",
    },

    // =========================================================================
    // Connected devices (Bluetooth)
    // =========================================================================
    SearchEntry {
        category: "Bluetooth",
        title: "Bluetooth",
        breadcrumb: "Connected devices > Connection preferences > Bluetooth",
        target_page: "bluetooth",
        keywords: "bluetooth blu bt pair devices connect wireless audio cast accessories",
    },
    SearchEntry {
        category: "Bluetooth",
        title: "Pair new device",
        breadcrumb: "Connected devices > Pair new device",
        target_page: "bluetooth",
        keywords: "pair scan discovery bluetooth blu accessories headphones earbuds speaker mouse keyboard",
    },
    SearchEntry {
        category: "Bluetooth",
        title: "Bluetooth audio codec",
        breadcrumb: "Connected devices > Audio codecs",
        target_page: "bluetooth",
        keywords: "bluetooth blu codec audio ldac aptx aac sbc quality bitrate latency sound",
    },
    SearchEntry {
        category: "Developer options",
        title: "Bluetooth HCI snoop log",
        breadcrumb: "Developer options > Bluetooth",
        target_page: "bluetooth",
        keywords: "bluetooth blu log debug snoop packet hci capture developer options",
    },
    SearchEntry {
        category: "Developer options",
        title: "Bluetooth AVRCP Version",
        breadcrumb: "Developer options > Bluetooth",
        target_page: "bluetooth",
        keywords: "bluetooth blu avrcp version profile media controls playback metadata",
    },
    SearchEntry {
        category: "Connected devices",
        title: "Cast & screen mirror",
        breadcrumb: "Connected devices > Connection preferences > Cast",
        target_page: "bluetooth",
        keywords: "cast screen display mirror wireless external tv projector",
    },

    // =========================================================================
    // Apps & Notifications
    // =========================================================================
    SearchEntry {
        category: "Apps",
        title: "Default apps",
        breadcrumb: "Apps > Default apps",
        target_page: "about",
        keywords: "apps default applications browser phone assistant home launcher",
    },
    SearchEntry {
        category: "Apps",
        title: "All installed apps",
        breadcrumb: "Apps > All apps",
        target_page: "about",
        keywords: "apps installed software packages list storage usage",
    },
    SearchEntry {
        category: "Notifications",
        title: "App notifications",
        breadcrumb: "Notifications > App notifications",
        target_page: "sound",
        keywords: "notifications alerts badges banners priority mute silence",
    },
    SearchEntry {
        category: "Notifications",
        title: "Notification history",
        breadcrumb: "Notifications > Notification history",
        target_page: "sound",
        keywords: "notification history log past missed alerts conversations",
    },

    // =========================================================================
    // Display
    // =========================================================================
    SearchEntry {
        category: "Display",
        title: "Brightness level",
        breadcrumb: "Display > Brightness",
        target_page: "display",
        keywords: "brightness backlight screen light luminozitate display level slider",
    },
    SearchEntry {
        category: "Display",
        title: "Adaptive brightness",
        breadcrumb: "Display > Adaptive brightness",
        target_page: "display",
        keywords: "adaptive auto brightness ambient sensor light display automatic lux",
    },
    SearchEntry {
        category: "Display",
        title: "Dark theme",
        breadcrumb: "Display > Dark theme",
        target_page: "display",
        keywords: "dark theme night mode oled pitch black display style tema intunecata colors",
    },
    SearchEntry {
        category: "Display",
        title: "Screen timeout",
        breadcrumb: "Display > Screen timeout",
        target_page: "display",
        keywords: "screen timeout sleep lock display inactivity minutes seconds screen off",
    },
    SearchEntry {
        category: "Display",
        title: "Wallpaper & style",
        breadcrumb: "Display > Wallpaper & style",
        target_page: "display",
        keywords: "wallpaper style material you accent colors themed icons palette personalization",
    },
    SearchEntry {
        category: "Display",
        title: "Font size & display scale",
        breadcrumb: "Display > Font size",
        target_page: "display",
        keywords: "font size text scale zoom dpi accessibility display",
    },

    // =========================================================================
    // Sound & vibration
    // =========================================================================
    SearchEntry {
        category: "Sound & vibration",
        title: "Media volume",
        breadcrumb: "Sound & vibration > Media volume",
        target_page: "sound",
        keywords: "sound volume media music audio playback speaker sunet video games",
    },
    SearchEntry {
        category: "Sound & vibration",
        title: "Call volume",
        breadcrumb: "Sound & vibration > Call volume",
        target_page: "sound",
        keywords: "sound call voice phone volume speaker audio apel earpiece",
    },
    SearchEntry {
        category: "Sound & vibration",
        title: "Ring & notification volume",
        breadcrumb: "Sound & vibration > Ring volume",
        target_page: "sound",
        keywords: "ring ringer notification alarm sound alert volume audio sonerie ringtone",
    },
    SearchEntry {
        category: "Sound & vibration",
        title: "Do Not Disturb",
        breadcrumb: "Sound & vibration > Do Not Disturb",
        target_page: "sound",
        keywords: "dnd do not disturb silence mute quiet bedtime modes nu deranja schedules",
    },
    SearchEntry {
        category: "Sound & vibration",
        title: "Vibration & haptics",
        breadcrumb: "Sound & vibration > Vibration & haptics",
        target_page: "sound",
        keywords: "vibration haptics tactile feedback buzz ring call touch",
    },

    // =========================================================================
    // Battery
    // =========================================================================
    SearchEntry {
        category: "Battery",
        title: "Battery level & status",
        breadcrumb: "Battery > Battery usage",
        target_page: "battery",
        keywords: "battery power charge level percentage usage health drain baterie mah remaining",
    },
    SearchEntry {
        category: "Battery",
        title: "Battery Saver",
        breadcrumb: "Battery > Battery Saver",
        target_page: "battery",
        keywords: "battery saver low power economizer performance throttle economisire energy",
    },
    SearchEntry {
        category: "Battery",
        title: "Battery percentage",
        breadcrumb: "Battery > Battery percentage",
        target_page: "battery",
        keywords: "battery percentage status bar icon show percent charge",
    },

    // =========================================================================
    // Storage
    // =========================================================================
    SearchEntry {
        category: "Storage",
        title: "Storage & free space",
        breadcrumb: "Storage > Internal shared storage",
        target_page: "battery",
        keywords: "storage internal shared space disk memory gb used free clean stocare",
    },
    SearchEntry {
        category: "Storage",
        title: "System memory & RAM",
        breadcrumb: "Storage > System memory",
        target_page: "about",
        keywords: "memory ram swap system cache memory usage",
    },

    // =========================================================================
    // About phone
    // =========================================================================
    SearchEntry {
        category: "About phone",
        title: "Device name",
        breadcrumb: "About phone > Device name",
        target_page: "about",
        keywords: "device name penumbra phone model hostname despre telefon model identifier",
    },
    SearchEntry {
        category: "About phone",
        title: "OS Version",
        breadcrumb: "About phone > Software information > OS Version",
        target_page: "about",
        keywords: "os version penumbra linux 6.6 android 17 style build software version release",
    },
    SearchEntry {
        category: "About phone",
        title: "Linux kernel version",
        breadcrumb: "About phone > Software information > Kernel",
        target_page: "about",
        keywords: "linux kernel release architecture x86_64 aarch64 build date",
    },
    SearchEntry {
        category: "About phone",
        title: "Compositor & Wayland",
        breadcrumb: "About phone > Software information > Compositor",
        target_page: "about",
        keywords: "compositor smithay wayland drm egl engine window display server desktop",
    },
    SearchEntry {
        category: "About phone",
        title: "UI Framework",
        breadcrumb: "About phone > Software information > UI Framework",
        target_page: "about",
        keywords: "slint rust ui framework declarative reactive rendering engine",
    },
    SearchEntry {
        category: "About phone",
        title: "Bluetooth address",
        breadcrumb: "About phone > Hardware specifications > Bluetooth address",
        target_page: "about",
        keywords: "bluetooth blu mac address hardware adapter adresa bdaddr",
    },
    SearchEntry {
        category: "About phone",
        title: "Screen Geometry",
        breadcrumb: "About phone > Hardware specifications > Screen Geometry",
        target_page: "about",
        keywords: "screen resolution geometry 412x915 display phone model aspect ratio",
    },
    SearchEntry {
        category: "About phone",
        title: "Open Source Licenses",
        breadcrumb: "About phone > Legal information > Open Source Licenses",
        target_page: "about",
        keywords: "license open source apache mit copyright legal notices",
    },
];

/// Curated suggestions across all categories shown before the user types.
pub static SUGGESTED_ENTRIES: &[SearchEntry] = &[
    SearchEntry {
        category: "Suggested",
        title: "Wi-Fi",
        breadcrumb: "Network & internet > Wi-Fi",
        target_page: "wifi",
        keywords: "",
    },
    SearchEntry {
        category: "Suggested",
        title: "Bluetooth",
        breadcrumb: "Connected devices > Bluetooth",
        target_page: "bluetooth",
        keywords: "",
    },
    SearchEntry {
        category: "Suggested",
        title: "Dark theme",
        breadcrumb: "Display > Dark theme",
        target_page: "display",
        keywords: "",
    },
    SearchEntry {
        category: "Suggested",
        title: "Brightness level",
        breadcrumb: "Display > Brightness",
        target_page: "display",
        keywords: "",
    },
    SearchEntry {
        category: "Suggested",
        title: "Sound & vibration",
        breadcrumb: "Sound & vibration > Volume",
        target_page: "sound",
        keywords: "",
    },
    SearchEntry {
        category: "Suggested",
        title: "Battery Saver",
        breadcrumb: "Battery > Battery Saver",
        target_page: "battery",
        keywords: "",
    },
    SearchEntry {
        category: "Suggested",
        title: "About phone",
        breadcrumb: "About phone > Software information",
        target_page: "about",
        keywords: "",
    },
];

/// Filters the search catalog using the provided query string and constructs
/// Slint-compatible `SearchResultData` models with category headers.
pub fn filter_search(query: &str) -> ModelRc<SearchResultData> {
    let clean_query = query.trim().to_lowercase();
    let tokens: Vec<&str> = clean_query.split_whitespace().collect();

    let matched_entries: Vec<&SearchEntry> = if tokens.is_empty() {
        // Return cross-category suggested items when query is empty
        SUGGESTED_ENTRIES.iter().collect()
    } else {
        SEARCH_CATALOG
            .iter()
            .filter(|entry| {
                let text_to_search = format!(
                    "{} {} {} {}",
                    entry.title.to_lowercase(),
                    entry.category.to_lowercase(),
                    entry.breadcrumb.to_lowercase(),
                    entry.keywords.to_lowercase()
                );
                // All tokens must match
                tokens.iter().all(|&t| text_to_search.contains(t))
            })
            .collect()
    };

    let mut results = Vec::new();
    let mut prev_category = "";

    for entry in matched_entries {
        let show_category_header = entry.category != prev_category;
        prev_category = entry.category;

        results.push(SearchResultData {
            category: entry.category.into(),
            show_category_header,
            title: entry.title.into(),
            breadcrumb: entry.breadcrumb.into(),
            target_page: entry.target_page.into(),
        });
    }

    ModelRc::from(Rc::new(VecModel::from(results)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    #[test]
    fn test_empty_query_shows_cross_category_suggestions() {
        let results = filter_search("");
        assert!(results.row_count() >= 6);
        let first = results.row_data(0).unwrap();
        assert_eq!(first.category, "Suggested");
    }

    #[test]
    fn test_search_blu_matches_pixel_screenshot() {
        let results = filter_search("blu");
        assert!(results.row_count() >= 4);

        let titles: Vec<String> = (0..results.row_count())
            .map(|i| results.row_data(i).unwrap().title.to_string())
            .collect();

        assert!(titles.contains(&"Bluetooth".to_string()));
        assert!(titles.contains(&"Bluetooth audio codec".to_string()));
        assert!(titles.contains(&"Bluetooth address".to_string()));
    }

    #[test]
    fn test_search_wifi_filtering() {
        let results = filter_search("wifi");
        assert!(results.row_count() >= 2);
        let first = results.row_data(0).unwrap();
        assert_eq!(first.category, "Network & internet");
        assert_eq!(first.target_page, "wifi");
    }

    #[test]
    fn test_search_battery() {
        let results = filter_search("battery");
        assert!(results.row_count() >= 2);
    }

    #[test]
    fn test_category_headers_computed_correctly() {
        let results = filter_search("blu");
        let first = results.row_data(0).unwrap();
        assert!(first.show_category_header);
    }
}
