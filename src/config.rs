//! carjack runtime configuration, loaded from a TOML file (see [`Config::load`]), overridable by
//! `CARJACK_*` environment variables and CLI flags.
use std::path::{Path, PathBuf};

use eyre::{Result, eyre};
use serde::Deserialize;

/// Top-level config. Every field has a default, so a missing/partial file is fine.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub source: Source,
    pub graphics: Graphics,
    pub display: Display,
    pub app: App,
    pub bluetooth: Bluetooth,
    pub hfp: Hfp,
    pub wifi: Wifi,
}

/// Which physical adapter plays which role, on a board with more than one.
///
/// Both accept an interface name (`wlan0`) or a MAC, and both may be left unset - see
/// [`Wifi::selector`] for what that resolves to.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Wifi {
    /// Associates with the head unit's access point.
    pub carplay: String,
    /// Carries the internet uplink, and hosts the setup AP when there is nothing to connect to.
    pub uplink: String,
}

/// The two things a WiFi adapter can be doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiRole {
    Carplay,
    Uplink,
}

impl Wifi {
    /// The configured selector for a role, or `None` to let the backend work it out.
    pub fn selector(&self, role: WifiRole) -> Option<&str> {
        let s = match role {
            WifiRole::Carplay => self.carplay.trim(),
            WifiRole::Uplink => self.uplink.trim(),
        };
        (!s.is_empty()).then_some(s)
    }
}

/// What carjack's hands-free gateway answers the unit's identity probe with. The defaults are an
/// iPhone's: a unit that gates CarPlay on the model string accepts nothing else.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Hfp {
    pub manufacturer: String,
    pub model: String,
    /// Only ever displayed by the unit, so left out of `+CNUM`/`+COPS` when unset.
    pub number: Option<String>,
    pub operator: Option<String>,
}

impl Default for Hfp {
    fn default() -> Self {
        Self {
            manufacturer: String::from("Apple Inc."),
            model: String::from("iPhone16,1"),
            number: None,
            operator: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
/// Which head unit to connect to.
pub struct Bluetooth {
    pub device: String,
}

/// The head unit a run is aiming at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Take whichever CarPlay-capable unit is in range, strongest signal first.
    Auto,
    /// One specific unit, by MAC or advertised name.
    Selector(String),
}

impl Target {
    /// `None` for an unset (or whitespace-only) setting, which means nothing to connect to.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            None
        } else if s.eq_ignore_ascii_case("auto") {
            Some(Self::Auto)
        } else {
            Some(Self::Selector(s.to_owned()))
        }
    }

    /// The MAC this target names, if it names one at all.
    pub fn address(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::Selector(s) => s.contains(':').then_some(s.as_str()),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Selector(s) => f.write_str(s),
        }
    }
}

/// Video source: run our own app, or capture the existing desktop via the portal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Run `[app] command` in carjack's own headless compositor and stream that (default).
    #[default]
    App,
    /// Capture the user's existing Wayland desktop via xdg-desktop-portal (ScreenCast) and inject
    /// input via RemoteDesktop. Needs a running desktop session with a portal backend; `[app]`,
    /// `[display]`, and the display cache are ignored in this mode.
    Desktop,
}

