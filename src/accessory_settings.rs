//! Per-accessory settings, keyed by the accessory's Bluetooth MAC. Hand-edited config under XDG config.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessorySettings {
    /// Force HE (802.11ax) off when associating with this accessory's WiFi - some head units disconnect
    /// otherwise. See `wifi_connect::WifiBackend::connect_to_accessory_wifi`.
    pub disable_he: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    accessory: HashMap<String, AccessorySettings>,
}

pub struct AccessoryStore {
    file: SettingsFile,
}

impl AccessoryStore {
    /// Load from `~/.config/carjack/accessories.toml` (all-empty if absent/unreadable).
    pub fn load() -> Self {
        let file = xdg::BaseDirectories::with_prefix("carjack")
            .find_config_file("accessories.toml")
            .and_then(|p| {
                config::Config::builder()
                    .add_source(config::File::from(p.as_path()))
                    .build()
                    .ok()?
                    .try_deserialize()
                    .ok()
            })
            .unwrap_or_default();
        AccessoryStore { file }
    }

    pub fn get(&self, mac: &str) -> AccessorySettings {
        self.file.accessory.get(mac).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_file_round_trips() {
        let mut file = SettingsFile::default();
        file.accessory.insert(
            "AA:BB:CC:DD:EE:FF".into(),
            AccessorySettings { disable_he: true },
        );
        let s = toml::to_string(&file).unwrap();
        let back: SettingsFile = toml::from_str(&s).unwrap();
        assert_eq!(
            back.accessory.get("AA:BB:CC:DD:EE:FF"),
            Some(&AccessorySettings { disable_he: true })
        );
    }

    #[test]
    fn empty_toml_is_default() {
        let back: SettingsFile = toml::from_str("").unwrap();
        assert!(back.accessory.is_empty());
    }

    #[test]
    fn unknown_mac_defaults_to_disable_he_false() {
        let store = AccessoryStore {
            file: SettingsFile::default(),
        };
        assert_eq!(store.get("AA:BB:CC:DD:EE:FF"), AccessorySettings::default());
    }
}
