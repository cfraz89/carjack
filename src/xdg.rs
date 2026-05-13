use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};
use eyre::Result;
use tracing::{debug, info};

pub async fn start_remote_desktop() -> Result<()> {
    let proxy = RemoteDesktop::new().await?;
    let session = proxy.create_session(Default::default()).await?;
    proxy
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer | DeviceType::Touchscreen),
        )
        .await?;
    debug!("Selected devices!");
    let response = proxy
        .start(&session, None, Default::default())
        .await?
        .response()?;
    debug!(response = ?response, "Response");
    Ok(())
}
