use std::sync::Arc;
use std::time::Duration;

use eyre::{Result, eyre};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

use carjack::compositor::Compositor;
use carjack::config::{Cli, Config, Source, Target};
use carjack::display_cache::DisplayCache;
use carjack::encode::{self, Bitrate};
use clap::Parser;

use crate::{wifi_connect::WifiBackend, wireless::IapHost};

mod accessory_settings;
mod app_service;
mod libei;
mod portal;
mod session;
mod usb;
mod wifi_connect;
mod wireless;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env_lossy(),
        )
        .init();

    spawn_sigint_watchdog()?;

    let config = Config::load(&Cli::parse())?;

    if config.bluetooth_device().is_none() {
        return Err(eyre!(
            "no head unit configured - set [bluetooth] device in config.toml to \"auto\", or to a \
             unit's MAC or advertised name; or pass --bt-device, or set CARJACK.BLUETOOTH.DEVICE"
        ));
    }

    // Shared HomeKit identity across both source paths and runs - in the state dir, not the cache,
    // so losing it doesn't leave every paired accessory with a dead pairing in its table.
    match carjack::paths::state_file("homekit.bin") {
        Ok(path) => libcarjack::airplay::init_homekit_storage(&path)?,
        Err(e) => tracing::warn!("No HomeKit storage path ({e}); pairing from scratch every run"),
    }

    let wifi: Arc<dyn WifiBackend> = wifi_connect::build().into();
    wifi.check_ready().await?;

    if config.source == Source::Desktop {
        portal::check_available().await?;
    }

    match config.source {
        Source::App => run_app(config, wifi).await,
        Source::Desktop => run_desktop(config, wifi).await,
    }
}

/// Make a second Ctrl-C always kill carjack, even if the graceful path is stuck.
fn spawn_sigint_watchdog() -> Result<()> {
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        tracing::info!("Stop requested, shutting down - signal again to exit immediately");
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        tracing::warn!("Second Ctrl-C, forcing exit");
        std::process::exit(130);
    });
    Ok(())
}

async fn shutdown_requested() {
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("No SIGTERM handler ({e}); Ctrl-C only");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("Ctrl-C received, shutting down"),
        _ = term.recv() => tracing::info!("SIGTERM received, shutting down"),
    }
}

/// Await a stop request or the retry backoff between sessions; returns `true` if we should stop.
async fn backoff_or_ctrl_c() -> bool {
    tokio::select! {
        _ = shutdown_requested() => true,
        _ = tokio::time::sleep(Duration::from_secs(2)) => false,
    }
}

/// `source = app`: run `[app] command` in carjack's own headless compositor and stream that.
async fn run_app(config: Config, wifi: Arc<dyn WifiBackend>) -> Result<()> {
    let render_node = carjack::config::resolve_render_node(&config.graphics.drm_device)?;
    // Provisional: `auto` settles on a codec once the accessory says what it decodes, below.
    let backend = encode::resolve_backend(
        config.graphics.zerocopy,
        &render_node,
        encode::resolve_codec(config.graphics.codec, false),
    )?;
    tracing::info!(
        "Render node: {render_node}, encode backend: {}",
        backend.describe()
    );

    let host = Arc::new(IapHost::start(config.hfp.clone()).await?);

    // Size the compositor from the last accessory's cached display, if any, so even a non-resizable app
    // can boot at the right size during the BT/WiFi/AirPlay setup rather than after.
    let mut cache = DisplayCache::load();
    let cached = cache.last_props();
    let (start_w, start_h, start_fps) = match cached {
        Some(p) => (p.width, p.height, p.fps),
        None => (
            config.display.width,
            config.display.height,
            config.display.fps,
        ),
    };
    let compositor = Compositor::start(
        render_node,
        backend.frame_format(),
        encode::importable_modifiers(&backend),
        start_w,
        start_h,
        start_fps,
    )?;
    // After `Compositor::start`: the service is pointed at its Wayland socket.
    let service =
        app_service::AppService::start(&config.app.service, &compositor.socket_name).await?;

    let mut app = session::DesktopApp::new(
        config.desktop_command().to_owned(),
        compositor.socket_name.clone(),
    );
    // Launch up front only if the target size is already known (resizable, or a cached display) -
    // otherwise `run_session` launches it once the accessory's real resolution comes in.
    if config.app.resizable || cached.is_some() {
        app.launch();
    }

    // Each session runs the full flow (Bluetooth -> WiFi -> AirPlay); on failure, restart from a
    // fresh Bluetooth connection instead of exiting.
    let exit = loop {
        tokio::select! {
            _ = shutdown_requested() => {
                tracing::info!("Ctrl-C received, shutting down");
                break Ok(());
            }
            result = session::run_session(&host, &compositor, &backend, &config, &mut app, &mut cache, &wifi) => {
                match result {
                    Ok(()) => {
                        tracing::info!("Session ended cleanly, exiting");
                        break Ok(());
                    }
                    Err(e) => {
                        tracing::warn!("Session failed, restarting from Bluetooth: {e}");
                        if backoff_or_ctrl_c().await {
                            break Ok(());
                        }
                    }
                }
            }
        }
    };

    // The app is a client of the service, which is a client of the compositor.
    app.kill();
    drop(service);
    drop(compositor);
    exit
}

/// `source = desktop`: stream the user's existing desktop via xdg-desktop-portal, with touch
/// injected back over libei (see [`libei`]).
async fn run_desktop(config: Config, wifi: Arc<dyn WifiBackend>) -> Result<()> {
    // Makes the gst `va` plugin register VA-API elements for every GPU on a multi-GPU box, not
    // just the first - needed to reach an integrated GPU that isn't the primary one. Must be set
    // before gst initializes.
    // SAFETY: nothing has initialized gst or read this var yet, and no other thread races on it.
    if std::env::var_os("GST_VA_ALL_DRIVERS").is_none() {
        unsafe { std::env::set_var("GST_VA_ALL_DRIVERS", "1") };
    }

    // Build the GStreamer registry now, not in the first session - see `encode::init_gstreamer`.
    tokio::task::spawn_blocking(encode::init_gstreamer)
        .await
        .map_err(|e| eyre!("gstreamer init task panicked: {e}"))??;

    let host = Arc::new(IapHost::start(config.hfp.clone()).await?);

    // ScreenCast + RemoteDesktop access, asked once (a consent dialog on first run).
    let rd = portal::start_remote_desktop().await?;
    tracing::info!("Desktop source ready (PipeWire node {})", rd.node_id());
    let zerocopy = config.graphics.zerocopy;
    let codec = config.graphics.codec;
    let bitrate = Bitrate::from_kbps(config.graphics.bitrate);
    let bt_device = config.bluetooth_device().unwrap_or(Target::Auto);

    let exit = loop {
        tokio::select! {
            _ = shutdown_requested() => {
                tracing::info!("Ctrl-C received, shutting down");
                break Ok(());
            }
            result = session::run_desktop_session(&host, &rd, zerocopy, codec, bitrate, &bt_device, &wifi) => {
                // Lift anything still held: the input session outlives accessory sessions.
                rd.end_session();
                match result {
                    Ok(()) => {
                        tracing::info!("Session ended cleanly, exiting");
                        break Ok(());
                    }
                    Err(e) => {
                        tracing::warn!("Session failed, restarting from Bluetooth: {e}");
                        if backoff_or_ctrl_c().await {
                            break Ok(());
                        }
                    }
                }
            }
        }
    };

    exit
}
