use std::sync::atomic::{AtomicU8, Ordering};

use bytes::{Buf, BytesMut};
use deku::{DekuContainerRead, DekuContainerWrite};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::codec::{Decoder, Encoder, Framed};

use std::collections::VecDeque;

use crate::error::Iap2Error;
use crate::link::{LinkAction, LinkState};
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

/// A packet waiting for the send window to open. Held as its parts rather than a built `Packet`
/// so the `ack` field is filled in at transmit time and carries the freshest value.
struct Queued {
    control: ControlByte,
    session_id: u8,
    payload: Vec<u8>,
}

pub struct Iap2<T> {
    // Split into independent read/write halves so a pending receive_packet() (which
    // spends nearly all its time blocked waiting for the accessory's next byte) never
    // blocks a concurrent send_packet() - needed for link-layer keep-alives to actually
    // go out while otherwise idle.
    sink: Mutex<SplitSink<Framed<T, Iap2Codec>, Packet>>,
    stream: Mutex<SplitStream<Framed<T, Iap2Codec>>>,
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
            link: Mutex::new(LinkState::new()),
            tx_queue: Mutex::new(VecDeque::new()),
            session_table: Mutex::new(SessionTable::new()),
        }
    }

    /// Bring the link up on a transport that is already established, skipping the iAP1 switch.
    ///
    /// The switch (`FF 55 02 00 EE 10`) is an artefact of the RFCOMM channel: it is how an
    /// accessory asks a serial link to change protocol. The AirPlay tunnel carries iAP2 frames
    /// directly, so there is nothing to switch - the link starts at the SYN.
    pub async fn start_established(stream: S) -> Result<Self, Iap2Error> {
        let iap2 = Iap2::wrap(stream);
        let table = Syn(&iap2).perform().await?;
        *iap2.session_table.lock().await = table;
        Ok(iap2)
    }

    pub async fn start(mut stream: S) -> Result<Self, Iap2Error> {
        perform_switch(&mut stream).await?;

        let iap2 = Iap2::wrap(stream);
        let table = Syn(&iap2).perform().await?;
        *iap2.session_table.lock().await = table;
        Ok(iap2)
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
        // `self.seq` is the number the *next* payload-bearing packet will use. A packet with
        // no payload is a pure acknowledgement, and repeats the last one instead - see
        // `send_packet` for why that matters.
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
    ///
    /// **This never blocks on the send window.** The session dispatches its replies from inside
    /// its own receive loop, so a `send_packet` that waited for an acknowledgement would stop
    /// that loop from ever reading the acknowledgement it is waiting for - a guaranteed deadlock
    /// at `max_outstanding = 1`, which is what the CFMOTO head unit negotiates. A packet that
    /// does not fit in the window is queued instead and goes out from `maintain` as soon as an
    /// acknowledgement frees a slot.
    pub async fn send_packet(
        &self,
        control: ControlByte,
        session_id: u8,
        payload: Vec<u8>,
    ) -> Result<(), Iap2Error> {
        self.check_length(&payload).await?;

        // Bare acknowledgements and link-control packets (SYN, RST) are not session data: they
        // take no sequence number, are never retransmitted, and are not subject to the window -
        // the SYN exchange has to complete *before* there is a negotiated window at all.
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
            sink.send(packet.clone()).await?;
        }
        if consumes_seq {
            self.seq.fetch_add(1, Ordering::Relaxed);
        }
        self.link.lock().await.record_sent(&packet, payload);
        Ok(())
    }

    /// Reject anything larger than the accessory said it could receive. Sending it regardless
    /// would overrun its buffer and cost us the link, which is far worse than a failed send.
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

    /// Drive the link-layer timers: retransmit anything that has gone unacknowledged for
    /// `retx_timeout`, and send a bare acknowledgement when the cumulative-ack timer expires or
    /// the link has been idle long enough to need a keepalive.
    ///
    /// Tell the accessory the link is finished, so it can start a fresh SYN straight away
    /// instead of waiting out its own retransmit timeout. Best-effort - by the time we send
    /// this the transport is often already gone.
    pub async fn send_reset(&self) -> Result<(), Iap2Error> {
        let control = ControlByte {
            rst: true,
            ..Default::default()
        };
        self.transmit(control, LINK_SESSION_ID, Vec::new()).await
    }

    /// Sleep until the link needs attention. Pure - it only waits - so it is safe to cancel,
    /// which is what lets it sit in a `select!` while `maintain` runs outside one.
    pub async fn wait_for_link_work(&self) {
        // Anything already queued goes out the moment the window has room for it.
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
                self.sink.lock().await.send(packet).await?;
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
    /// packet - bare acknowledgements and SYNs included.
    ///
    /// The SYN handshake needs those: its third leg *is* a bare acknowledgement, and the SYN it
    /// answers carries the sequence number our SYN+ACK has to acknowledge. The session doesn't,
    /// and uses [`Iap2::receive_packet`] instead.
    ///
    /// Returns the packet and whether it was new (`false` = a retransmission of a sequence
    /// number we have already taken).
    pub(crate) async fn receive_raw(&self) -> Result<(Packet, bool), Iap2Error> {
        let packet = {
            let mut stream = self.stream.lock().await;
            stream.next().await.ok_or(Iap2Error::NoPacket)??
        };
        let control = packet.header.control;

        if control.rst {
            tracing::warn!("Accessory reset the link");
            return Err(Iap2Error::LinkReset);
        }
        if control.sleep {
            // Nothing to do beyond noting it: we have no power state to enter, and the
            // accessory carries on talking regardless.
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
        // SYN** - it is a numbered packet like any other, and its number is what our SYN+ACK
        // has to acknowledge. Skipping it left the SYN+ACK claiming `ack = 0`, so the accessory
        // never saw its SYN acknowledged and retransmitted it until it gave up
        // (`bench_2.pcapng`: our `ff5a 0017 c0 9d 00 00` against its SYN at seq `0xa8`).
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

    /// Write a packet into the peer's end of the duplex, as the accessory would.
    async fn peer_sends(peer: &mut tokio::io::DuplexStream, packet: Packet) {
        use tokio::io::AsyncWriteExt;
        peer.write_all(&packet.to_bytes().unwrap()).await.unwrap();
    }

    /// Let `receive_packet` consume whatever is buffered. It swallows link-layer traffic and
    /// then waits for real session data, so a timeout is how a test gets the side effects
    /// without hanging on the wait that follows.
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

    /// The full SYN handshake, driven with the AUTO BOX bench unit's real bytes.
    ///
    /// Two regressions hid behind the absence of this test, both in `bench_2.pcapng`:
    /// our SYN+ACK claimed `ack = 0` instead of the accessory's SYN sequence (so it never saw
    /// its SYN acknowledged and retransmitted until it gave up), and the session-level receive
    /// filter swallowed bare acknowledgements - which is exactly what the third leg of this
    /// handshake is, so it could never complete.
    #[tokio::test]
    async fn syn_handshake_acknowledges_the_accessorys_sequence() {
        use crate::ops::syn::Syn;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (ours, mut theirs) = tokio::io::duplex(1024);
        let iap2 = Iap2::wrap(ours);

        // bench_2.pcapng: SYN at seq 0xa8, control session id 1 (the CFMOTO numbers its 2).
        theirs
            .write_all(&hex("ff5a001780a8000068017fffff07d003e81e0601000299"))
            .await
            .unwrap();

        let peer = tokio::spawn(async move {
            let mut buf = [0u8; 128];
            let n = theirs.read(&mut buf).await.unwrap();
            let (_, reply) = Packet::from_bytes((&buf[..n], 0)).unwrap();
            // The third leg: a bare acknowledgement of whatever the SYN+ACK was numbered.
            let ack = Packet::new(
                ControlByte::with_ack(),
                0xa8,
                reply.header.seq,
                0,
                Vec::new(),
            );
            theirs.write_all(&ack.to_bytes().unwrap()).await.unwrap();
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

    /// The CFMOTO head unit negotiates a one-packet window. Anything beyond it has to wait for
    /// an acknowledgement rather than going out immediately - we used to send three at once
    /// (`cfmoto_11.pcapng` frames 851-853).
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

    /// A reset from the accessory means the link is gone. Carrying on talking over it - which is
    /// what ignoring the bit amounted to - just wastes the session's time.
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

    /// A real accessory SYN, used by the `start` tests below.
    const BENCH_SYN: &str = "ff5a001780a8000068017fffff07d003e81e0601000299";

    /// `Iap2::start` end to end: switch echoed on the raw stream, then the SYN handshake.
    #[tokio::test]
    async fn start_echoes_the_switch_for_an_iap1_accessory() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        theirs.write_all(&hex("ff550200ee10")).await.unwrap();

        let peer = tokio::spawn(async move {
            // The echo comes first, on its own, before the codec is in play.
            let mut echo = [0u8; 6];
            theirs.read_exact(&mut echo).await.unwrap();
            theirs.write_all(&hex(BENCH_SYN)).await.unwrap();
            let mut buf = [0u8; 128];
            let n = theirs.read(&mut buf).await.unwrap();
            let (_, reply) = Packet::from_bytes((&buf[..n], 0)).unwrap();
            let ack = Packet::new(
                ControlByte::with_ack(),
                0xa8,
                reply.header.seq,
                0,
                Vec::new(),
            );
            theirs.write_all(&ack.to_bytes().unwrap()).await.unwrap();
            echo
        });

        Iap2::start(ours).await.expect("handshake should complete");
        assert_eq!(peer.await.unwrap(), hex("ff550200ee10").as_slice());
    }

    /// A silent channel must be abandoned quickly rather than re-read.
    #[tokio::test(start_paused = true)]
    async fn a_silent_channel_is_abandoned_quickly() {
        use crate::ops::switch::perform_switch;

        let (mut ours, _theirs) = tokio::io::duplex(1024);
        let started = tokio::time::Instant::now();
        let err = perform_switch(&mut ours)
            .await
            .expect_err("a silent channel must not hang");

        assert!(matches!(err, Iap2Error::Timeout(_)), "got {err:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "gave up after {:?}; the head unit walks away in about two seconds",
            started.elapsed()
        );
    }
}
