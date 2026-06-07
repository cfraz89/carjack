use std::time::Duration;

use bluer::{
    Session, Uuid,
    adv::Advertisement,
    monitor::{
        Monitor, Pattern,
        data_type::{
            COMPLETE_LIST_128_BIT_SERVICE_CLASS_UUIDS, INCOMPLETE_LIST_128_BIT_SERVICE_CLASS_UUIDS,
        },
    },
};
use eyre::Result;
use tokio::time::sleep;

const DEVICE_UUID: Uuid = UUid::from_u128(0x2D8D2466E14D451C88BC7301ABEA291A);
const ACCESSORY_UUID: Uuid = Uuid::from_u128(0xEC884348CD4140A29727575D50BF1FD3);

pub async fn wireless_carplay() -> Result<()> {
    let session = Session::new().await?;
    let adapter = session.default_adapter().await?;
    let advertisement_handle = adapter
        .advertise(Advertisement {
            service_uuids: DEVICE_UUID.into(),
            ..Default::default()
        })
        .await?;
    let monitor_handle = adapter
        .monitor()
        .await?
        .register(Monitor {
            patterns: Some(vec![
                Pattern::new(
                    INCOMPLETE_LIST_128_BIT_SERVICE_CLASS_UUIDS,
                    0,
                    ACCESSORY_UUID.into(),
                ),
                Pattern::new(
                    COMPLETE_LIST_128_BIT_SERVICE_CLASS_UUIDS,
                    0,
                    ACCESSORY_UUID.into(),
                ),
            ]),
            ..Default::default()
        })
        .await?;
    tracing::info!("Advertising and monitoring");
    sleep(Duration::from_hours(2)).await?;
    Ok(())
}
