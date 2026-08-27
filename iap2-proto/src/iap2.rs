use std::sync::atomic::{AtomicU8, Ordering};

use bytes::{Buf, BytesMut};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::codec::{Decoder, Encoder, Framed};

use std::collections::VecDeque;

use crate::error::Iap2Error;
use crate::link::{LinkAction, LinkState};
use crate::ops::syn::Syn;
use crate::packet::{ControlByte, Frame, Packet};
use crate::stream::{SessionTable, SessionType};

#[derive(Debug)]
pub struct Iap2Codec;

impl Decoder for Iap2Codec {
    type Item = Frame;
    type Error = Iap2Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, Iap2Error> {
        let mut pos = 0;
        while pos + 2 <= src.len() {
            if src[pos] == 0xFF && (src[pos + 1] == 0x5A || src[pos + 1] == 0x55) {
                break;
            }
            pos += 1;
        }
        if pos > 0 {
            let skipped = src[..pos]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            tracing::warn!("Skipping {pos} byte(s) before valid magic: {skipped}");
            src.advance(pos);
        }
        match Frame::read(&src[..]) {
            Ok((frame, consumed)) => {
                src.advance(consumed);
                Ok(Some(frame))
            }
            Err(deku::DekuError::Incomplete(_)) => Ok(None),
            Err(e) => Err(Iap2Error::PacketDecoding(src.clone().freeze(), e)),
        }
    }
}

impl Encoder<Frame> for Iap2Codec {
    type Error = Iap2Error;

    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> Result<(), Iap2Error> {
        let bytes = frame
            .to_bytes()
            .map_err(|err| Iap2Error::FrameEncoding(frame, err))?;
        dst.extend_from_slice(&bytes);
        Ok(())
    }
}

/// A packet waiting for the send window to open. Held as its parts rather than a built `Packet`
/// so the `ack` field is filled in at transmit time and carries the freshest value.
struct Queued {
    control: ControlByte,
    session_id: u8,
    payload: Vec<u8>,
}

pub struct Iap2<T> {
    // Independent halves, so a receive_packet() blocked on the accessory's next byte doesn't
    // block a concurrent send_packet() - link-layer keep-alives have to go out while idle.
    sink: Mutex<SplitSink<Framed<T, Iap2Codec>, Frame>>,
    stream: Mutex<SplitStream<Framed<T, Iap2Codec>>>,
    /// Whether anything at all has arrived on this transport - see [`Iap2::heard_from_peer`].
    heard_from_peer: std::sync::atomic::AtomicBool,
    seq: AtomicU8,
    /// Negotiated parameters plus the send window, retransmit queue and cumulative-ack timers.
    pub link: Mutex<LinkState>,
    /// Packets that arrived while the send window was full, oldest first.
    tx_queue: Mutex<VecDeque<Queued>>,
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
    /// The control session ID the *accessory* advertised in its SYN. Never assume a value: head
    /// units number theirs differently, and messages on any other ID are link-ACKed then dropped.
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

    /// Wrap a stream in the codec without performing the SYN handshake.
    fn wrap(stream: S) -> Self {
        let framed = Framed::new(stream, Iap2Codec);
        let (sink, stream) = framed.split();
        Iap2 {
            sink: Mutex::new(sink),
            stream: Mutex::new(stream),
            heard_from_peer: std::sync::atomic::AtomicBool::new(false),
            seq: AtomicU8::new(INITIAL_SEQ),
            link: Mutex::new(LinkState::new()),
            tx_queue: Mutex::new(VecDeque::new()),
            session_table: Mutex::new(SessionTable::new()),
        }
    }

    /// Bring the link up on a transport the accessory has just opened. An accessory may open with
    /// the iAP1 detect, which [`Iap2::receive_raw`] answers wherever it appears; the link itself
    /// starts at the SYN.
    pub async fn start(stream: S) -> Result<Self, Iap2Error> {
        let iap2 = Iap2::wrap(stream);
        let table = Syn(&iap2).perform().await?;
        *iap2.session_table.lock().await = table;
        Ok(iap2)
    }

    /// Whether the accessory has sent anything at all here. A transport that stays silent is the
    /// wrong one - one of the channels BlueZ offered with no iAP session behind it.
    pub(crate) fn heard_from_peer(&self) -> bool {
        self.heard_from_peer.load(Ordering::Relaxed)
    }