impl Source {
    fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "app" => Ok(Self::App),
            "desktop" => Ok(Self::Desktop),
            other => Err(eyre!("invalid source '{other}' (expected app/desktop)")),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Graphics {
    /// Which DRM device to render *and* encode on (they must match for zero-copy). One of:
    /// `"auto"`, a driver name (`"nvidia"`, `"amdgpu"`, `"i915"`, ...), a render-node path
    /// (`"/dev/dri/renderD128"`), or a numeric index (`"0"` -> `renderD128`, `"1"` -> `renderD129`).
    pub drm_device: String,
    /// Zero-copy encode mode: `"auto"` (try it, fall back to software with a warning), `true`
    /// (require it - error out if the pipeline can't be built), or `false` (software).
    pub zerocopy: ZeroCopyMode,
    /// Video codec: `"auto"` (HEVC, which is what an iPhone sends), `"hevc"`, or `"h264"` for an
    /// accessory that needs the older one.
    pub codec: CodecChoice,
    /// Target bitrate in kbit/s. Variable, so this is a ceiling. Kept modest by default: the
    /// accessory's WiFi link is the bottleneck, and overshooting it shows up as stutter.
    pub bitrate: u32,
}

/// Default video bitrate, in kbit/s (3 Mbit/s).
pub const DEFAULT_BITRATE_KBPS: u32 = 3000;

impl Default for Graphics {
    fn default() -> Self {
        Self {
            drm_device: "auto".into(),
            zerocopy: ZeroCopyMode::Auto,
            codec: CodecChoice::Auto,
            bitrate: DEFAULT_BITRATE_KBPS,
        }
    }
}

/// Zero-copy selection. Accepts either a bool (`true`/`false`) or a string (`"auto"`/`"true"`/
/// `"false"`) in the TOML so `zerocopy = true` and `zerocopy = "auto"` both work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroCopyMode {
    /// Try zero-copy; on any failure fall back to software (with a warning).
    Auto,
    /// Require zero-copy; error out if it can't be set up.
    Force,
    /// Software encode only.
    Off,
}

impl<'de> Deserialize<'de> for ZeroCopyMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bool(true) => Ok(Self::Force),
            Raw::Bool(false) => Ok(Self::Off),
            Raw::Str(s) => Self::parse(&s).map_err(serde::de::Error::custom),
        }
    }
}

impl ZeroCopyMode {
    fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "true" | "yes" | "on" | "1" => Ok(Self::Force),
            "false" | "no" | "off" | "0" => Ok(Self::Off),
            other => Err(eyre!(
                "invalid zerocopy value '{other}' (expected auto/true/false)"
            )),
        }
    }
}

/// Which codec to encode the screen stream in. `Auto` resolves to HEVC: an iPhone sends it even to
/// a unit that advertises no codec support, so there is nothing reliable to negotiate against - see
/// `encode::resolve_codec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodecChoice {
    #[default]
    Auto,
    Hevc,
    H264,
}

