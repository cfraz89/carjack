use ashpd::desktop::{
    PersistMode,
    remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions, StartOptions},
    screencast::{self, CursorMode, Screencast, SelectSourcesOptions, SourceType, Stream},
};
use eyre::{Result, eyre};
use tracing::{debug, info};

pub struct RemoteDesktopStream {
    pub remote_desktop: RemoteDesktop,
    pub stream: Stream,
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

    Ok(RemoteDesktopStream {
        remote_desktop,
        stream,
    })
}
