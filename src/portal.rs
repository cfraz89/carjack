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
    use super::scale_touch;

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

pub async fn start_remote_desktop() -> Result<RemoteDesktopStream> {
    let remote_desktop = RemoteDesktop::new().await?;
    let session = remote_desktop.create_session(Default::default()).await?;

    remote_desktop
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Touchscreen)
                .set_persist_mode(PersistMode::Application),
        )
        .await?;

    Screencast::new()
        .await?
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_sources(SourceType::Monitor | SourceType::Virtual | SourceType::Window),
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
