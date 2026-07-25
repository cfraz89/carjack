use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{
        DeviceType, NotifyTouchDownOptions, NotifyTouchMotionOptions, NotifyTouchUpOptions,
        RemoteDesktop, SelectDevicesOptions,
    },
    screencast::{Screencast, SelectSourcesOptions, SourceType},
};
use eyre::{Result, eyre};
use libcarjack::carplay_session::{TouchEvent, TouchEventType};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tracing::debug;
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle};

/// The output scale reported by whatever Wayland compositor is running (any DE) - read from
/// `wl_output`, so it doesn't depend on GNOME/KDE-specific APIs. The RemoteDesktop portal maps
/// touch in *logical* coordinates while `pipewiresrc` delivers *physical* pixels, so on a HiDPI
/// desktop (e.g. a 3456x2160 panel at scale 2 = 1728x1080 logical) the physical size must be
/// divided by this before touch is scaled to it, else the bottom/right of the screen is
/// unreachable. `None` if there's no Wayland display or no output advertised a scale (callers then
/// assume 1.0). `wl_output` scale is integer; fractional scaling reports the rounded-up value.
pub fn display_scale() -> Option<f64> {
    let conn = Connection::connect_to_env().ok()?;
    let mut queue = conn.new_event_queue::<ScaleState>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = ScaleState::default();
    queue.roundtrip(&mut state).ok()?; // registry globals -> bind outputs
    queue.roundtrip(&mut state).ok()?; // output events -> scale
    (state.scale > 0).then_some(f64::from(state.scale))
}

/// Collects the max `wl_output` scale across all outputs.
#[derive(Default)]
struct ScaleState {
    scale: i32,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ScaleState {
    fn event(
        _state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_output" {
                // v2+ carries the `scale` event.
                registry.bind::<wl_output::WlOutput, (), Self>(name, version.min(2), qh, ());
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for ScaleState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Scale { factor } = event {
            state.scale = state.scale.max(factor);
        }
    }
}

pub struct RemoteDesktopStream {
    /// Held for input injection (`send_touch`).
    pub remote_desktop: RemoteDesktop,
    /// Held to keep the portal ScreenCast/RemoteDesktop session (and its PipeWire node) alive.
    pub session: Session<RemoteDesktop>,
    pub node_id: u32,
    /// The desktop stream's size in pixels (from the portal). `notify_touch_*` coordinates are in
    /// *this* space, but CarPlay sends touch in the car's (scaled-down) display space - so we scale
    /// car coords up to this. `None` if the portal didn't report a size (then coords pass through).
    stream_size: Option<(f64, f64)>,
}

impl RemoteDesktopStream {
    /// Inject a CarPlay touch. `car_size` is the car's display size (the space `event.x/.y` are in);
    /// coords are scaled to the desktop stream's size, which is what the portal expects. Prefer the
    /// caller-provided `desktop_size` (the encoder's negotiated pipewiresrc size, the frame the car
    /// actually sees) over the portal-reported stream size, which can be missing or HiDPI-physical.
    pub async fn send_touch(
        &self,
        event: &TouchEvent,
        car_size: (u32, u32),
        desktop_size: Option<(u32, u32)>,
    ) -> Result<()> {
        let stream_id = self.node_id;
        let target = desktop_size
            .map(|(w, h)| (f64::from(w), f64::from(h)))
            .or(self.stream_size);
        let (x, y) = scale_touch(event.x, event.y, target, car_size);
        debug!(
            "touch {:?} slot={} car=({:.0},{:.0}) car_size={car_size:?} target={target:?} -> ({x:.0},{y:.0})",
            event.event_type, event.slot, event.x, event.y
        );
        match event.event_type {
            TouchEventType::Down => {
                self.remote_desktop
                    .notify_touch_down(
                        &self.session,
                        stream_id,
                        event.slot,
                        x,
                        y,
                        NotifyTouchDownOptions::default(),
                    )
                    .await?;
            }
            TouchEventType::Motion => {
                self.remote_desktop
                    .notify_touch_motion(
                        &self.session,
                        stream_id,
                        event.slot,
                        x,
                        y,
                        NotifyTouchMotionOptions::default(),
                    )
                    .await?;
            }
            TouchEventType::Up => {
                self.remote_desktop
                    .notify_touch_up(&self.session, event.slot, NotifyTouchUpOptions::default())
                    .await?;
            }
        }
        Ok(())
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }
}

/// Scale a car-space touch coordinate (`x`, `y` in the car's `car_size` display) to the desktop
/// stream's pixel space. The desktop is scaled *down* to the car's resolution for streaming, so
/// touch has to be scaled back *up*. Passes through unchanged if the stream size is unknown or the
/// car size is degenerate.
fn scale_touch(
    x: f64,
    y: f64,
    stream_size: Option<(f64, f64)>,
    car_size: (u32, u32),
) -> (f64, f64) {
    match stream_size {
        Some((sw, sh)) if car_size.0 > 0 && car_size.1 > 0 => (
            x * sw / f64::from(car_size.0),
            y * sh / f64::from(car_size.1),
        ),
        _ => (x, y),
    }
}

#[cfg(test)]
mod tests {
    use super::{load_restore_token_at, save_restore_token_at, scale_touch};

