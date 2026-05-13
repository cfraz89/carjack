use ashpd::desktop::{
    PersistMode,
    remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions},
    screencast::{self, CursorMode, Screencast, SelectSourcesOptions, SourceType},
};
use eyre::Result;
use tracing::{debug, info};

pub async fn start_remote_desktop() -> Result<()> {
    let remote_desktop = RemoteDesktop::new().await?;
    let screencast = Screencast::new().await?;
    let session = remote_desktop.create_session(Default::default()).await?;

    remote_desktop
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Touchscreen)
                .set_persist_mode(PersistMode::Application),
        )
        .await?;

    screencast
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

    remote_desktop.Ok(())
}
