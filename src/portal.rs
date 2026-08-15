use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{
        DeviceType, NotifyTouchDownOptions, NotifyTouchMotionOptions, NotifyTouchUpOptions,
        RemoteDesktop, SelectDevicesOptions,
    },
    screencast::{Screencast, SelectSourcesOptions, SourceType},
};
use eyre::{Result, eyre};
use libcarjack::carplay_init::{TouchEvent, TouchEventType};

use crate::libei::EiTouch;
use tracing::debug;
use wayland_client::protocol::{wl_compositor, wl_output, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// The display scale reported by the compositor - used to convert `pipewiresrc`'s *physical* pixel
/// size into the *logical* coordinates the RemoteDesktop portal injects touch in.
///
/// Prefers `wp_fractional_scale_v1::preferred_scale` (exact, a fraction over 120) over the integer
/// `wl_output.scale` fallback, which rounds under fractional scaling. `None` if there's no Wayland
/// display or neither source answered (callers then assume 1.0). Multi-monitor setups with
/// different per-output scales can get the wrong one, since this only probes one surface.
pub fn display_scale() -> Option<f64> {
    let state = probe_scale()?;
    if let Some(num) = state.fractional_scale {
        let scale = f64::from(num) / 120.0;
        tracing::debug!("display scale {scale} (wp_fractional_scale_v1 {num}/120)");
        return Some(scale);
    }
    if state.output_scale > 0 {
        tracing::debug!(
            "display scale {} (wl_output, integer - no wp_fractional_scale_v1)",
            state.output_scale
        );
        return Some(f64::from(state.output_scale));
    }
    None
}

/// Connect to the compositor and collect both scale sources. `None` if there's no Wayland display
/// to talk to.
fn probe_scale() -> Option<ScaleState> {
    let conn = Connection::connect_to_env().ok()?;
    let mut queue = conn.new_event_queue::<ScaleState>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = ScaleState::default();
    queue.roundtrip(&mut state).ok()?; // registry globals -> bind compositor/manager/outputs
    // The scale is per-surface, so ask about a throwaway one. It never gets a buffer attached, so
    // it can't map - nothing appears on the user's desktop.
    let _probe = state.start_fractional_probe(&qh);
    queue.roundtrip(&mut state).ok()?; // wl_output scales + preferred_scale for the probe surface
    Some(state)
}

/// What [`display_scale`] collects: the exact fractional scale if the compositor supports
/// `wp_fractional_scale_v1`, else the max integer `wl_output` scale across outputs.
#[derive(Default)]
struct ScaleState {
    /// Numerator of the `wp_fractional_scale_v1::preferred_scale` fraction (denominator 120).
    fractional_scale: Option<u32>,
    /// Max `wl_output` scale, the fallback.
    output_scale: i32,
    compositor: Option<wl_compositor::WlCompositor>,
    fractional_manager: Option<WpFractionalScaleManagerV1>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
}

/// The throwaway objects [`ScaleState::start_fractional_probe`] creates, kept alive until the
/// roundtrip that carries `preferred_scale` (destroying the surface would cancel the event).
#[allow(dead_code)]
struct FractionalProbe {
    surface: wl_surface::WlSurface,
    fractional: WpFractionalScaleV1,
    xdg_surface: Option<xdg_surface::XdgSurface>,
    toplevel: Option<xdg_toplevel::XdgToplevel>,
}

impl ScaleState {
    /// Create the throwaway surface and its `wp_fractional_scale_v1`, so the next roundtrip can
    /// pick up a `preferred_scale`. Given an `xdg_toplevel` role since Mutter only reports a
    /// preferred scale once a surface has one; no buffer is ever attached, so nothing appears
    /// on the user's desktop.
    fn start_fractional_probe(&self, qh: &QueueHandle<Self>) -> Option<FractionalProbe> {
        let compositor = self.compositor.as_ref()?;
        let manager = self.fractional_manager.as_ref()?;
        let surface = compositor.create_surface(qh, ());
        let fractional = manager.get_fractional_scale(&surface, qh, ());
        let (xdg_surface, toplevel) = match &self.wm_base {
            Some(wm_base) => {
                let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
                let toplevel = xdg_surface.get_toplevel(qh, ());
                toplevel.set_title("carjack scale probe".into());
                (Some(xdg_surface), Some(toplevel))
            }
            None => (None, None),
        };
        surface.commit();
        Some(FractionalProbe {
            surface,
            fractional,
            xdg_surface,
            toplevel,
        })
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for ScaleState {
    fn event(
        state: &mut Self,
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
            match interface.as_str() {
                "wl_output" => {
                    // v2+ carries the `scale` event.
                    registry.bind::<wl_output::WlOutput, (), Self>(name, version.min(2), qh, ());
                }
                "wl_compositor" => {
                    state.compositor = registry
                        .bind::<wl_compositor::WlCompositor, (), Self>(name, version.min(1), qh, ())
                        .into();
                }
                "wp_fractional_scale_manager_v1" => {
                    state.fractional_manager = registry
                        .bind::<WpFractionalScaleManagerV1, (), Self>(name, version.min(1), qh, ())
                        .into();
                }
                "xdg_wm_base" => {
                    state.wm_base = registry
                        .bind::<xdg_wm_base::XdgWmBase, (), Self>(name, version.min(1), qh, ())
                        .into();
                }
                _ => {}
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
            state.output_scale = state.output_scale.max(factor);
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for ScaleState {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.fractional_scale = Some(scale);
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for ScaleState {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Answer the keepalive ping; an unanswered one makes the compositor treat us as hung.
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for ScaleState {
    fn event(
        _: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Ack the configure so the role is fully established (we still never attach a buffer, so
        // the surface stays unmapped and invisible).
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
        }
    }
}

// No events we care about: wl_compositor and the manager have none, wl_surface only reports
// enter/leave/preferred_buffer_* for a surface we never map, and the toplevel's configure/close are
// irrelevant to a window that never appears.
delegate_noop!(ScaleState: ignore wl_compositor::WlCompositor);
delegate_noop!(ScaleState: ignore wl_surface::WlSurface);
delegate_noop!(ScaleState: ignore WpFractionalScaleManagerV1);
delegate_noop!(ScaleState: ignore xdg_toplevel::XdgToplevel);

pub struct RemoteDesktopStream {
    /// Held for input injection (`send_touch`).
    pub remote_desktop: RemoteDesktop,
    /// Held to keep the portal ScreenCast/RemoteDesktop session (and its PipeWire node) alive.
    pub session: Session<RemoteDesktop>,
    pub node_id: u32,
    /// The stream size the portal advertised - the space its `check_position` validates touch
    /// against. `None` if it didn't report one; the caller's frame size is used instead.
    stream_size: Option<(f64, f64)>,
    /// The desktop's scale factor, probed once at startup - see `warn_on_scale_mismatch`.
    display_scale: f64,
    /// So the mismatch warning is logged once per session, not once per touch.
    warned_mismatch: std::sync::atomic::AtomicBool,
    /// libei injection over `ConnectToEIS` - the normal path, with no `check_position` to fight.
    /// `None` when the portal/compositor can't do it, falling back to the D-Bus path below.
    ei: Option<EiTouch>,
}

impl RemoteDesktopStream {
    /// Inject a CarPlay touch. `accessory_size` is the accessory's display size (the space `event.x/.y` are in).
    ///
    /// Over libei the coordinates go straight into the device's region. On the D-Bus fallback
    /// they're scaled to `stream_size` (or `frame_size` if the portal gave no size) and clamped
    /// just inside it, since `check_position` is exclusive but CarPlay reports touch at the edge.
    pub async fn send_touch(
        &self,
        event: &TouchEvent,
        accessory_size: (u32, u32),
        frame_size: Option<(u32, u32)>,
    ) -> Result<()> {
        if let Some(ei) = &self.ei {
            return ei.send_touch(event, accessory_size);
        }
        let stream_id = self.node_id;
        let frame = frame_size.map(|(w, h)| (f64::from(w), f64::from(h)));
        self.warn_on_scale_mismatch(frame);
        let target = self.stream_size.or(frame);
        let (x, y) = clamp_inside(
            scale_touch(event.x, event.y, target, accessory_size),
            target,
        );
        debug!(
            "touch {:?} slot={} accessory=({:.0},{:.0}) accessory_size={accessory_size:?} target={target:?} \
             -> ({x:.0},{y:.0})",
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

    /// Lift any finger still held down, and report whether input is still usable. Called when a
    /// CarPlay session ends: the input session outlives individual accessory sessions, so an accessory that
    /// vanishes mid-gesture would otherwise leave the desktop with a touch stuck down.
    pub fn end_session(&self) {
        if let Some(ei) = &self.ei {
            if let Err(e) = ei.release_all() {
                tracing::warn!("libei release-all failed: {e}");
            }
        }
    }

    /// Whether touch injection is still working (libei only; the D-Bus path is stateless).
    #[cfg(test)]
    pub fn touch_is_healthy(&self) -> bool {
        self.ei
            .as_ref()
            .is_none_or(crate::libei::EiTouch::is_connected)
    }

    /// Warn (once) when the advertised stream size is the desktop's *logical* size while frames are
    /// physical - `advertised * display_scale == frame`. Mutter does this, and no D-Bus coordinate
    /// satisfies both its validation and its input transform - hence the libei path above; this
    /// warning is for any other compositor with the same mismatch.
    fn warn_on_scale_mismatch(&self, frame: Option<(f64, f64)>) {
        use std::sync::atomic::Ordering;
        let (Some((sw, _)), Some((fw, _))) = (self.stream_size, frame) else {
            return;
        };
        if (sw * self.display_scale - fw).abs() > 1.0 || self.display_scale <= 1.0 {
            return;
        }
        if !self.warned_mismatch.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "portal advertises a {sw}px-wide stream but delivers {fw}px frames \
                 (display scale {}): touch will only reach the top-left {:.0}% of the desktop. \
                 This is the portal/compositor coordinate-space mismatch carjack avoids on GNOME \
                 by driving Mutter directly.",
                self.display_scale,
                100.0 / self.display_scale
            );
        }
    }
}

/// Keep a coordinate strictly inside `target`, which `xdg-desktop-portal`'s `check_position`
/// requires (`0 <= x < width`): a touch at the very edge of the accessory's display scales to exactly
/// `width` and would be rejected with "Invalid position", killing the whole event.
fn clamp_inside((x, y): (f64, f64), target: Option<(f64, f64)>) -> (f64, f64) {
    match target {
        Some((w, h)) => (
            x.clamp(0.0, (w - 1.0).max(0.0)),
            y.clamp(0.0, (h - 1.0).max(0.0)),
        ),
        None => (x, y),
    }
}

/// Scale an accessory-space touch coordinate (`x`, `y` in the accessory's `accessory_size` display) to the desktop
/// stream's coordinate space. The desktop is scaled *down* to the accessory's resolution for streaming, so
/// touch has to be scaled back *up*. Passes through unchanged if the target size is unknown or the
/// accessory size is degenerate.
pub(crate) fn scale_touch(
    x: f64,
    y: f64,
    stream_size: Option<(f64, f64)>,
    accessory_size: (u32, u32),
) -> (f64, f64) {
    match stream_size {
        Some((sw, sh)) if accessory_size.0 > 0 && accessory_size.1 > 0 => (
            x * sw / f64::from(accessory_size.0),
            y * sh / f64::from(accessory_size.1),
        ),
        _ => (x, y),
    }
}

#[cfg(test)]
mod tests {
    use super::{TouchEvent, clamp_inside, scale_touch};

    /// Machine-gated probe: prints what this compositor reports through each scale source.
    #[test]
    fn display_scale_probe() {
        let Some(state) = super::probe_scale() else {
            eprintln!("no wayland display (no WAYLAND_DISPLAY) - skipping");
            return;
        };
        eprintln!(
            "wp_fractional_scale_v1: {:?} (/120), wl_output: {}, chosen: {:?}",
            state.fractional_scale,
            state.output_scale,
            super::display_scale()
        );
        if let Some(s) = super::display_scale() {
            assert!(s > 0.0);
        }
    }

    #[test]
    fn touch_scales_accessory_space_to_stream_space() {
        // 1080x600 accessory -> 1920x1080 desktop stream.
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
        // unknown stream size or degenerate accessory size -> pass through
        assert_eq!(scale_touch(500.0, 250.0, None, (1080, 600)), (500.0, 250.0));
        assert_eq!(scale_touch(500.0, 250.0, stream, (0, 0)), (500.0, 250.0));
    }

    /// Opens a real portal session and checks touch ends up on libei rather than the D-Bus
    /// fallback. Needs a human to accept the consent dialog, hence `#[ignore]`.
    /// `CARJACK_TEST_INJECT=1` also injects one tap at the centre of the screen.
    #[tokio::test]
    #[ignore = "opens a portal consent dialog, needs a human"]
    async fn portal_session_uses_libei() -> eyre::Result<()> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .try_init();
        let stream = match super::start_remote_desktop().await {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("no portal session ({e}) - skipping");
                return Ok(());
            }
        };
        assert!(
            stream.ei.is_some(),
            "touch fell back to the D-Bus path; libei should be available on portal API v2"
        );
        if std::env::var("CARJACK_TEST_INJECT").as_deref() == Ok("1") {
            inject_tap(&stream).await?;
        }
        Ok(())
    }

    /// Inject a real tap and confirm libei *accepted* it - a protocol violation makes the EIS
    /// implementation hang up, closing the command channel and failing the next send.
    async fn inject_tap(stream: &super::RemoteDesktopStream) -> eyre::Result<()> {
        use libcarjack::carplay_init::TouchEventType::{Down, Up};
        let accessory_size = (1080u32, 600u32);
        let tap = |event_type| TouchEvent {
            event_type,
            slot: 0,
            x: 540.0,
            y: 300.0,
        };
        stream.send_touch(&tap(Down), accessory_size, None).await?;
        stream.send_touch(&tap(Up), accessory_size, None).await?;
        // A duplicate Up here would be dropped by the slot tracker - but never send one on purpose:
        // unfiltered, it aborts Mutter (`g_assert` in release_slot) and takes the session with it.
        // Injection is queued to the EI thread; give it time to reach the compositor and for a
        // protocol error to come back if the events were malformed.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            stream.touch_is_healthy(),
            "the EI connection died after injecting a tap - the events were malformed"
        );
        eprintln!("injected a tap at the centre of the screen; libei connection still healthy");
        Ok(())
    }

    /// `check_position` is exclusive, so an edge touch - which CarPlay does report - has to be
    /// pulled just inside or the portal rejects it with "Invalid position".
    #[test]
    fn edge_touch_is_clamped_inside_the_stream() {
        let stream = Some((1920.0, 1080.0));
        assert_eq!(clamp_inside((1920.0, 1080.0), stream), (1919.0, 1079.0));
        // interior and origin untouched
        assert_eq!(clamp_inside((960.0, 540.0), stream), (960.0, 540.0));
        assert_eq!(clamp_inside((0.0, 0.0), stream), (0.0, 0.0));
        // overshoot (an accessory reporting beyond its own display) and negatives are pulled in
        assert_eq!(clamp_inside((5000.0, -3.0), stream), (1919.0, 0.0));
        // unknown target -> unchanged
        assert_eq!(clamp_inside((5000.0, 1.0), None), (5000.0, 1.0));
    }
}

/// Fail fast if xdg-desktop-portal isn't reachable, rather than deep into `run_desktop`. The
/// portal lives on the D-Bus session bus, which the systemd system service has none of.
pub async fn check_available() -> Result<()> {
    zbus::Connection::session().await.map(|_| ()).map_err(|e| {
        eyre!(
            "Could not reach a D-Bus session bus ({e}); desktop sharing needs a logged-in user \
             session, so `source = desktop` only works when run as a user, not as the systemd \
             service"
        )
    })
}

pub async fn start_remote_desktop() -> Result<RemoteDesktopStream> {
    let remote_desktop = RemoteDesktop::new().await?;
    let session = remote_desktop.create_session(Default::default()).await?;

    remote_desktop
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                // `Pointer` is requested even though only Touchscreen is injected: Mutter's EIS
                // refuses to create any viewport-backed device without a pointer capability, so
                // Keyboard|Touchscreen alone silently yields no touch device at all.
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer | DeviceType::Touchscreen)
                .set_persist_mode(PersistMode::Application),
        )
        .await?;

    Screencast::new()
        .await?
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_sources(SourceType::Monitor | SourceType::Virtual | SourceType::Window)
                .set_cursor_mode(Some(ashpd::desktop::screencast::CursorMode::Embedded)),
        )
        .await?;

    let response = remote_desktop
        .start(&session, None, Default::default())
        .await?
        .response()?;

    debug!(response = ?response, "Response");

    let streams = response.streams();
    let stream = streams
        .first()
        .ok_or(eyre!("No stream in remote desktop response!"))?;
    let node_id = stream.pipe_wire_node_id();
    let stream_size = stream.size().map(|(w, h)| (f64::from(w), f64::from(h)));

    // Touch goes over libei when the portal supports `ConnectToEIS` (API v2); `mapping_id` ties
    // the stream to the compositor's matching input region.
    let mapping_id = stream.mapping_id().map(str::to_string);
    let ei = match EiTouch::connect(&remote_desktop, &session, mapping_id).await {
        Ok(ei) => Some(ei),
        Err(e) => {
            tracing::warn!(
                "libei touch unavailable ({e}); falling back to the portal's NotifyTouch* D-Bus \
                 methods, whose coordinate validation misplaces touch on a scaled desktop"
            );
            None
        }
    };

    // Probed once, here, to spot the scale mismatch `warn_on_scale_mismatch` checks for. Runs on
    // the blocking pool since it does blocking Wayland roundtrips.
    let display_scale = tokio::task::spawn_blocking(display_scale)
        .await
        .ok()
        .flatten()
        .unwrap_or(1.0);
    tracing::info!(
        "Desktop stream node {node_id}, advertised size {:?}, display scale {display_scale}, \
         touch via {}",
        stream.size(),
        if ei.is_some() { "libei" } else { "D-Bus" }
    );

    Ok(RemoteDesktopStream {
        remote_desktop,
        session,
        node_id,
        stream_size,
        display_scale,
        warned_mismatch: std::sync::atomic::AtomicBool::new(false),
        ei,
    })
}
