//! Data model and built-in registry for Quick Settings tiles.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TileState {
    /// High-contrast accent color.
    Active,
    /// Dark surface container.
    Inactive,
    /// Dimmed / disabled.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileDefinition {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub icon_name: String,
    pub state: TileState,
    pub is_enabled: bool,
}

impl TileDefinition {
    fn new(id: &str, title: &str, icon: &str, on: &str, off: &str, active: bool) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: if active { on } else { off }.into(),
            icon_name: icon.into(),
            state: if active { TileState::Active } else { TileState::Inactive },
            is_enabled: true,
        }
    }

    /// Flips Active/Inactive and refreshes the subtitle. Unavailable tiles are untouched.
    pub fn toggle(&mut self) {
        let (on, off) = subtitles(&self.id);
        match self.state {
            TileState::Active => {
                self.state = TileState::Inactive;
                self.subtitle = off.into();
            }
            TileState::Inactive => {
                self.state = TileState::Active;
                self.subtitle = on.into();
            }
            TileState::Unavailable => {}
        }
    }
}

fn subtitles(id: &str) -> (&'static str, &'static str) {
    match id {
        "internet" => ("Connected", "Off"),
        "bluetooth" => ("On", "Off"),
        "flashlight" => ("On", "Off"),
        "dnd" => ("Priority only", "Off"),
        "rotation" => ("Auto-rotate", "Portrait"),
        "battery_saver" => ("On", "Off"),
        "airplane" => ("On", "Off"),
        "hotspot" => ("Sharing", "Off"),
        "night_light" => ("On", "Off"),
        "recorder" => ("Recording", "Tap to start"),
        _ => ("On", "Off"),
    }
}

/// Every tile the shell knows about, in default catalogue order.
pub fn registry() -> Vec<TileDefinition> {
    vec![
        TileDefinition::new("internet", "Internet", "wifi", "Connected", "Off", true),
        TileDefinition::new("bluetooth", "Bluetooth", "bluetooth", "On", "Off", true),
        TileDefinition::new("flashlight", "Flashlight", "flashlight", "On", "Off", false),
        TileDefinition::new("dnd", "Do Not Disturb", "dnd", "Priority only", "Off", false),
        TileDefinition::new("rotation", "Auto-rotate", "rotation", "Auto-rotate", "Portrait", true),
        TileDefinition::new("battery_saver", "Battery Saver", "battery", "On", "Off", false),
        TileDefinition::new("airplane", "Airplane mode", "airplane", "On", "Off", false),
        TileDefinition::new("hotspot", "Hotspot", "hotspot", "Sharing", "Off", false),
        TileDefinition::new("night_light", "Night Light", "night", "On", "Off", false),
        TileDefinition::new("recorder", "Screen recorder", "recorder", "Recording", "Tap to start", false),
    ]
}

pub fn default_active_ids() -> Vec<String> {
    ["internet", "bluetooth", "flashlight", "dnd", "rotation", "battery_saver"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
