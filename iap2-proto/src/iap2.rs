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
use crate::packet::{ControlByte, Packet};
use crate::stream::{SessionTable, SessionType};

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

/// Session ID for packets that belong to the link layer itself rather than any
/// negotiated session - bare ACKs and keep-alives. An iPhone uses 0 for these.
pub const LINK_SESSION_ID: u8 = 0;

impl<S: Stream> Iap2<S> {
    /// The control session ID the *accessory* advertised in its SYN. Never assume a
    /// value here: the CFMOTO head unit numbers its control session 2, and messages
    /// sent on any other ID are link-ACKed and then silently dropped.
    pub async fn control_session_id(&self) -> Result<u8, Iap2Error> {
        self.session_table
            .lock()
            .await
            .find_type(SessionType::Control)
            .map(|s| s.id)
            .ok_or(Iap2Error::ControlSessionError(
                "No control session in session table",
            ))
    }

    /// Wrap a stream in the codec without performing the iAP1 switch or the SYN handshake.
    fn wrap(stream: S) -> Self {
        let framed = Framed::new(stream, Iap2Codec);
        let (sink, stream) = framed.split();
        Iap2 {
            sink: Mutex::new(sink),
            stream: Mutex::new(stream),
            seq: AtomicU8::new(INITIAL_SEQ),
            ack: AtomicU8::new(0),
            session_table: Mutex::new(SessionTable::new()),
        }
    }

    pub async fn start(mut stream: S) -> Result<Self, Iap2Error> {
        perform_switch(&mut stream).await?;

        let iap2 = Iap2::wrap(stream);
        let table = Syn(&iap2).perform().await?;
        *iap2.session_table.lock().await = table;
        Ok(iap2)
    }

    pub fn make_packet(&self, control: ControlByte, session_id: u8, payload: Vec<u8>) -> Packet {
        // `self.seq` is the number the *next* payload-bearing packet will use. A packet with
        // no payload is a pure acknowledgement, and repeats the last one instead - see
        // `send_packet` for why that matters.
        let next_seq = self.seq.load(Ordering::Relaxed);
        let seq = if payload.is_empty() {
            next_seq.wrapping_sub(1)
        } else {
            next_seq
        };
        Packet::new(
            control,
            seq,
            self.ack.load(Ordering::Relaxed),
            session_id,
            payload,
        )
    }

    /// Send a packet, consuming a sequence number only if it carries a payload.
    ///
    /// Pure acknowledgements do not advance the sequence: both an iPhone and the accessory
    /// repeat their last data packet's number on them (`iphone-cfmoto.pklg` - the phone sends
    /// data as seq `0x47` and then acknowledges two separate messages with seq `0x47` again;
    /// the car does the same with `0x78` before moving to `0x79`).
    ///
    /// Burning a sequence number per acknowledgement leaves a gap the accessory never sees
    /// filled, so it stops accepting everything we send afterwards: it freezes its cumulative
    /// ack, retransmits its last message until it gives up, and tears down the link. That is
    /// exactly what `cfmoto_9.pcapng` shows - three 1 s keepalives take us from seq `0xa7` to
    /// `0xa9`, and the car ignores every packet from there on, including the `0xaa` that
    /// follows, while re-sending its WiFi configuration nine times.
    pub async fn send_packet(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
    ) -> Result<(), Iap2Error> {
        let consumes_seq = !payload.is_empty();
        let packet = self.make_packet(control, session_id, payload);
        let mut sink = self.sink.lock().await;
        sink.send(packet).await?;
        if consumes_seq {
            self.seq.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    pub async fn receive_packet(&self) -> Result<Packet, Iap2Error> {
        let mut stream = self.stream.lock().await;
        let packet = stream.next().await.ok_or(Iap2Error::NoPacket)??;
        self.ack.store(packet.header.seq, Ordering::Relaxed);
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn control_payload() -> Vec<u8> {
        vec![0x40, 0x40, 0x00, 0x06, 0xaa, 0x05]
    }

    /// Read every packet the peer received and report `(seq, payload_len)` for each.
    async fn drain(peer: &mut tokio::io::DuplexStream) -> Vec<(u8, usize)> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 512];
        let n = peer.read(&mut chunk).await.unwrap();
        buf.extend_from_slice(&chunk[..n]);

        let mut packets = Vec::new();
        let mut rest = buf.as_slice();
        while !rest.is_empty() {
            let ((tail, _), packet) = Packet::from_bytes((rest, 0)).unwrap();
            packets.push((packet.header.seq, packet.payload.len()));
            rest = tail;
        }
        packets
    }

    /// Acknowledgements repeat the last data packet's sequence number rather than taking one
    /// of their own. Getting this wrong deadlocks the link: the accessory sees a gap it can
    /// never fill and ignores everything we send afterwards.
    #[tokio::test]
    async fn pure_acks_do_not_consume_a_sequence_number() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        // The shape of the tail of a real session: a message, an idle spell covered by
        // keepalives, then another message.
        iap2.send_packet(ControlByte::with_ack(), 2, control_payload())
            .await
            .unwrap();
        for _ in 0..3 {
            iap2.send_packet(ControlByte::with_ack(), LINK_SESSION_ID, Vec::new())
                .await
                .unwrap();
        }
        iap2.send_packet(ControlByte::with_ack(), 2, control_payload())
            .await
            .unwrap();

        assert_eq!(
            drain(&mut theirs).await,
            vec![
                (INITIAL_SEQ, 6),
                (INITIAL_SEQ, 0),
                (INITIAL_SEQ, 0),
                (INITIAL_SEQ, 0),
                (INITIAL_SEQ.wrapping_add(1), 6),
            ]
        );
    }

    /// Consecutive messages still advance one at a time.
    #[tokio::test]
    async fn payload_packets_advance_the_sequence() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        for _ in 0..3 {
            iap2.send_packet(ControlByte::with_ack(), 2, control_payload())
                .await
                .unwrap();
        }

        let seqs: Vec<u8> = drain(&mut theirs).await.into_iter().map(|p| p.0).collect();
        assert_eq!(
            seqs,
            vec![
                INITIAL_SEQ,
                INITIAL_SEQ.wrapping_add(1),
                INITIAL_SEQ.wrapping_add(2)
            ]
        );
    }
}
