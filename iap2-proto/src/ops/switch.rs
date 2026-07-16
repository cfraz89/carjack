use bytes::{Bytes, BytesMut};
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
        let mut buf = BytesMut::with_capacity(512);
        let mut packet_stream = self.0.packet_stream.lock().await;
        let stream = packet_stream.get_mut();
        stream.write_all(&REQUEST_IAP2_SWITCH).await?;
        tracing::debug!("Wrote IAP2 switch packet");

        //Not sure if it is correct to allow a lack of response
        match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await {
            Ok(Ok(n)) => {
                tracing::trace!(buf = ?buf.hex_dump(), "Read stream");
                if n != 0 && buf != REQUEST_IAP2_SWITCH {
                    Err(Iap2Error::UnexpectedIAP2SwitchResponse(buf.freeze()))?;
                }
                tracing::debug!("Received iap2 switch response");
                Ok(())
            }
            Ok(Err(e)) => Err(e)?,
            Err(_) => {
                tracing::debug!("Timed out waiting for switch packet response. Continuing.");
                Ok(())
            }
        }
    }

    /// Write out iAP2 switch bytes, and attempt to read back the same bytes as response.
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
