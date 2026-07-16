use bytes::Bytes;
use pretty_hex::PrettyHex;
use std::time::Duration;

use crate::{
    error::Iap2Error,
    iap2::{Iap2, Stream},
};

use backon::ConstantBuilder;
use backon::Retryable;

//One-of-a-kind initial packet thats actually iAP1, so we just leave it raw
const REQUEST_IAP2_SWITCH: Bytes = Bytes::from_static(&[0xFF, 0x55, 0x02, 0x00, 0xEE, 0x10]);

///Send magic bytes to signify switch to iap2
pub struct Switch<'a, S>(pub &'a Iap2<S>);

impl<S: Stream> Switch<'_, S> {
    async fn perform_inner(&self) -> Result<(), Iap2Error> {
        let mut buf = [0u8; 6];
        let mut packet_stream = self.0.packet_stream.lock().await;
        let stream = packet_stream.get_mut();
        tracing::debug!("Waiting for iap2 switch command");
        stream.read_exact(&mut buf).await?;
        tracing::trace!(buf = ?buf.hex_dump(), "Read stream");
        if buf.as_slice() != REQUEST_IAP2_SWITCH.as_ref() {
            Err(Iap2Error::UnexpectedIAP2SwitchResponse(Bytes::copy_from_slice(&buf)))?;
        }
        tracing::debug!("Received iap2 switch command");
        stream.write_all(&REQUEST_IAP2_SWITCH).await?;
        Ok(())
    }

    /// Wait for iAP1 switch from accessory, then echo it back.
    pub async fn perform(&self) -> Result<(), Iap2Error> {
        (|| async { self.perform_inner().await })
            .retry(
                ConstantBuilder::new()
                    .with_delay(Duration::from_secs(1))
                    .with_max_times(10),
            )
            .await
    }
}
