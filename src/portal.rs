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
    /// coords are scaled to the desktop stream's size, which is what the portal expects.
    pub async fn send_touch(&self, event: &TouchEvent, car_size: (u32, u32)) -> Result<()> {
        let stream_id = self.node_id;
        let (x, y) = self.scale_to_stream(event.x, event.y, car_size);
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
                    .notify_touch_up(
                        &self.session,
                        event.slot,
                        NotifyTouchUpOptions::default(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Map a car-space touch coordinate to the desktop stream's pixel space.
    fn scale_to_stream(&self, x: f64, y: f64, car_size: (u32, u32)) -> (f64, f64) {
        scale_touch(x, y, self.stream_size, car_size)
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }
}

/// Scale a car-space touch coordinate (`x`, `y` in the car's `car_size` display) to the desktop
/// stream's pixel space. The desktop is scaled *down* to the car's resolution for streaming, so
/// touch has to be scaled back *up*. Passes through unchanged if the stream size is unknown or the
/// car size is degenerate.
fn scale_touch(x: f64, y: f64, stream_size: Option<(f64, f64)>, car_size: (u32, u32)) -> (f64, f64) {
    match stream_size {
        Some((sw, sh)) if car_size.0 > 0 && car_size.1 > 0 => {
            (x * sw / f64::from(car_size.0), y * sh / f64::from(car_size.1))
        }
        _ => (x, y),
    }
}

#[cfg(test)]
mod tests {
    use super::scale_touch;

    #[test]
    fn touch_scales_car_space_to_stream_space() {
        // 1080x600 car -> 1920x1080 desktop stream.
        let stream = Some((1920.0, 1080.0));
        // centre stays centre
        assert_eq!(scale_touch(540.0, 300.0, stream, (1080, 600)), (960.0, 540.0));
        // corners map to the stream's corners
        assert_eq!(scale_touch(0.0, 0.0, stream, (1080, 600)), (0.0, 0.0));
        assert_eq!(scale_touch(1080.0, 600.0, stream, (1080, 600)), (1920.0, 1080.0));
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
