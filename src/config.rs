//! carjack runtime configuration, loaded from a TOML file (see [`Config::load`]), overridable by
//! `CARJACK_*` environment variables and CLI flags. Also resolves the configured DRM device
//! selector to a concrete render node.

use std::path::{Path, PathBuf};

use eyre::{Result, eyre};
use serde::Deserialize;

/// Top-level config. Every field has a default, so a missing/partial file is fine.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub graphics: Graphics,
    pub display: Display,
    pub app: App,
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
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            drm_device: "auto".into(),
            zerocopy: ZeroCopyMode::Auto,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Display {
    /// Initial compositor size/rate, used before the car reports its real display over AirPlay.
    /// Set these to your car's values so an app that can't live-resize (Waydroid) boots correct
    /// and the later reconfigure is a no-op.
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
    /// Whether the app can live-resize. `true`: launched at startup and left to resize itself.
    /// `false` (Android/Waydroid, which fix their resolution at boot): *not* launched until the
    /// car's resolution is known, and relaunched on later reconnects that change the size.
    pub resizable: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            command: String::new(),
            resizable: false,
        }
    }
}

/// Command-line arguments. `-c` picks the config file; the rest override individual config
/// settings (flag > env var > config file > default).
#[derive(Debug, Clone, clap::Parser)]
#[command(about = "Wireless CarPlay bridge - streams a Wayland desktop to a car head unit")]
pub struct Cli {
    /// Config file to load (default: ~/.config/carjack/config.toml if present).
    #[arg(short = 'c', long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Override [graphics] drm_device: auto | <driver> | <path> | <index>.
    #[arg(long, value_name = "DEVICE")]
    pub drm_device: Option<String>,

    /// Override [graphics] zerocopy: auto | true | false.
    #[arg(long, value_name = "MODE")]
    pub zerocopy: Option<String>,

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
}

impl Config {
    /// Load config from `--config` (or the XDG default `~/.config/carjack/config.toml` if it
    /// exists, else all-defaults), then apply overrides: environment variables, then CLI flags
    /// (highest precedence).
    pub fn load(cli: &Cli) -> Result<Config> {
        let path = match &cli.config {
            Some(p) => {
                if !p.exists() {
                    return Err(eyre!("config file {} does not exist", p.display()));
                }
                Some(p.clone())
            }
            None => xdg::BaseDirectories::with_prefix("carjack").find_config_file("config.toml"),
        };

        let mut builder = config::Config::builder();
        if let Some(p) = &path {
            builder = builder.add_source(config::File::from(p.clone()));
        }
        // Environment support: any setting can be overridden via `CARJACK_<SECTION>.<KEY>`,
        // e.g. `CARJACK_GRAPHICS.DRM_DEVICE`, `CARJACK_APP.COMMAND`,
        // `CARJACK_DISPLAY__WIDTH`. (`try_parsing` coerces numbers/bools from the string values.)
        builder = builder.add_source(config::Environment::with_prefix("CARJACK").try_parsing(true));
        let mut cfg: Config = builder
            .build()
            .map_err(|e| eyre!("read config: {e}"))?
            .try_deserialize()
            .map_err(|e| eyre!("parse config: {e}"))?;

        // CLI flag overrides (highest precedence).
        if let Some(v) = &cli.drm_device {
            cfg.graphics.drm_device = v.clone();
        }
        if let Some(v) = &cli.zerocopy {
            cfg.graphics.zerocopy = ZeroCopyMode::parse(v)?;
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
        Ok(cfg)
    }

    /// The desktop command to launch, if any.
    pub fn desktop_command(&self) -> &str {
        self.app.command.trim()
    }
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

/// Rank a driver by how well we can hardware-encode on it, so `auto` picks the GPU we can do
/// zero-copy on: nvidia (NVENC) first, then amdgpu/i915 (VA-API), then others (v4l2), avoiding
/// virtual devices. Lower is better.
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
    // min_by_key keeps the first on ties, which is the lowest-numbered node (stable/predictable).
    nodes
        .iter()
        .min_by_key(|(_, drv)| driver_rank(drv))
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
    fn zerocopy_accepts_bool_or_string_and_fills_defaults() {
        // string form + a partial section: everything else falls back to defaults.
        let cfg = parse_toml("[graphics]\nzerocopy = \"auto\"\n");
        assert_eq!(cfg.graphics.zerocopy, ZeroCopyMode::Auto);
        assert_eq!(cfg.graphics.drm_device, "auto"); // default kept
        assert_eq!(cfg.display.width, 1280); // default section
        assert!(!cfg.app.resizable); // default
    }

    #[test]
    fn cli_flags_override_everything() {
        use clap::Parser;
        // CLI is applied last, so it wins over defaults, config file, and env.
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

    #[test]
    fn auto_selects_nvidia_when_present() {
        // Machine-gated: only asserts on hosts that actually expose an nvidia render node.
        let nodes = enumerate_render_nodes();
        if let Some((nv_path, _)) = nodes.iter().find(|(_, d)| d == "nvidia") {
            assert_eq!(&resolve_render_node("auto").unwrap(), nv_path);
        }
    }
}
