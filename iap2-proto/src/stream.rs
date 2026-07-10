use bytes::{Buf, BytesMut};
use deku::{DekuContainerRead, DekuContainerWrite};
use pretty_hex::PrettyHex;
use std::io::{Error, ErrorKind};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::codec::{Decoder, Encoder, Framed};

use crate::config::LinkConfig;
use crate::error::Iap2Error;
use crate::packet::{Header, Packet};

#[derive(Debug)]
pub struct Iap2Codec;

impl Decoder for Iap2Codec {
    type Item = Packet;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Packet>, Error> {
        match Packet::from_bytes((&src[..], 0)) {
            Ok((rest, packet)) => {
                let consumed = src.len() - rest.0.len();
                src.advance(consumed);
                Ok(Some(packet))
            }
            Err(_) => Ok(None),
        }
    }
}

impl Encoder<Packet> for Iap2Codec {
    type Error = Error;

    fn encode(&mut self, packet: Packet, dst: &mut BytesMut) -> Result<(), Error> {
        let bytes = packet
            .to_bytes()
            .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;
        dst.extend_from_slice(&bytes);
        Ok(())
    }
}

/// Models iAP2 protocol, manages read and write network streams, providing a `Packet` stream and sink.
#[derive(Debug)]
pub struct Iap2<T> {
    pub framed: Framed<T, Iap2Codec>,
    seq: u8,
    ack: u8,
    pub config: LinkConfig,
}

impl<T> Iap2<T>
where
    T: AsyncReadExt + AsyncWriteExt + Unpin,
{
    /// Create an iap2 link, starting with an iap2 compatibility handshake with the receiving end.
    pub async fn start(stream: T, config: LinkConfig) -> Result<Self, Iap2Error> {
        let mut stream = stream;
        request_iap2_switch(&mut stream, &config).await?;
        Ok(Iap2 {
            framed: Framed::new(stream, Iap2Codec),
            seq: config.initial_seq,
            ack: 0x00,
            config,
        })
    }

    pub async fn reset(&mut self) -> Result<(), Iap2Error> {
        use futures::SinkExt;
        Ok(self
            .framed
            .send(Packet {
                header: Header {
                    length: 0,
                    control: crate::packet::ControlByte {
                        rst: true,
                        ..Default::default()
                    },
                    seq: self.seq,
                    ack: self.ack,
                    session_id: 0,
                },
                header_checksum: 0,
                payload: Vec::new(),
                payload_checksum: None,
            })
            .await?)
    }
}

/// Write out iAP2 switch bytes, and attempt to read back the same bytes as response.
async fn request_iap2_switch<T: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut T,
    config: &LinkConfig,
) -> Result<(), Iap2Error> {
    let mut buf = BytesMut::with_capacity(6);
    //Keep trying to send iap2 switch packet until we receive matching response, or timeout
    let deadline = tokio::time::Instant::now() + config.iap2_switch_timeout;

    loop {
        match stream.write_all(&Packet::REQUEST_IAP2_SWITCH).await {
            Ok(()) => tracing::debug!("Wrote IAP2 switch packet"),
            Err(e) => {
                tracing::debug!(?e, "Write failed, retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        }

        match tokio::time::timeout_at(deadline, stream.read_exact(&mut buf)).await {
            Ok(Ok(n)) => {
                tracing::debug!(buf = ?buf.hex_dump(), "Read stream");
                if n == 0 {
                    return Err(Error::new(
                        ErrorKind::ConnectionAborted,
                        "Stream closed by remote side",
                    )
                    .into());
                }
                if buf != Packet::REQUEST_IAP2_SWITCH {
                    return Err(Iap2Error::UnexpectedIAP2SwitchResponse(buf.freeze()));
                }
                return Ok(());
            }
            Ok(Err(e)) => {
                tracing::debug!(?e, "Read failed, retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(_) => {
                return Err(Iap2Error::Timeout("iAP2 switch".to_string()));
            }
        }
    }
}
