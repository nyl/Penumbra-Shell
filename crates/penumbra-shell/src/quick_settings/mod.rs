//! Data-driven Quick Settings tile engine with persistent configuration.

pub mod tile;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
pub use tile::{TileDefinition, TileState};

/// Persistent order / visibility of tiles (`~/.config/penumbra/quicksettings.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickSettingsConfig {
    pub active_tiles: Vec<String>,
    pub available_tiles: Vec<String>,
}

impl Default for QuickSettingsConfig {
    fn default() -> Self {
        let active = tile::default_active_ids();
        let available = tile::registry()
            .into_iter()
            .map(|t| t.id)
            .filter(|id| !active.contains(id))
            .collect();
        Self {
            active_tiles: active,
            available_tiles: available,
        }
    }
}

pub fn config_path() -> PathBuf {
    crate::config::get_layout_config_path().with_file_name("quicksettings.json")
}

impl QuickSettingsConfig {
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Self>(&t).ok())
            .map(Self::sanitized)
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        crate::ipc::write_atomic(path, &json)
    }

    /// Drops unknown / duplicate ids and adds any registry tile that is missing from both lists.
    fn sanitized(mut self) -> Self {
        let known: Vec<String> = tile::registry().into_iter().map(|t| t.id).collect();
        let mut seen = Vec::new();
        let mut keep = |ids: &mut Vec<String>| {
            ids.retain(|id| {
                known.contains(id) && !seen.contains(id) && {
                    seen.push(id.clone());
                    true
                }
            });
        };
        keep(&mut self.active_tiles);
        keep(&mut self.available_tiles);
        for id in known {
            if !seen.contains(&id) {
                self.available_tiles.push(id);
            }
        }
        self
    }

    /// Moves the active tile at `from` to position `to` (indices clamped).
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.active_tiles.len() {
            return;
        }
        let item = self.active_tiles.remove(from);
        self.active_tiles
            .insert(to.min(self.active_tiles.len()), item);
    }

    /// Moves a tile from the active list to the available pool.
    pub fn remove(&mut self, id: &str) {
        if let Some(i) = self.active_tiles.iter().position(|t| t == id) {
            let t = self.active_tiles.remove(i);
            self.available_tiles.push(t);
        }
    }

    /// Moves a tile from the available pool to the end of the active list.
    pub fn add(&mut self, id: &str) {
        if let Some(i) = self.available_tiles.iter().position(|t| t == id) {
            let t = self.available_tiles.remove(i);
            self.active_tiles.push(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_partitions_registry() {
        let c = QuickSettingsConfig::default();
        assert_eq!(
            c.active_tiles.len() + c.available_tiles.len(),
            tile::registry().len()
        );
        assert!(c.available_tiles.contains(&"airplane".to_string()));
    }

    #[test]
    fn edit_roundtrip_persists() {
        let path = std::env::temp_dir().join(format!("penumbra_qs_{}.json", std::process::id()));
        let mut c = QuickSettingsConfig::default();
        c.reorder(0, 3);
        c.remove("dnd");
        c.add("airplane");
        c.save_to(&path).unwrap();
        let loaded = QuickSettingsConfig::load_from(&path);
        assert_eq!(loaded, c);
        assert_eq!(loaded.active_tiles.last().unwrap(), "airplane");
        assert!(!loaded.active_tiles.contains(&"dnd".to_string()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sanitize_fixes_garbage() {
        let c = QuickSettingsConfig {
            active_tiles: vec!["internet".into(), "internet".into(), "bogus".into()],
            available_tiles: vec![],
        }
        .sanitized();
        assert_eq!(c.active_tiles, vec!["internet".to_string()]);
        assert_eq!(
            c.active_tiles.len() + c.available_tiles.len(),
            tile::registry().len()
        );
    }

    #[test]
    fn toggle_updates_subtitle() {
        let mut t = tile::registry().remove(0);
        t.toggle();
        assert_eq!(t.state, TileState::Inactive);
        assert_eq!(t.subtitle, "Off");
    }
}
