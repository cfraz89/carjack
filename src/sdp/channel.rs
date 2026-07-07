use deku::DekuContainerRead;
use eyre::{Result, eyre};

use zbus::{self, Connection, proxy};

use crate::sdp::record::SdpRecord;

#[proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait BluezDevice1 {
    async fn get_service_records(&self) -> Result<Vec<Vec<u8>>>;
}

pub(crate) async fn get_rfcomm_channel(address: &bluer::Address) -> Result<u16> {
    let conn = Connection::system().await?;
    let device1 = BluezDevice1Proxy::builder(&conn)
        // .destination("org.bluez")?
        .path(format!(
            "/org/bluez/hci0/dev_{}",
            address
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join("_")
        ))?
        .build()
        .await?;
    let records = device1.get_service_records().await?;
    let sdp_records = records
        .into_iter()
        .filter_map(|r| SdpRecord::from_bytes((&r, 0)).ok().map(|(_, rec)| rec))
        .collect::<Vec<_>>();
    let channel = sdp_records
        .iter()
        .filter_map(|r| {
            if r.service_class_ids()
                .contains(&libcarjack::uuid::IAP_ACCESSORY_UUID)
            {
                r.rfcomm_channel()
            } else {
                None
            }
        })
        .next()
        .ok_or(eyre!("No RFCOMM channel found in service records"))?;
    Ok(channel)
}
