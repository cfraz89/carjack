use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{
        DeviceType, NotifyTouchDownOptions, NotifyTouchMotionOptions, NotifyTouchUpOptions,
        RemoteDesktop, SelectDevicesOptions,
    },
    screencast::{Screencast, SelectSourcesOptions, SourceType, Stream},
};
use eyre::{Result, eyre};
use libcarjack::carplay_session::{TouchEvent, TouchEventType};
use tracing::debug;

pub struct RemoteDesktopStream {
    pub remote_desktop: RemoteDesktop,
    pub session: Session<RemoteDesktop>,
    pub stream: Stream,
    pub node_id: u32,
}

impl RemoteDesktopStream {
    pub async fn send_touch(&self, event: &TouchEvent) -> Result<()> {
        let stream_id = self.node_id;
        match event.event_type {
            TouchEventType::Down => {
                self.remote_desktop
                    .notify_touch_down(
                        &self.session,
                        stream_id,
                        event.slot,
                        event.x,
                        event.y,
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
                        event.x,
                        event.y,
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

    pub fn node_id(&self) -> u32 {
        self.node_id
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

    let stream = response
        .streams()
        .first()
        .ok_or(eyre!("No stream in remote desktop response!"))?
        .clone();

    let node_id = stream.pipe_wire_node_id();

    Ok(RemoteDesktopStream {
        remote_desktop,
        session,
        stream,
        node_id,
    })
}