    pub async fn make_packet(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
    ) -> Packet {
        let ack = self.link.lock().await.ack_field();
        self.make_packet_with_ack(control, session_id, payload, ack)
    }

    fn make_packet_with_ack(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
        ack: u8,
    ) -> Packet {
        // `self.seq` is the number the *next* payload-bearing packet will use; a pure
        // acknowledgement repeats the last one instead - see `send_packet` for why.
        let next_seq = self.seq.load(Ordering::Relaxed);
        let seq = if payload.is_empty() {
            next_seq.wrapping_sub(1)
        } else {
            next_seq
        };
        Packet::new(control, seq, ack, session_id, payload)
    }

    /// Queue or send a packet, consuming a sequence number only if it carries a payload.
    ///
    /// Pure acknowledgements do not advance the sequence - both an iPhone and the accessory repeat
    /// their last data packet's number on them. Burning a sequence number per acknowledgement
    /// leaves a gap the accessory never sees filled, so it freezes its cumulative ack, retransmits
    /// until it gives up, and tears the link down.
    ///
    /// **This never blocks on the send window.** The session dispatches its replies from inside
    /// its own receive loop, so a `send_packet` that waited for an acknowledgement would stop that
    /// loop from ever reading the acknowledgement it is waiting for - a guaranteed deadlock at the
    /// `max_outstanding = 1` some accessories negotiate. A packet that does not fit is queued
    /// instead and goes out from `maintain` as soon as an acknowledgement frees a slot.
    pub async fn send_packet(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
    ) -> Result<(), Iap2Error> {
        self.check_length(&payload).await?;

        // Bare acknowledgements and link control (SYN, RST) take no sequence number and are not
        // subject to the window - the SYN exchange completes *before* a window is negotiated.
        let bypasses_window = payload.is_empty() || control.syn || control.rst;
        if !bypasses_window && self.link.lock().await.window_is_full() {
            tracing::debug!(
                session_id,
                bytes = payload.len(),
                "Send window full; queueing packet"
            );
            self.tx_queue.lock().await.push_back(Queued {
                control,
                session_id,
                payload,
            });
            return Ok(());
        }

        self.transmit(control, session_id, payload).await
    }

    /// Put a packet on the wire now, updating the sequence number and link bookkeeping.
    async fn transmit(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
    ) -> Result<(), Iap2Error> {
        let consumes_seq = !payload.is_empty();
        let ack = self.link.lock().await.ack_field();
        let packet = self.make_packet_with_ack(control, session_id, payload.clone(), ack);
        {
            let mut sink = self.sink.lock().await;
            sink.send(Frame::Iap2(packet.clone())).await?;
        }
        if consumes_seq {
            self.seq.fetch_add(1, Ordering::Relaxed);
        }
        self.link.lock().await.record_sent(&packet, payload);
        Ok(())
    }

    /// Reject anything larger than the accessory said it could receive - sending it would overrun
    /// its buffer and cost us the link.
    async fn check_length(&self, payload: &[u8]) -> Result<(), Iap2Error> {
        let max = self.link.lock().await.params.max_length;
        let len = Packet::wire_len(payload.len());
        if len > usize::from(max) {
            return Err(Iap2Error::PacketTooLarge { len, max });
        }
        Ok(())
    }

