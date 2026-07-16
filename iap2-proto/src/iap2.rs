use std::sync::atomic::{AtomicU8, Ordering};

use bytes::{Buf, BytesMut};
use deku::{DekuContainerRead, DekuContainerWrite};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::codec::{Decoder, Encoder, Framed};

use crate::config::LinkConfig;
use crate::error::Iap2Error;
use crate::ops::{switch::Switch, syn::Syn};
use crate::packet::{Checksumable, ControlByte, Header, Packet};
use crate::stream::SessionTable;

#[derive(Debug)]
pub struct Iap2Codec;

impl Decoder for Iap2Codec {
    type Item = Packet;
    type Error = Iap2Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Packet>, Iap2Error> {
        if src.len() < 9 {
            return Ok(None);
        }
        match Packet::from_bytes((&src[..], 0)) {
            Ok((rest, packet)) => {
                let consumed = src.len() - rest.0.len();
                src.advance(consumed);
                Ok(Some(packet))
            }
            Err(deku::DekuError::Incomplete(_)) => Ok(None),
            Err(e) => Err(Iap2Error::PacketDecoding(src.clone().freeze(), e)),
        }
    }
}

impl Encoder<Packet> for Iap2Codec {
    type Error = Iap2Error;

    fn encode(&mut self, packet: Packet, dst: &mut BytesMut) -> Result<(), Iap2Error> {
        let bytes = packet
            .to_bytes()
            .map_err(|err| Iap2Error::PacketEncoding(packet, err))?;
        dst.extend_from_slice(&bytes);
        Ok(())
    }
}

#[derive(Debug)]
pub struct Iap2<T> {
    pub packet_stream: Mutex<Framed<T, Iap2Codec>>,
    seq: AtomicU8,
    ack: AtomicU8,
    pub session_table: Mutex<SessionTable>,
    pub config: LinkConfig,
}

pub trait Stream: AsyncWriteExt + AsyncReadExt + Unpin {}
impl<S: AsyncWriteExt + AsyncReadExt + Unpin> Stream for S {}

impl<S: Stream> Iap2<S> {
    pub async fn start(stream: S, config: LinkConfig) -> Result<Self, Iap2Error> {
        let iap2 = Iap2 {
            packet_stream: Mutex::new(Framed::new(stream, Iap2Codec)),
            seq: AtomicU8::new(config.initial_seq),
            ack: AtomicU8::new(0),
            session_table: Mutex::new(SessionTable::new()),
            config,
        };
        iap2.establish().await?;
        Ok(iap2)
    }

    pub fn make_packet(&self, control: ControlByte, session_id: u8, payload: Vec<u8>) -> Packet {
        let header = Header {
            length: payload.len() as u16 + 10,
            control,
            seq: self.seq.load(Ordering::Relaxed),
            ack: self.ack.load(Ordering::Relaxed),
            session_id,
        };
        let header_checksum = header.calculate_checksum();
        let payload_checksum = if !payload.is_empty() {
            Some(payload.calculate_checksum())
        } else {
            None
        };
        Packet {
            header,
            header_checksum,
            payload,
            payload_checksum,
        }
    }

    pub async fn send_packet(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
    ) -> Result<(), Iap2Error> {
        let packet = self.make_packet(control, session_id, payload);
        let mut packet_stream = self.packet_stream.lock().await;
        packet_stream.send(packet).await?;
        self.seq.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub async fn receive_packet(&self) -> Result<Packet, Iap2Error> {
        let mut stream = self.packet_stream.lock().await;
        let packet = stream.next().await.ok_or(Iap2Error::NoPacket)??;
        self.ack.store(packet.header.seq, Ordering::Relaxed);
        Ok(packet)
    }

    /// Perform the iAP2 link establishment handshake:
    /// 1. iAP1→iAP2 protocol switch
    /// 2. SYN exchange
    /// Populates the session table.
    pub async fn establish(&self) -> Result<(), Iap2Error> {
        Switch(self).perform().await?;
        let table = Syn(self).perform().await?;
        *self.session_table.lock().await = table;
        Ok(())
    }
}
