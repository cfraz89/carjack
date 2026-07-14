use bytes::{Buf, BytesMut};
use deku::{DekuContainerRead, DekuContainerWrite};
use pretty_hex::PrettyHex;
use std::io::{Error, ErrorKind};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::codec::{Decoder, Encoder, Framed};

use crate::config::LinkConfig;
use crate::error::Iap2Error;
use crate::ops::switch::Switch;
use crate::packet::{Checksumable, ControlByte, Header, Packet};

use futures::SinkExt;

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
    pub packet_stream: Mutex<Framed<T, Iap2Codec>>,
    seq: u8,
    ack: u8,
    pub config: LinkConfig,
}

pub trait Stream: AsyncWriteExt + AsyncReadExt + Unpin {}
impl<S: AsyncWriteExt + AsyncReadExt + Unpin> Stream for S {}

impl<S: Stream> Iap2<S> {
    /// Create an iap2 link, starting with an iap2 compatibility handshake with the receiving end.
    pub async fn start(stream: S, config: LinkConfig) -> Result<Self, Iap2Error> {
        let iap2 = Iap2 {
            packet_stream: Mutex::new(Framed::new(stream, Iap2Codec)),
            seq: config.initial_seq,
            ack: 0x00,
            config,
        };
        Switch(&iap2).perform().await?;
        Ok(iap2)
    }

    /// Construct a packet with lengths, payloads, and seq and ack values correctly filled in
    pub fn make_packet(&self, control: ControlByte, session_id: u8, payload: Vec<u8>) -> Packet {
        let header = Header {
            length: payload.len() as u16 + 10,
            control,
            seq: self.seq,
            ack: self.ack,
            session_id,
        };
        let header_checksum = header.calculate_checksum();
        let payload_checksum = if payload.len() > 0 {
            Some(payload.calculate_checksum())
        } else {
            None
        };
        Packet {
            header: header,
            header_checksum,
            payload,
            payload_checksum,
        }
    }

    /// Send a packet over the internal stream
    pub async fn send_packet(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
    ) -> Result<(), Iap2Error> {
        let packet = self.make_packet(control, session_id, payload);
        let mut packet_stream = self.packet_stream.lock().await;
        Ok(packet_stream.send(packet).await?)
    }
}
