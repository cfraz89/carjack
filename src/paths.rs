//! Where carjack's files live.
//!
//! Under the systemd service the answer comes from systemd: `StateDirectory=`/`CacheDirectory=`
//! create the directories, hand them over in `$STATE_DIRECTORY`/`$CACHE_DIRECTORY`, and (under
//! `DynamicUser=`) keep them out of reach of a recycled UID. Left to XDG instead, all of it lands
//! under the service's `HOME` - `/var/lib/carjack/.cache/carjack/displays.json` and friends - which
//! buries a cache inside the state directory, where `systemctl clean` will never find it.
//!
//! Anywhere else - a developer run, a test - the XDG dirs under `$HOME` are still the right answer.

use std::io::Result;
use std::path::PathBuf;

/// System-wide config, and the only place the service ever finds one: its `HOME`
/// (`/var/lib/carjack`) has no `.config`, so the XDG lookup below always misses there.
const ETC_DIR: &str = "/etc/carjack";

/// A directory systemd set up for this unit. The variable is documented as a colon-separated list,
/// one entry per name given to the directive, so take the first rather than the whole string.
fn systemd_dir(var: &str) -> Option<PathBuf> {
    let value = std::env::var(var).ok()?;
    let first = value.split(':').next().filter(|s| !s.is_empty())?;
    Some(PathBuf::from(first))
}

/// A file carjack can afford to lose: `$CACHE_DIRECTORY`, else `~/.cache/carjack/`.
pub fn cache_file(name: &str) -> Result<PathBuf> {
    match systemd_dir("CACHE_DIRECTORY") {
        Some(dir) => Ok(dir.join(name)),
        None => xdg::BaseDirectories::with_prefix("carjack").place_cache_file(name),
    }
}

/// A config file someone hand-wrote: their own `~/.config/carjack/`, else `/etc/carjack/`. `None`
/// if neither has it - every setting has a default, so no file is not an error.
pub fn config_file(name: &str) -> Option<PathBuf> {
    xdg::BaseDirectories::with_prefix("carjack")
        .get_config_home()
        .map(|home| home.join(name))
        .filter(|p| p.exists())
        .or_else(|| {
            let etc = PathBuf::from(ETC_DIR).join(name);
            etc.exists().then_some(etc)
        })
}

/// A file that has to survive: `$STATE_DIRECTORY`, else the XDG data dir.
pub fn state_file(name: &str) -> Result<PathBuf> {
    match systemd_dir("STATE_DIRECTORY") {
        Some(dir) => Ok(dir.join(name)),
        None => xdg::BaseDirectories::with_prefix("carjack").place_data_file(name),
    }
}

#[cfg(test)]
mod tests {
    /// Only the first entry of a `$STATE_DIRECTORY`-style list is a usable path.
    #[test]
    fn takes_the_first_directory_of_a_list() {
        // SAFETY: single-threaded test, and the variable is read nowhere else here.
        unsafe { std::env::set_var("CARJACK_TEST_DIRS", "/var/lib/one:/var/lib/two") };
        assert_eq!(
            super::systemd_dir("CARJACK_TEST_DIRS"),
            Some(std::path::PathBuf::from("/var/lib/one"))
        );
        unsafe { std::env::remove_var("CARJACK_TEST_DIRS") };
        assert_eq!(super::systemd_dir("CARJACK_TEST_DIRS"), None);
    }
}