    /// Send whatever the newly-freed window allows. Returns how many packets went out.
    async fn drain_tx_queue(&self) -> Result<usize, Iap2Error> {
        let mut sent = 0;
        loop {
            if self.link.lock().await.window_is_full() {
                break;
            }
            let Some(queued) = self.tx_queue.lock().await.pop_front() else {
                break;
            };
            self.transmit(queued.control, queued.session_id, queued.payload)
                .await?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Send a bare acknowledgement. Doubles as the idle keepalive - the accessory drops the
    /// Bluetooth link if it hears nothing for `retx_timeout * max_retx`.
    pub async fn send_ack(&self) -> Result<(), Iap2Error> {
        self.transmit(ControlByte::with_ack(), LINK_SESSION_ID, Vec::new())
            .await
    }

    /// Tell the accessory the link is finished, so it can start a fresh SYN straight away instead
    /// of waiting out its own retransmit timeout. Best-effort - the transport is often already
    /// gone by the time this goes out.
    pub async fn send_reset(&self) -> Result<(), Iap2Error> {
        let control = ControlByte {
            rst: true,
            ..Default::default()
        };
        self.transmit(control, LINK_SESSION_ID, Vec::new()).await
    }

    /// Sleep until the link needs attention. It only waits, so it is safe to cancel - which is what
    /// lets it sit in a `select!` while `maintain` runs outside one.
    pub async fn wait_for_link_work(&self) {
        if !self.tx_queue.lock().await.is_empty() && !self.link.lock().await.window_is_full() {
            return;
        }
        let deadline = self.link.lock().await.next_deadline();
        tokio::time::sleep_until(deadline).await;
    }

    /// Run once `wait_for_link_work` says there is something to do. **Do not call this inside a
    /// `select!`**: it transmits, and being cancelled part-way through would lose or duplicate a
    /// packet.
    pub async fn maintain(&self) -> Result<(), Iap2Error> {
        if self.drain_tx_queue().await? > 0 {
            return Ok(());
        }
        let action = self.link.lock().await.next_action();
        match action {
            LinkAction::Idle(_) => Ok(()),
            LinkAction::SendAck => self.send_ack().await,
            LinkAction::Retransmit(unacked) => {
                tracing::warn!(
                    seq = format_args!("{:#04x}", unacked.seq),
                    retx = unacked.retx,
                    "Retransmitting unacknowledged packet"
                );
                let ack = self.link.lock().await.ack_field();
                let packet = Packet::new(
                    unacked.control,
                    unacked.seq,
                    ack,
                    unacked.session_id,
                    unacked.payload.clone(),
                );
                self.sink.lock().await.send(Frame::Iap2(packet)).await?;
                self.link.lock().await.record_retransmit(unacked.seq);
                Ok(())
            }
            LinkAction::Dead { seq, retx } => Err(Iap2Error::RetransmitsExhausted { seq, retx }),
        }
    }

    /// Whether keepalives should go out. Off during identification/auth, where interleaving an
    /// out-of-band packet into the exchange confuses some accessories.
    pub async fn set_keepalive_enabled(&self, enabled: bool) {
        self.link.lock().await.keepalive_enabled = enabled;
    }

    /// Read the next packet off the wire and apply link-layer bookkeeping, returning **every**
    /// packet - bare acknowledgements and SYNs included, because the SYN handshake needs them: its
    /// third leg *is* a bare acknowledgement, and the SYN it answers carries the sequence number
    /// our SYN+ACK has to acknowledge. The session uses [`Iap2::receive_packet`] instead.
    ///
    /// Returns the packet and whether it was new (`false` = a retransmission of a sequence
    /// number we have already taken).
    pub(crate) async fn receive_raw(&self) -> Result<(Packet, bool), Iap2Error> {
        let packet = loop {
            let frame = {
                let mut stream = self.stream.lock().await;
                stream.next().await.ok_or(Iap2Error::NoPacket)??
            };
            self.heard_from_peer.store(true, Ordering::Relaxed);
            match frame {
                Frame::Iap2(packet) => break packet,
                // Answered here, not at bring-up: the accessory detects at the start of an RFCOMM
                // channel and again on the AirPlay tunnel when the session moves there.
                Frame::Detect(detect) => {
                    tracing::debug!("Echoing iAP1 detect");
                    self.sink.lock().await.send(Frame::Detect(detect)).await?;
                }
            }
        };
        let control = packet.header.control;

        if control.rst {
            tracing::warn!("Accessory reset the link");
            return Err(Iap2Error::LinkReset);
        }
        if control.sleep {
            // We have no power state to enter, and the accessory carries on talking regardless.
            tracing::info!("Accessory signalled device sleep");
        }

        // Every packet carries a cumulative acknowledgement, data or not.
        if control.ack {
            let retired = self.link.lock().await.retire_acked(packet.header.ack);
            if retired > 0 {
                tracing::trace!(retired, "Acknowledged in-flight packets");
            }
        }
        // An extended ack additionally lists sequence numbers received out of order, which
        // the cumulative field cannot describe.
        if control.eak && !packet.payload.is_empty() {
            let retired = self.link.lock().await.retire_eak(&packet.payload);
            tracing::debug!(retired, "Extended ack retired out-of-order packets");
        }
        // An acknowledgement may have opened the window for something we had queued.
        self.drain_tx_queue().await?;

        // Anything carrying a payload advances the receive sequence, and that **includes the
        // SYN** - it is a numbered packet like any other, and its number is what our SYN+ACK has
        // to acknowledge. Skip it and the SYN+ACK claims `ack = 0`, so the accessory never sees
        // its SYN acknowledged and retransmits until it gives up.
        let is_new = if packet.payload.is_empty() || (control.eak && !control.syn) {
            true
        } else {
            self.link.lock().await.accept_rx(packet.header.seq)
        };

        Ok((packet, is_new))
    }

    /// Receive the next packet the session should act on.
    ///
    /// Everything the link layer handles for itself is absorbed here: bare acknowledgements,
    /// extended acks, and a repeat of a sequence number we have already taken (re-acknowledged
    /// but not returned twice, or the session would act on the same message again).
    pub async fn receive_packet(&self) -> Result<Packet, Iap2Error> {
        loop {
            let (packet, is_new) = self.receive_raw().await?;
            let control = packet.header.control;

            // A SYN mid-session means the accessory wants to renegotiate; that is the caller's
            // problem, not something the link layer can absorb.
            if control.syn {
                return Ok(packet);
            }

            if packet.payload.is_empty() || control.eak {
                // A bare acknowledgement (or an EAK we have now consumed) is not session data.
                continue;
            }

            if !is_new {
                tracing::debug!(
                    seq = format_args!("{:#04x}", packet.header.seq),
                    "Duplicate packet; re-acknowledging without dispatching"
                );
                self.send_ack().await?;
                continue;
            }

            if self.link.lock().await.must_ack_now() {
                self.send_ack().await?;
            }
            return Ok(packet);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BufMut;
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
            let (frame, consumed) = Frame::read(rest).expect("a whole frame");
            if let Frame::Iap2(packet) = frame {
                packets.push((packet.header.seq, packet.payload.len()));
            }
            rest = &rest[consumed..];
        }
        packets
    }

    /// Acknowledgements repeat the last data packet's sequence number rather than taking one of
    /// their own. Get it wrong and the accessory sees a gap it can never fill, then ignores
    /// everything we send afterwards.
    #[tokio::test]
    async fn pure_acks_do_not_consume_a_sequence_number() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        // The tail of a real session: a message, an idle spell of keepalives, then another message.
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

    /// Write a packet into the peer's end of the duplex, as the accessory would.
    async fn peer_sends(peer: &mut tokio::io::DuplexStream, packet: Packet) {
        use tokio::io::AsyncWriteExt;
        peer.write_all(&Frame::Iap2(packet).to_bytes().unwrap())
            .await
            .unwrap();
    }

    /// Let `receive_packet` consume whatever is buffered. It swallows link-layer traffic and then
    /// waits for session data, so a timeout is how a test gets the side effects without hanging.
    async fn pump<S: Stream>(iap2: &Iap2<S>) {
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(50), iap2.receive_packet()).await;
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The full SYN handshake, driven with a real accessory's bytes. Two ways it fails silently:
    /// a SYN+ACK claiming `ack = 0` instead of the accessory's SYN sequence, and a session-level
    /// receive filter that swallows the bare acknowledgement forming the third leg.
    #[tokio::test]
    async fn syn_handshake_acknowledges_the_accessorys_sequence() {
        use crate::ops::syn::Syn;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        // SYN at seq 0xa8, control session id 1.
        theirs
            .write_all(&hex("ff5a001780a8000068017fffff07d003e81e0601000299"))
            .await
            .unwrap();

        let peer = tokio::spawn(async move {
            let mut buf = [0u8; 128];
            let n = theirs.read(&mut buf).await.unwrap();
            let (Frame::Iap2(reply), _) = Frame::read(&buf[..n]).unwrap() else {
                panic!("expected an iAP2 packet");
            };
            // The third leg: a bare acknowledgement of whatever the SYN+ACK was numbered.
            let ack = Packet::new(
                ControlByte::with_ack(),
                0xa8,
                reply.header.seq,
                0,
                Vec::new(),
            );
            theirs
                .write_all(&Frame::Iap2(ack).to_bytes().unwrap())
                .await
                .unwrap();
            reply
        });

        let table = Syn(&iap2)
            .perform()
            .await
            .expect("the handshake should complete");
        let reply = peer.await.unwrap();

        assert!(
            reply.header.control.syn && reply.header.control.ack,
            "expected SYN+ACK"
        );
        assert_eq!(
            reply.header.ack, 0xa8,
            "SYN+ACK must acknowledge the accessory's SYN sequence number"
        );

        // The accessory's SYN is the sole authority on session numbering.
        let control = table
            .find_type(SessionType::Control)
            .expect("SYN declared a control session");
        assert_eq!(control.id, 1);

        // And its parameters are now binding on us.
        let params = iap2.link.lock().await.params;
        assert_eq!(params.max_outstanding, 127);
        assert_eq!(params.max_cumack, 6);
    }

    /// Head units negotiate windows as small as one packet; anything beyond it has to wait for an
    /// acknowledgement rather than go out immediately.
    #[tokio::test]
    async fn a_full_window_queues_the_next_packet() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);
        iap2.link.lock().await.params.max_outstanding = 1;

        iap2.send_packet(ControlByte::with_ack(), 2, control_payload())
            .await
            .unwrap();
        iap2.send_packet(ControlByte::with_ack(), 2, control_payload())
            .await
            .unwrap();

        // Only the first is on the wire; the second is held back.
        assert_eq!(drain(&mut theirs).await, vec![(INITIAL_SEQ, 6)]);
        assert_eq!(iap2.link.lock().await.in_flight(), 1);

        // The accessory acknowledges it, which frees the window and releases the queued packet.
        peer_sends(
            &mut theirs,
            Packet::new(ControlByte::with_ack(), 0x2a, INITIAL_SEQ, 0, Vec::new()),
        )
        .await;
        pump(&iap2).await;

        assert_eq!(
            drain(&mut theirs).await,
            vec![(INITIAL_SEQ.wrapping_add(1), 6)]
        );
    }

