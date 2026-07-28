use bytes::Bytes;
use pretty_hex::PrettyHex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::Iap2Error;

//One-of-a-kind initial packet thats actually iAP1, so we just leave it raw
const REQUEST_IAP2_SWITCH: Bytes = Bytes::from_static(&[0xFF, 0x55, 0x02, 0x00, 0xEE, 0x10]);

/// How long to wait for the switch before abandoning the channel as the wrong one.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(3);

/// Wait for the iAP1 switch from the accessory and echo it back.
///
/// Runs on the raw stream, before any framing exists.
pub async fn perform_switch<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut S,
) -> Result<(), Iap2Error> {
    let mut buf = [0u8; 6];
    tracing::debug!("Waiting for iap2 switch command");
    // read_exact only returns on data or EOF, so bound it.
    tokio::time::timeout(SWITCH_TIMEOUT, stream.read_exact(&mut buf))
        .await
        .map_err(|_| {
            Iap2Error::Timeout(String::from("Timed out waiting for iAP2 switch command"))
        })??;
    tracing::trace!(buf = ?buf.hex_dump(), "Read stream");
    if buf.as_slice() != REQUEST_IAP2_SWITCH.as_ref() {
        Err(Iap2Error::UnexpectedBytes(
            String::from("Unexpected switch response"),
            Bytes::copy_from_slice(&buf),
        ))?;
    }
    tracing::debug!("Received iap2 switch command");
    stream.write_all(&REQUEST_IAP2_SWITCH).await?;
    tracing::debug!("Sent iap2 switch response");
    Ok(())
}
