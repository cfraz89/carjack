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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portal: Option<StoredPortal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBluetooth {
    /// `"auto"`, a MAC, or an advertised name - the same vocabulary as `[bluetooth] device`.
    pub device: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPortal {
    /// WPA2 key for the setup AP. Absent until someone changes it from [`DEFAULT_AP_PASSPHRASE`].
    pub ap_passphrase: String,
}

/// What the setup AP ships with, so a board out of the box can be joined without being told
/// anything. The portal refuses to do anything else until it has been changed.
///
/// Not the plain word "carjack": WPA2 keys are 8-63 characters and that is 7.
pub const DEFAULT_AP_PASSPHRASE: &str = "carjack1";

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

    /// The setup AP's WPA2 key: whatever was set, else the shipped default.
    pub fn ap_passphrase(&self) -> &str {
        self.portal
            .as_ref()
            .map(|p| p.ap_passphrase.as_str())
            .filter(|p| !p.is_empty())
            .unwrap_or(DEFAULT_AP_PASSPHRASE)
    }

    /// Whether the AP is still on the key it shipped with, which the portal makes you fix before
    /// it will do anything else.
    pub fn ap_passphrase_is_default(&self) -> bool {
        self.ap_passphrase() == DEFAULT_AP_PASSPHRASE
    }

    pub fn set_ap_passphrase(passphrase: &str) -> Result<()> {
        let passphrase = passphrase.trim();
        if passphrase.len() < 8 || passphrase.len() > 63 {
            return Err(eyre!("a WPA2 password must be 8 to 63 characters"));
        }
        if passphrase == DEFAULT_AP_PASSPHRASE {
            return Err(eyre!("that is the default password - pick a different one"));
        }
        let mut settings = Self::load();
        settings.portal = Some(StoredPortal {
            ap_passphrase: passphrase.to_owned(),
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
            portal: None,
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

    #[test]
    fn an_unset_passphrase_reads_as_the_shipped_default() {
        let fresh = StoredSettings::default();
        assert_eq!(fresh.ap_passphrase(), DEFAULT_AP_PASSPHRASE);
        assert!(fresh.ap_passphrase_is_default());

        // WPA2's own minimum, which is why the default is not the plain word "carjack".
        assert!(DEFAULT_AP_PASSPHRASE.len() >= 8);

        let changed = StoredSettings {
            bluetooth: None,
            portal: Some(StoredPortal {
                ap_passphrase: "a better secret".into(),
            }),
        };
        assert_eq!(changed.ap_passphrase(), "a better secret");
        assert!(!changed.ap_passphrase_is_default());
    }

    /// An empty string in the file is a half-written save, not a chosen password.
    #[test]
    fn an_empty_stored_passphrase_falls_back_to_the_default() {
        let blank = StoredSettings {
            bluetooth: None,
            portal: Some(StoredPortal {
                ap_passphrase: String::new(),
            }),
        };
        assert_eq!(blank.ap_passphrase(), DEFAULT_AP_PASSPHRASE);
        assert!(blank.ap_passphrase_is_default());
    }
}
