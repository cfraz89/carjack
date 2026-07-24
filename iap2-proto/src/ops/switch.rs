use bytes::Bytes;
use pretty_hex::PrettyHex;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::Iap2Error;

//One-of-a-kind initial packet thats actually iAP1, so we just leave it raw
const REQUEST_IAP2_SWITCH: Bytes = Bytes::from_static(&[0xFF, 0x55, 0x02, 0x00, 0xEE, 0x10]);

/// Wait for iAP1 switch from accessory, then echo it back.
///
/// Runs directly on the raw stream, before any framing/codec exists - this is a
/// one-of-a-kind raw exchange that happens once, prior to `Iap2::start` splitting the
/// stream into independent read/write halves.
pub async fn perform_switch<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut S,
) -> Result<(), Iap2Error> {
    const MAX_ATTEMPTS: u32 = 2;
    let mut attempt = 0;
    loop {
        match perform_switch_inner(stream).await {
            Ok(()) => return Ok(()),
            Err(e @ Iap2Error::IO(_)) => return Err(e),
            Err(e) if attempt < MAX_ATTEMPTS => {
                tracing::warn!(?e, attempt, "iAP2 switch attempt failed, retrying");
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn perform_switch_inner<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut S,
) -> Result<(), Iap2Error> {
    let mut buf = [0u8; 6];
    tracing::debug!("Waiting for iap2 switch command");
    // Bound the read: an accessory that connects but never drives the iAP1->iAP2 switch
    // (seen intermittently) would otherwise block here forever, since read_exact only
    // returns on data or EOF. A timeout lets the caller retry.
    tokio::time::timeout(Duration::from_secs(12), stream.read_exact(&mut buf))
        .await
        .map_err(|_| Iap2Error::Timeout(String::from("Timed out waiting for iAP2 switch command")))?
        .map_err(|e| match e.kind() {
            io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof => Iap2Error::IO(e),
            _ => e.into(),
        })?;
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