    /// A payload larger than the accessory said it could take is refused rather than sent - it
    /// would overrun the accessory's receive buffer and cost us the link.
    #[tokio::test]
    async fn an_oversized_payload_is_refused() {
        let (ours, _theirs) = tokio::io::duplex(4096);
        let iap2 = Iap2::wrap(ours);
        iap2.link.lock().await.params.max_length = 64;

        let err = iap2
            .send_packet(ControlByte::with_ack(), 2, vec![0u8; 128])
            .await
            .expect_err("a packet over max_length must not go out");
        assert!(matches!(err, Iap2Error::PacketTooLarge { max: 64, .. }));
    }

    /// A reset from the accessory means the link is gone; carrying on talking over it just wastes
    /// the session's time.
    #[tokio::test]
    async fn a_link_reset_is_surfaced_as_an_error() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        let rst = ControlByte {
            rst: true,
            ..Default::default()
        };
        peer_sends(&mut theirs, Packet::new(rst, 0x2a, 0, 0, Vec::new())).await;

        assert!(matches!(
            iap2.receive_packet().await,
            Err(Iap2Error::LinkReset)
        ));
    }

    /// A retransmission from the accessory has to be acknowledged again but must not be handed
    /// to the session twice - it would replay whatever message it carried.
    #[tokio::test]
    async fn a_duplicate_packet_is_not_dispatched_twice() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        let data = Packet::new(ControlByte::with_ack(), 0x2a, 0, 2, control_payload());
        peer_sends(&mut theirs, data.clone()).await;
        let first = iap2.receive_packet().await.unwrap();
        assert_eq!(first.header.seq, 0x2a);

        // The same sequence number again is a retransmission, not new data.
        peer_sends(&mut theirs, data).await;
        pump(&iap2).await;
        assert_eq!(iap2.link.lock().await.ack_field(), 0x2a);
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

    /// A transport hands us whatever arrived, which is not whole frames - one `iAPSendMessage`
    /// carries six bytes and an RFCOMM read splits wherever it likes. A short buffer has to be
    /// "not yet", never an error: erroring would tear down a link over a split read.
    #[test]
    fn a_partial_frame_is_not_an_error() {
        for wire in [hex(DETECT), hex(ACCESSORY_SYN)] {
            let mut buf = BytesMut::new();
            for byte in &wire[..wire.len() - 1] {
                buf.put_u8(*byte);
                assert!(
                    matches!(Iap2Codec.decode(&mut buf), Ok(None)),
                    "decoded early at {} of {} bytes",
                    buf.len(),
                    wire.len()
                );
            }
            buf.put_u8(wire[wire.len() - 1]);
            assert!(matches!(Iap2Codec.decode(&mut buf), Ok(Some(_))));
            assert!(buf.is_empty(), "decoder left bytes behind");
        }
    }

    /// A real accessory SYN, used by the `start` tests below.
    const ACCESSORY_SYN: &str = "ff5a001780a8000068017fffff07d003e81e0601000299";

    /// The iAP1 detect, as a head unit sends it.
    const DETECT: &str = "ff550200ee10";

    /// `Iap2::start` end to end: the detect echoed, then the SYN handshake.
    #[tokio::test]
    async fn start_echoes_the_detect_for_an_iap1_accessory() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        theirs.write_all(&hex(DETECT)).await.unwrap();

        let peer = tokio::spawn(async move {
            let mut echo = [0u8; 6];
            theirs.read_exact(&mut echo).await.unwrap();
            theirs.write_all(&hex(ACCESSORY_SYN)).await.unwrap();
            let mut buf = [0u8; 128];
            let n = theirs.read(&mut buf).await.unwrap();
            let (Frame::Iap2(reply), _) = Frame::read(&buf[..n]).unwrap() else {
                panic!("expected an iAP2 packet");
            };
            let ack = Packet::new(
                ControlByte::with_ack(),
                0xa8,
                reply.header.seq,
                0,
                Vec::new(),
            );
            theirs
                .write_all(&Frame::Iap2(ack).to_bytes().unwrap())
                .await
                .unwrap();
            echo
        });

        Iap2::start(ours).await.expect("handshake should complete");
        assert_eq!(peer.await.unwrap(), hex(DETECT).as_slice());
    }

    /// The accessory does not always detect at the start: over the AirPlay tunnel it moves an
    /// established session across and detects there, once a second, until it is answered.
    #[tokio::test]
    async fn a_detect_is_echoed_wherever_it_arrives() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        // Only the two messages reach the session; the echo goes out in between.
        let data = Packet::new(ControlByte::with_ack(), 0x2a, 0, 2, control_payload());
        peer_sends(&mut theirs, data).await;
        theirs.write_all(&hex(DETECT)).await.unwrap();
        peer_sends(
            &mut theirs,
            Packet::new(ControlByte::with_ack(), 0x2b, 0, 2, control_payload()),
        )
        .await;

        assert_eq!(iap2.receive_packet().await.unwrap().header.seq, 0x2a);
        assert_eq!(iap2.receive_packet().await.unwrap().header.seq, 0x2b);

        let mut buf = [0u8; 64];
        let n = theirs.read(&mut buf).await.unwrap();
        assert!(
            buf[..n].windows(6).any(|w| w == hex(DETECT).as_slice()),
            "the detect was never echoed: {:02x?}",
            &buf[..n]
        );
    }

    /// Over the tunnel the accessory SYNs seconds after its detect is answered, by which time the
    /// first attempt has timed out - so a transport that has said anything must still be retried.
    #[tokio::test(start_paused = true)]
    async fn a_late_syn_is_still_picked_up() {
        use crate::ops::syn::Syn;

        let (ours, mut theirs) = tokio::io::duplex(1024);
        theirs.write_all(&hex(DETECT)).await.unwrap();
        let iap2 = Iap2::wrap(ours);

        tokio::spawn(async move {
            let mut echo = [0u8; 6];
            theirs.read_exact(&mut echo).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(7)).await;
            theirs.write_all(&hex(ACCESSORY_SYN)).await.unwrap();

            let mut buf = [0u8; 128];
            let n = theirs.read(&mut buf).await.unwrap();
            let (Frame::Iap2(reply), _) = Frame::read(&buf[..n]).unwrap() else {
                panic!("expected an iAP2 packet");
            };
            let ack = Packet::new(
                ControlByte::with_ack(),
                0xa8,
                reply.header.seq,
                0,
                Vec::new(),
            );
            theirs
                .write_all(&Frame::Iap2(ack).to_bytes().unwrap())
                .await
                .unwrap();
        });

        Syn(&iap2)
            .perform()
            .await
            .expect("a late SYN should still complete the handshake");
    }

    /// A silent channel must be abandoned rather than retried - on RFCOMM it is simply the wrong
    /// one of the channels BlueZ offered, and the head unit walks away while we re-read it.
    #[tokio::test(start_paused = true)]
    async fn a_silent_channel_is_abandoned_without_retrying() {
        use crate::ops::syn::Syn;

        let (ours, _theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);
        let started = tokio::time::Instant::now();

        let err = Syn(&iap2)
            .perform()
            .await
            .expect_err("a silent channel must not hang");

        assert!(matches!(err, Iap2Error::Timeout(_)), "got {err:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "gave up after {:?}; that is a retry loop, not a single attempt",
            started.elapsed()
        );
    }
}
