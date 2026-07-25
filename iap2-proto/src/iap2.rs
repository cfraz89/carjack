use std::sync::atomic::{AtomicU8, Ordering};

use bytes::{Buf, BytesMut};
use deku::{DekuContainerRead, DekuContainerWrite};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::codec::{Decoder, Encoder, Framed};

use crate::error::Iap2Error;
use crate::ops::{switch::perform_switch, syn::Syn};
use crate::packet::{Checksumable, ControlByte, Header, Packet};
use crate::stream::SessionTable;

#[derive(Debug)]
pub struct Iap2Codec;

impl Decoder for Iap2Codec {
    type Item = Packet;
    type Error = Iap2Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Packet>, Iap2Error> {
        // Walk bytes one at a time looking for iAP2 magic (FF 5A)
        let mut pos = 0;
        while pos + 2 <= src.len() {
            if src[pos] == 0xFF && src[pos + 1] == 0x5A {
                break;
            }
            pos += 1;
        }
        if pos > 0 {
            tracing::warn!("Skipping {} byte(s) before valid magic", pos);
            src.advance(pos);
        }
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

pub struct Iap2<T> {
    // Split into independent read/write halves so a pending receive_packet() (which
    // spends nearly all its time blocked waiting for the accessory's next byte) never
    // blocks a concurrent send_packet() - needed for link-layer keep-alives to actually
    // go out while otherwise idle.
    sink: Mutex<SplitSink<Framed<T, Iap2Codec>, Packet>>,
    stream: Mutex<SplitStream<Framed<T, Iap2Codec>>>,
    seq: AtomicU8,
    ack: AtomicU8,
    pub session_table: Mutex<SessionTable>,
}

pub trait Stream: AsyncWriteExt + AsyncReadExt + Unpin {}
impl<S: AsyncWriteExt + AsyncReadExt + Unpin> Stream for S {}

//Yoinked from iap2-rs
const INITIAL_SEQ: u8 = 0x9D;

impl<S: Stream> Iap2<S> {
    pub async fn start(mut stream: S) -> Result<Self, Iap2Error> {
        perform_switch(&mut stream).await?;

        let framed = Framed::new(stream, Iap2Codec);
        let (sink, stream) = framed.split();

        let iap2 = Iap2 {
            sink: Mutex::new(sink),
            stream: Mutex::new(stream),
            seq: AtomicU8::new(INITIAL_SEQ),
            ack: AtomicU8::new(0),
            session_table: Mutex::new(SessionTable::new()),
        };
        let table = Syn(&iap2).perform().await?;
        *iap2.session_table.lock().await = table;
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
        let mut sink = self.sink.lock().await;
        sink.send(packet).await?;
        self.seq.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub async fn receive_packet(&self) -> Result<Packet, Iap2Error> {
        let mut stream = self.stream.lock().await;
        let packet = stream.next().await.ok_or(Iap2Error::NoPacket)??;
        self.ack.store(packet.header.seq, Ordering::Relaxed);
        Ok(packet)
    }
}
