//! Per-accessory display cache. Remembers the resolution/refresh a head unit reported, keyed by the
//! accessory's Bluetooth MAC, so on the next connect we can size the compositor (and launch the desktop
//! app) up front - hiding a slow app's boot (e.g. Waydroid) behind the BT/WiFi/AirPlay setup
//! instead of waiting for AirPlay to report the display. Stored as JSON in the XDG cache dir; a
//! missing or corrupt file just means "no cache yet".

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A head unit's display parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayProps {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    /// MAC of the most recently connected accessory - used to pick a size at startup, before we know
    /// which accessory this run will connect to.
    #[serde(default)]
    last: Option<String>,
    #[serde(default)]
    displays: HashMap<String, DisplayProps>,
}

pub struct DisplayCache {
    path: Option<PathBuf>,
    file: CacheFile,
}

impl DisplayCache {
    /// Load the cache from `~/.cache/carjack/displays.json` (all-empty if absent/unreadable).
    pub fn load() -> Self {
        let path = xdg::BaseDirectories::with_prefix("carjack")
            .place_cache_file("displays.json")
            .ok();
        let file = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        DisplayCache { path, file }
    }

    /// Cached props for a specific accessory MAC, if known.
    pub fn get(&self, mac: &str) -> Option<DisplayProps> {
        self.file.displays.get(mac).copied()
    }

    /// Best guess for startup, before we know which accessory will connect: the most recently used accessory.
    pub fn last_props(&self) -> Option<DisplayProps> {
        self.file.last.as_ref().and_then(|m| self.get(m))
    }

    /// Record an accessory's display and mark it the most recent, persisting if anything changed.
    pub fn remember(&mut self, mac: &str, props: DisplayProps) {
        let unchanged = self.file.displays.get(mac) == Some(&props);
        let is_last = self.file.last.as_deref() == Some(mac);
        if unchanged && is_last {
            return;
        }
        self.file.displays.insert(mac.to_string(), props);
        self.file.last = Some(mac.to_string());
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        match serde_json::to_vec_pretty(&self.file) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(path, bytes) {
                    tracing::warn!("display cache: write {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("display cache: serialize: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_file_round_trips() {
        let mut file = CacheFile::default();
        file.displays.insert(
            "AA:BB:CC:DD:EE:FF".into(),
            DisplayProps {
                width: 1080,
                height: 600,
                fps: 30,
            },
        );
        file.last = Some("AA:BB:CC:DD:EE:FF".into());
        let json = serde_json::to_vec(&file).unwrap();
        let back: CacheFile = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.last.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(
            back.displays.get("AA:BB:CC:DD:EE:FF"),
            Some(&DisplayProps {
                width: 1080,
                height: 600,
                fps: 30
            })
        );
    }

    #[test]
    fn empty_json_is_default() {
        let back: CacheFile = serde_json::from_slice(b"{}").unwrap();
        assert!(back.last.is_none());
        assert!(back.displays.is_empty());
    }
}