impl CodecChoice {
    fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "hevc" | "h265" | "h.265" => Ok(Self::Hevc),
            "h264" | "h.264" | "avc" => Ok(Self::H264),
            other => Err(eyre!(
                "invalid codec value '{other}' (expected auto/hevc/h264)"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for CodecChoice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Display {
    /// Initial compositor size/rate, used before the accessory reports its display over AirPlay.
    /// Set to the accessory's values so an app that can't live-resize boots correct.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct App {
    /// Desktop command to stream (run via `sh -c`). Empty = stream black until a client connects.
    pub command: String,
    /// `true`: launched at startup and left to resize itself. `false` (apps that fix their
    /// resolution at boot, e.g. Waydroid): held until the accessory's resolution is known.
    pub resizable: bool,
    /// A background process `command` needs running - see [`AppService`].
    pub service: AppService,
}

impl Default for App {
    fn default() -> Self {
        Self {
            command: String::new(),
            resizable: false,
            service: AppService::default(),
        }
    }
}

/// A background process `[app] command` depends on, brought up before it and taken down after it.
/// All three run via `sh -c`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppService {
    /// Long-running, spawned with `WAYLAND_DISPLAY` pointing at carjack's compositor. Empty
    /// disables the whole section.
    pub start: String,
    /// Polled until it exits 0, between `start` and `[app] command`. Empty = don't wait.
    pub ready: String,
    /// Graceful stop, given a chance to end `start` before it is signalled.
    pub stop: String,
}

impl AppService {
    pub fn is_enabled(&self) -> bool {
        !self.start.trim().is_empty()
    }
}

/// Command-line arguments. `-c` picks the config file; the rest override individual config
/// settings (flag > env var > config file > default).
#[derive(Debug, Clone, clap::Parser)]
#[command(about = "Wireless CarPlay bridge - streams a Wayland desktop to an accessory head unit")]
pub struct Cli {
    /// Config file to load (default: ~/.config/carjack/config.toml, falling back to
    /// /etc/carjack/config.toml under the systemd service).
    #[arg(short = 'c', long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Override source: app (run [app] command in our compositor) | desktop (existing desktop).
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,

    /// Override [graphics] drm_device: auto | <driver> | <path> | <index>.
    #[arg(long, value_name = "DEVICE")]
    pub drm_device: Option<String>,

    /// Override [graphics] zerocopy: auto | true | false.
    #[arg(long, value_name = "MODE")]
    pub zerocopy: Option<String>,

    /// Override [graphics] bitrate: target video bitrate in kbit/s (default 3000 = 3 Mbit/s).
    #[arg(long, value_name = "KBPS")]
    pub bitrate: Option<u32>,

    /// Override [display] width / height / fps.
    #[arg(long)]
    pub width: Option<u32>,
    #[arg(long)]
    pub height: Option<u32>,
    #[arg(long)]
    pub fps: Option<u32>,

    /// Override [app] command (desktop app to stream).
    #[arg(long, value_name = "CMD")]
    pub command: Option<String>,

    /// Override [app] resizable.
    #[arg(long, action = clap::ArgAction::Set, value_name = "BOOL")]
    pub resizable: Option<bool>,

    /// Override [bluetooth] device: "auto", or the head unit's MAC or advertised name.
    #[arg(long, value_name = "AUTO|MAC|NAME")]
    pub bt_device: Option<String>,
}

impl Config {
    /// Load config from [`config_file`], then apply overrides: environment variables, then CLI
    /// flags (highest precedence).
    pub fn load(cli: &Cli) -> Result<Config> {
        let path = config_file(cli)?;

        let mut builder = config::Config::builder();
        if let Some(p) = &path {
            builder = builder.add_source(config::File::from(p.clone()));
        }
        // The config portal's own file, over the hand-written one - see `crate::stored_settings`.
        if let Some(p) = crate::stored_settings::StoredSettings::path().filter(|p| p.exists()) {
            builder = builder.add_source(config::File::from(p));
        }
        // `CARJACK.<dotted.path>` overrides any setting - dots throughout, including the prefix
        // separator (not the crate's default `_`), to match the config paths.
        builder = builder.add_source(
            config::Environment::with_prefix("CARJACK")
                .prefix_separator(".")
                .try_parsing(true),
        );
        let mut cfg: Config = builder
            .build()
            .map_err(|e| eyre!("read config: {e}"))?
            .try_deserialize()
            .map_err(|e| eyre!("parse config: {e}"))?;

        apply_cli_overrides(&mut cfg, cli)?;
        Ok(cfg)
    }

    /// The desktop command to launch, if any.
    pub fn desktop_command(&self) -> &str {
        self.app.command.trim()
    }

    /// The configured head unit, or `None` if unset.
    pub fn bluetooth_device(&self) -> Option<Target> {
        Target::parse(&self.bluetooth.device)
    }
}

/// The config file to read: an explicit `--config` (which must exist), else the user's
/// `~/.config/carjack/config.toml`, else `/etc/carjack/config.toml` (the systemd service's `HOME`
/// has no `.config`, so it lands there), else none (all defaults).
fn config_file(cli: &Cli) -> Result<Option<PathBuf>> {
    match &cli.config {
        Some(p) => {
            if !p.exists() {
                return Err(eyre!("config file {} does not exist", p.display()));
            }
            Ok(Some(p.clone()))
        }
        None => Ok(crate::paths::config_file("config.toml")),
    }
}

/// Apply `--*` flags over a loaded config, highest precedence of the override chain.
fn apply_cli_overrides(cfg: &mut Config, cli: &Cli) -> Result<()> {
    if let Some(v) = &cli.source {
        cfg.source = Source::parse(v)?;
    }
    if let Some(v) = &cli.drm_device {
        cfg.graphics.drm_device = v.clone();
    }
    if let Some(v) = &cli.zerocopy {
        cfg.graphics.zerocopy = ZeroCopyMode::parse(v)?;
    }
    if let Some(v) = cli.bitrate {
        cfg.graphics.bitrate = v;
    }
    if let Some(v) = cli.width {
        cfg.display.width = v;
    }
    if let Some(v) = cli.height {
        cfg.display.height = v;
    }
    if let Some(v) = cli.fps {
        cfg.display.fps = v;
    }
    if let Some(v) = &cli.command {
        cfg.app.command = v.clone();
    }
    if let Some(v) = cli.resizable {
        cfg.app.resizable = v;
    }
    if let Some(v) = &cli.bt_device {
        cfg.bluetooth.device = v.clone();
    }
    if cfg.graphics.bitrate == 0 {
        return Err(eyre!("[graphics] bitrate must be greater than 0 (kbit/s)"));
    }
    Ok(())
}

/// The kernel driver of the GPU that renders the desktop - the driver of the card with a connected
/// display. Used to pick a zero-copy encoder on the same GPU the ScreenCast dmabuf lives on.
pub fn desktop_render_driver() -> Option<String> {
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Connector dirs look like "card1-eDP-1"; skip the bare "card1" / "renderD*" nodes.
        let Some((card, _connector)) = name.split_once('-') else {
            continue;
        };
        if !card.starts_with("card") {
            continue;
        }
        let connected = std::fs::read_to_string(entry.path().join("status"))
            .map(|s| s.trim() == "connected")
            .unwrap_or(false);
        if !connected {
            continue;
        }
        if let Ok(link) = std::fs::read_link(format!("/sys/class/drm/{card}/device/driver")) {
            if let Some(drv) = link.file_name() {
                return Some(drv.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// The kernel driver behind a `/dev/dri/renderD*` path. `None` if sysfs can't say.
pub fn render_node_driver(node: &str) -> Option<String> {
    let name = Path::new(node).file_name()?.to_string_lossy().into_owned();
    std::fs::read_link(format!("/sys/class/drm/{name}/device/driver"))
        .ok()?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Resolve the configured `drm_device` selector to a concrete render-node path.
pub fn resolve_render_node(selector: &str) -> Result<String> {
    let sel = selector.trim();

    // Explicit device path.
    if sel.starts_with('/') {
        return if Path::new(sel).exists() {
            Ok(sel.to_string())
        } else {
            Err(eyre!("configured drm_device '{sel}' does not exist"))
        };
    }

    let nodes = enumerate_render_nodes();
    if nodes.is_empty() {
        return Err(eyre!("no DRM render nodes found under /dev/dri"));
    }

    match sel.to_ascii_lowercase().as_str() {
        "auto" | "" => {
            let pick = pick_auto(&nodes);
            tracing::info!("drm_device=auto -> {} (driver {})", pick.0, pick.1);
            Ok(pick.0.clone())
        }
        other => {
            // Numeric index: 0 -> renderD128, 1 -> renderD129, ...
            if let Ok(idx) = other.parse::<u32>() {
                let path = format!("/dev/dri/renderD{}", 128 + idx);
                return if Path::new(&path).exists() {
                    Ok(path)
                } else {
                    Err(eyre!("drm_device index {idx} ({path}) does not exist"))
                };
            }
            // Driver name (e.g. "nvidia", "amdgpu", "i915").
            nodes
                .iter()
                .find(|(_, drv)| drv.eq_ignore_ascii_case(other))
                .map(|(p, _)| p.clone())
                .ok_or_else(|| {
                    let found = nodes
                        .iter()
                        .map(|(p, d)| format!("{p}={d}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    eyre!("no DRM render node with driver '{other}' (found: {found})")
                })
        }
    }
}

/// All `/dev/dri/renderD*` nodes with their kernel driver (from sysfs), sorted by node name.
fn enumerate_render_nodes() -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut paths: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = Path::new(&p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let driver = std::fs::read_link(format!("/sys/class/drm/{name}/device/driver"))
                .ok()
                .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "unknown".to_string());
            (p, driver)
        })
        .collect()
}

/// Rank a driver by hardware-encode capability: nvidia first, then amdgpu/i915, then others,
/// virtual devices last. Lower is better. Only a tiebreak - see [`pick_auto`].
fn driver_rank(driver: &str) -> u8 {
    match driver {
        "nvidia" | "nvidia-drm" => 0,
        "amdgpu" | "radeon" => 1,
        "i915" | "xe" => 2,
        "vgem" | "vkms" => 250, // virtual/software - last resort
        _ => 100,               // unknown / embedded (e.g. v4l2 target)
    }
}

fn pick_auto(nodes: &[(String, String)]) -> &(String, String) {
    // A GPU this build can zero-copy encode on beats a nominally faster one it cannot: on a
    // non-cuda build nvidia has no encode path, so ranking it first would force software encode
    // with an Intel/AMD GPU sitting idle. `min_by_key` keeps the lowest-numbered node on ties.
    nodes
        .iter()
        .min_by_key(|(_, drv)| {
            (
                !crate::encode::driver_has_zero_copy_backend(drv),
                driver_rank(drv),
            )
        })
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_rank_prefers_encode_capable_gpus() {
        assert!(driver_rank("nvidia") < driver_rank("amdgpu"));
        assert!(driver_rank("amdgpu") < driver_rank("i915"));
        assert!(driver_rank("i915") < driver_rank("unknown_driver"));
        assert!(driver_rank("unknown_driver") < driver_rank("vgem"));
    }

    #[test]
    fn zerocopy_mode_parses_strings() {
        assert_eq!(ZeroCopyMode::parse("auto").unwrap(), ZeroCopyMode::Auto);
        assert_eq!(ZeroCopyMode::parse("TRUE").unwrap(), ZeroCopyMode::Force);
        assert_eq!(ZeroCopyMode::parse("off").unwrap(), ZeroCopyMode::Off);
        assert!(ZeroCopyMode::parse("nonsense").is_err());
    }

    fn parse_toml(s: &str) -> Config {
        config::Config::builder()
            .add_source(config::File::from_str(s, config::FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }

    #[test]
    fn parses_full_config() {
        let cfg = parse_toml(
            r#"
            [graphics]
            drm_device = "nvidia"
            zerocopy = true
            [display]
            width = 800
            height = 480
            fps = 60
            [app]
            command = "foo"
            resizable = true
            "#,
        );
        assert_eq!(cfg.graphics.drm_device, "nvidia");
        assert_eq!(cfg.graphics.zerocopy, ZeroCopyMode::Force);
        assert_eq!(
            (cfg.display.width, cfg.display.height, cfg.display.fps),
            (800, 480, 60)
        );
        assert_eq!(cfg.desktop_command(), "foo");
        assert!(cfg.app.resizable);
    }

    #[test]
    fn app_service_parses_and_defaults_to_disabled() {
        // No [app.service] at all: nothing to manage.
        assert!(
            !parse_toml("[app]\ncommand = \"foo\"\n")
                .app
                .service
                .is_enabled()
        );
        // `ready`/`stop` are optional; `start` alone is enough to turn it on.
        let cfg = parse_toml("[app.service]\nstart = \"waydroid session start\"\n");
        assert!(cfg.app.service.is_enabled());
        assert_eq!(cfg.app.service.start, "waydroid session start");
        assert_eq!(cfg.app.service.ready, "");
        assert_eq!(cfg.app.service.stop, "");

        let cfg = parse_toml(
            r#"
            [app]
            command = "waydroid show-full-ui"
            [app.service]
            start = "waydroid session start"
            ready = "waydroid status | grep -q RUNNING"
            stop = "waydroid session stop"
            "#,
        );
        assert_eq!(cfg.desktop_command(), "waydroid show-full-ui");
        assert_eq!(cfg.app.service.stop, "waydroid session stop");
    }

    #[test]
    fn zerocopy_accepts_bool_or_string_and_fills_defaults() {
        // string form + a partial section: everything else falls back to defaults.
        let cfg = parse_toml("[graphics]\nzerocopy = \"auto\"\n");
        assert_eq!(cfg.graphics.zerocopy, ZeroCopyMode::Auto);
        assert_eq!(cfg.graphics.drm_device, "auto"); // default kept
        assert_eq!(cfg.display.width, 1280); // default section
        assert!(!cfg.app.resizable); // default
    }

    #[test]
    fn source_parses_and_defaults_app() {
        assert_eq!(Source::default(), Source::App);
        assert_eq!(Source::parse("app").unwrap(), Source::App);
        assert_eq!(Source::parse("desktop").unwrap(), Source::Desktop);
        assert!(Source::parse("internal").is_err());
        assert!(Source::parse("nonsense").is_err());
        // via config file (serde rename_all=lowercase)
        assert_eq!(parse_toml("source = \"desktop\"\n").source, Source::Desktop);
        assert_eq!(parse_toml("source = \"app\"\n").source, Source::App);
        assert_eq!(parse_toml("").source, Source::App);
    }

    #[test]
    fn env_vars_are_dot_separated() {
        // CARJACK.<path> with dots throughout (prefix separator included), matching config paths.
        let mut env = std::collections::HashMap::new();
        env.insert("CARJACK.SOURCE".to_string(), "desktop".to_string());
        env.insert("CARJACK.GRAPHICS.ZEROCOPY".to_string(), "false".to_string());
        env.insert("CARJACK.APP.COMMAND".to_string(), "foo".to_string());
        env.insert("CARJACK.DISPLAY.WIDTH".to_string(), "800".to_string());
        env.insert("CARJACK.GRAPHICS.BITRATE".to_string(), "6000".to_string());
        env.insert(
            "CARJACK.BLUETOOTH.DEVICE".to_string(),
            "MYCAR-1234".to_string(),
        );
        let cfg: Config = config::Config::builder()
            .add_source(
                config::Environment::with_prefix("CARJACK")
                    .prefix_separator(".")
                    .try_parsing(true)
                    .source(Some(env)),
            )
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.source, Source::Desktop);
        assert_eq!(cfg.graphics.zerocopy, ZeroCopyMode::Off);
        assert_eq!(cfg.desktop_command(), "foo");
        assert_eq!(cfg.display.width, 800);
        assert_eq!(cfg.graphics.bitrate, 6000);
        assert_eq!(
            cfg.bluetooth_device(),
            Some(Target::Selector("MYCAR-1234".into()))
        );
    }

    #[test]
    fn bt_device_flag_overrides_and_blank_is_none() {
        use clap::Parser;
        let cli = Cli::parse_from(["carjack", "--bt-device", "AA:BB:CC:DD:EE:FF"]);
        let cfg = Config::load(&cli).unwrap();
        assert_eq!(
            cfg.bluetooth_device(),
            Some(Target::Selector("AA:BB:CC:DD:EE:FF".into()))
        );

        // Unset and whitespace-only both read as "no target", which `main` rejects at startup.
        let blank = Config {
            bluetooth: Bluetooth {
                device: "   ".into(),
            },
            ..Config::default()
        };
        assert_eq!(blank.bluetooth_device(), None);
        assert_eq!(Config::default().bluetooth_device(), None);
    }

    #[test]
    fn target_parses_auto_and_selectors() {
        assert_eq!(Target::parse("auto"), Some(Target::Auto));
        assert_eq!(Target::parse("  AUTO  "), Some(Target::Auto));
        assert_eq!(Target::parse(""), None);
        assert_eq!(Target::parse("   "), None);
        assert_eq!(
            Target::parse("MYCAR-1234"),
            Some(Target::Selector("MYCAR-1234".into()))
        );
        // Only a MAC names an address; an advertised name has to be resolved by discovery.
        assert_eq!(
            Target::parse("AA:BB:CC:DD:EE:FF").unwrap().address(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(Target::parse("MYCAR-1234").unwrap().address(), None);
        assert_eq!(Target::Auto.address(), None);
        assert_eq!(Target::Auto.to_string(), "auto");
    }

    #[test]
    fn auto_is_a_target_not_a_unit_name() {
        assert_eq!(
            parse_toml("[bluetooth]\ndevice = \"auto\"\n").bluetooth_device(),
            Some(Target::Auto)
        );
        use clap::Parser;
        let cli = Cli::parse_from(["carjack", "--bt-device", "auto"]);
        assert_eq!(
            Config::load(&cli).unwrap().bluetooth_device(),
            Some(Target::Auto)
        );
    }

    #[test]
    fn wifi_roles_default_to_unset() {
        let cfg = parse_toml("");
        assert_eq!(cfg.wifi.selector(WifiRole::Carplay), None);
        assert_eq!(cfg.wifi.selector(WifiRole::Uplink), None);

        let cfg = parse_toml("[wifi]\ncarplay = \"wlan0\"\nuplink = \"  \"\n");
        assert_eq!(cfg.wifi.selector(WifiRole::Carplay), Some("wlan0"));
        // Whitespace is not a selector, it is an unset field.
        assert_eq!(cfg.wifi.selector(WifiRole::Uplink), None);

        let cfg = parse_toml("[wifi]\nuplink = \"AA:BB:CC:DD:EE:FF\"\n");
        assert_eq!(
            cfg.wifi.selector(WifiRole::Uplink),
            Some("AA:BB:CC:DD:EE:FF")
        );
    }

    #[test]
    fn bitrate_defaults_and_overrides() {
        use clap::Parser;
        assert_eq!(Config::default().graphics.bitrate, 3000); // 3 Mbit/s
        assert_eq!(parse_toml("").graphics.bitrate, 3000);
        assert_eq!(
            parse_toml("[graphics]\nbitrate = 8000\n").graphics.bitrate,
            8000
        );
        // Flag wins, and 0 is rejected rather than handed to an encoder.
        let cli = Cli::parse_from(["carjack", "--bitrate", "6000"]);
        assert_eq!(Config::load(&cli).unwrap().graphics.bitrate, 6000);
        assert!(Config::load(&Cli::parse_from(["carjack", "--bitrate", "0"])).is_err());
    }

    #[test]
    fn cli_flags_override_everything() {
        use clap::Parser;
        let cli = Cli::parse_from([
            "carjack",
            "--drm-device",
            "i915",
            "--zerocopy",
            "false",
            "--width",
            "640",
            "--resizable",
            "true",
        ]);
        let cfg = Config::load(&cli).unwrap();
        assert_eq!(cfg.graphics.drm_device, "i915");
        assert_eq!(cfg.graphics.zerocopy, ZeroCopyMode::Off);
        assert_eq!(cfg.display.width, 640);
        assert!(cfg.app.resizable);
    }

    /// Machine-gated: only asserts on hosts that expose a render node. `auto` must land on a GPU
    /// this build can zero-copy encode on whenever one exists.
    #[test]
    fn auto_prefers_a_gpu_this_build_can_encode_on() {
        let nodes = enumerate_render_nodes();
        if nodes.is_empty() {
            return;
        }
        let picked = resolve_render_node("auto").unwrap();
        let picked_driver = render_node_driver(&picked).unwrap_or_default();
        if nodes
            .iter()
            .any(|(_, d)| crate::encode::driver_has_zero_copy_backend(d))
        {
            assert!(
                crate::encode::driver_has_zero_copy_backend(&picked_driver),
                "auto chose {picked} (driver {picked_driver}), which has no zero-copy backend, \
                 while another node does"
            );
        }
    }
}