    /// A scratch path in the target dir; no tempfile dep in this crate.
    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("carjack-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn restore_token_round_trips() {
        let path = scratch("restore-token");
        assert_eq!(load_restore_token_at(&path), None, "missing file -> no token");

        save_restore_token_at(&path, "tok-abc123").unwrap();
        assert_eq!(load_restore_token_at(&path).as_deref(), Some("tok-abc123"));

        // Rotated token overwrites the old one.
        save_restore_token_at(&path, "tok-def456").unwrap();
        assert_eq!(load_restore_token_at(&path).as_deref(), Some("tok-def456"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token must not be world/group readable");
        }

        // An empty (e.g. truncated) file reads as "no token" rather than an empty one, which the
        // portal would reject.
        std::fs::write(&path, "").unwrap();
        assert_eq!(load_restore_token_at(&path), None);
        // Trailing newline from a hand-edited file is trimmed.
        std::fs::write(&path, "tok-xyz\n").unwrap();
        assert_eq!(load_restore_token_at(&path).as_deref(), Some("tok-xyz"));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn display_scale_probe() {
        match super::display_scale() {
            Some(s) => {
                eprintln!("wayland display scale = {s}");
                assert!(s > 0.0);
            }
            None => eprintln!("no wayland display scale (no WAYLAND_DISPLAY) - skipping"),
        }
    }

    #[test]
    fn touch_scales_car_space_to_stream_space() {
        // 1080x600 car -> 1920x1080 desktop stream.
        let stream = Some((1920.0, 1080.0));
        // centre stays centre
        assert_eq!(
            scale_touch(540.0, 300.0, stream, (1080, 600)),
            (960.0, 540.0)
        );
        // corners map to the stream's corners
        assert_eq!(scale_touch(0.0, 0.0, stream, (1080, 600)), (0.0, 0.0));
        assert_eq!(
            scale_touch(1080.0, 600.0, stream, (1080, 600)),
            (1920.0, 1080.0)
        );
        // unknown stream size or degenerate car size -> pass through
        assert_eq!(scale_touch(500.0, 250.0, None, (1080, 600)), (500.0, 250.0));
        assert_eq!(scale_touch(500.0, 250.0, stream, (0, 0)), (500.0, 250.0));
    }
}

/// Path of the stored portal restore token, `~/.local/state/carjack/portal-restore-token`.
/// State (not cache): losing it costs the user a consent dialog, so it shouldn't be swept up by a
/// cache clean. `None` if there's no state dir to write to.
fn restore_token_path() -> Option<PathBuf> {
    xdg::BaseDirectories::with_prefix("carjack")
        .place_state_file("portal-restore-token")
        .ok()
}

/// The saved restore token, if any. A missing/unreadable/empty file just means "no token yet" -
/// the portal then shows its consent dialog as on a first run.
fn load_restore_token() -> Option<String> {
    load_restore_token_at(&restore_token_path()?)
}

fn load_restore_token_at(path: &Path) -> Option<String> {
    let token = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Persist the token the portal handed back. Portals may rotate it on every restore, so this is
/// called with whatever `Start` returned, every session. Written 0600 - the token grants silent
/// screen capture and input injection to whoever holds it.
fn save_restore_token(token: &str) {
    let Some(path) = restore_token_path() else {
        return;
    };
    if let Err(e) = save_restore_token_at(&path, token) {
        tracing::warn!("portal: write restore token {}: {e}", path.display());
    }
}

fn save_restore_token_at(path: &Path, token: &str) -> std::io::Result<()> {
    std::fs::write(path, token)?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Forget a token the portal wouldn't accept, so the next attempt starts from a clean consent.
fn clear_restore_token() {
    let Some(path) = restore_token_path() else {
        return;
    };
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::info!("portal: discarded stale restore token"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("portal: remove restore token {}: {e}", path.display()),
    }
}

/// Open the ScreenCast + RemoteDesktop session. With a saved restore token the portal reuses the
/// previous selection and (`PersistMode::Application`) grants it without a dialog; without one the
/// user is prompted, and the token the portal returns is saved for next time. A token the portal
/// rejects is discarded and the session retried once, prompting.
pub async fn start_remote_desktop() -> Result<RemoteDesktopStream> {
    let token = load_restore_token();
    if token.is_some() {
        debug!("portal: restoring session from saved token");
    }
    match start_session(token.as_deref()).await {
        Ok(stream) => Ok(stream),
        // Only a token-bearing attempt is worth retrying: without one there's nothing stale to
        // blame, and a second prompt would just annoy the user. A cancelled request is the user
        // saying no - re-prompting them would be worse than failing.
        Err(e) if token.is_some() && !is_cancelled(&e) => {
            tracing::warn!("portal: restoring saved session failed ({e}), re-requesting consent");
            clear_restore_token();
            start_session(None).await
        }
        Err(e) => Err(e),
    }
}

/// Whether a portal error is the user dismissing/denying the dialog.
fn is_cancelled(e: &eyre::Report) -> bool {
    matches!(
        e.downcast_ref::<ashpd::Error>(),
        Some(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled))
    )
}

async fn start_session(restore_token: Option<&str>) -> Result<RemoteDesktopStream> {
    let remote_desktop = RemoteDesktop::new().await?;
    let session = remote_desktop.create_session(Default::default()).await?;

    remote_desktop
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Touchscreen)
                .set_persist_mode(PersistMode::Application)
                .set_restore_token(restore_token),
        )
        .await?;

    Screencast::new()
        .await?
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_sources(SourceType::Monitor | SourceType::Virtual | SourceType::Window)
                // The screencast half needs its own persist mode + token, otherwise the source
                // picker reappears every run even though the device grant was remembered.
                .set_persist_mode(PersistMode::Application)
                .set_restore_token(restore_token),
        )
        .await?;

    let response = remote_desktop
        .start(&session, None, Default::default())
        .await?
        .response()?;

    debug!(response = ?response, "Response");

    match response.restore_token() {
        Some(token) => save_restore_token(token),
        // Portal chose not to persist (user picked "deny"/one-off, or the backend doesn't support
        // it): drop any stale token so the next run doesn't try to restore something that's gone.
        None => clear_restore_token(),
    }

    let streams = response.streams();
    let stream = streams
        .first()
        .ok_or(eyre!("No stream in remote desktop response!"))?;
    let node_id = stream.pipe_wire_node_id();
    let stream_size = stream.size().map(|(w, h)| (f64::from(w), f64::from(h)));
    tracing::info!(
        "Desktop stream node {node_id}, size {:?} (touch coords scaled to this)",
        stream.size()
    );

    Ok(RemoteDesktopStream {
        remote_desktop,
        session,
        node_id,
        stream_size,
    })
}
