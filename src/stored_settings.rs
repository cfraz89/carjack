//! Settings the config portal writes, kept apart from the hand-written `config.toml`.
//!
//! Two files rather than one because a hand-edited config carries comments and ordering that a
//! serializer would throw away on the first save. This one is carjack's to rewrite, lives in the
//! state dir, and is layered *over* `config.toml` by [`crate::config::Config::load`] - so the
//! portal's choice wins, and the file someone wrote by hand is never touched.

use std::path::PathBuf;

use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "settings.toml";

/// Every section is optional, and an absent one leaves `config.toml` in charge of it. Writing
/// empty strings instead would silently override a hand-written setting with nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluetooth: Option<StoredBluetooth>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBluetooth {
    /// `"auto"`, a MAC, or an advertised name - the same vocabulary as `[bluetooth] device`.
    pub device: String,
}

impl StoredSettings {
    pub fn path() -> Option<PathBuf> {
        crate::paths::state_file(FILE).ok()
    }

    /// All-absent if there is no file yet, or if it cannot be read or parsed - the portal is
    /// meant to be usable precisely when things are unconfigured.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), "Ignoring unparseable settings: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path().ok_or_else(|| eyre!("no state directory to save settings in"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        tracing::info!(path = %path.display(), "Saved settings");
        Ok(())
    }

    /// Read, change the head unit, write back - so saving one setting never drops another.
    pub fn set_bluetooth_device(device: &str) -> Result<()> {
        let mut settings = Self::load();
        settings.bluetooth = Some(StoredBluetooth {
            device: device.trim().to_owned(),
        });
        settings.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_section_serializes_to_nothing() {
        // An empty `[bluetooth] device = ""` here would read back as "no head unit configured"
        // and override whatever config.toml says.
        assert_eq!(toml::to_string(&StoredSettings::default()).unwrap(), "");
    }

    #[test]
    fn round_trips_a_head_unit() {
        let settings = StoredSettings {
            bluetooth: Some(StoredBluetooth {
                device: "auto".into(),
            }),
        };
        let text = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(toml::from_str::<StoredSettings>(&text).unwrap(), settings);
        // And it is shaped like the config file it layers over.
        assert!(text.contains("[bluetooth]"));
        assert!(text.contains("device = \"auto\""));
    }

    #[test]
    fn an_empty_file_is_all_absent() {
        assert_eq!(
            toml::from_str::<StoredSettings>("").unwrap(),
            StoredSettings::default()
        );
    }
}
