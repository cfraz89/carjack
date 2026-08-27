//! Link-layer state: the parameters the SYN handshake negotiated, and the bookkeeping needed to
//! actually honour them (send window, retransmission, cumulative acknowledgement).

use std::collections::VecDeque;

use tokio::time::{Duration, Instant};

use crate::packet::{ControlByte, Packet};
use crate::stream::SynPayload;

/// How long the link may sit silent before a bare acknowledgement goes out to prove we are still
/// here. The accessory drops the Bluetooth link after roughly `retx_timeout * max_retx` without
/// hearing anything, which would otherwise fire during the (on this channel, silent) AirPlay phase.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);

/// The link parameters from the SYN handshake. `SynPayload` is the wire form; this is the same
/// thing with the timeouts as `Duration`s and sane fallbacks for a zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkParams {
    pub version: u8,
    /// Payload-bearing packets we may have in flight at once. Head units negotiate values as low
    /// as **1**, i.e. strictly lock-step.
    pub max_outstanding: u8,
    /// Largest whole packet, header and checksums included, the accessory will accept.
    pub max_length: u16,
    /// How long to wait for an acknowledgement before retransmitting.
    pub retx_timeout: Duration,
    /// How long a received packet may go unacknowledged. We prefer to piggyback the ack on a
    /// reply; this bounds how long we will hold out for one before sending a bare ack.
    pub cumack_timeout: Duration,
    /// Retransmissions before the link is declared dead.
    pub max_retx: u8,
    /// How many received packets one cumulative acknowledgement may cover. **Zero means the
    /// accessory set no limit**, in which case only `cumack_timeout` governs and replies piggyback
    /// wherever possible.
    pub max_cumack: u8,
}

impl Default for LinkParams {
    fn default() -> Self {
        LinkParams::from(&SynPayload::new())
    }
}

impl From<&SynPayload> for LinkParams {
    fn from(syn: &SynPayload) -> Self {
        // A zero timeout would busy-loop, and a zero window would deadlock every send, so fall
        // back to the defaults rather than trusting an accessory that sends nonsense.
        let defaults = SynPayload::new();
        let ms = |v: u16, fallback: u16| {
            Duration::from_millis(u64::from(if v == 0 { fallback } else { v }))
        };
        LinkParams {
            version: syn.version,
            max_outstanding: syn.max_outstanding.max(1),
            max_length: if syn.max_length < MIN_SANE_MAX_LENGTH {
                defaults.max_length
            } else {
                syn.max_length
            },
            retx_timeout: ms(syn.retx_timeout, defaults.retx_timeout),
            cumack_timeout: ms(syn.cumack_timeout, defaults.cumack_timeout),
            max_retx: syn.max_retx.max(1),
            max_cumack: syn.max_cumack,
        }
    }
}

/// Below this an accessory could not receive even a trivial control message, so treat it as a
/// bad SYN rather than wedging every send.
const MIN_SANE_MAX_LENGTH: u16 = 64;

impl LinkParams {
    /// Number of received packets that forces an immediate acknowledgement. Zero `max_cumack`
    /// means "unlimited" - the timer alone decides - which is what lets a reply piggyback.
    pub fn cumack_threshold(&self) -> u32 {
        if self.max_cumack == 0 {
            u32::MAX
        } else {
            u32::from(self.max_cumack)
        }
    }
}

/// A payload-bearing packet we have sent and not yet seen acknowledged.
#[derive(Debug, Clone, PartialEq)]
pub struct Unacked {
    pub seq: u8,
    pub control: ControlByte,
    pub session_id: u8,
    pub payload: Vec<u8>,
    pub sent_at: Instant,
    pub retx: u8,
}

/// What `maintain` decided needs doing now.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkAction {
    /// Nothing due; wait until this instant and ask again.
    Idle(Instant),
    /// Resend this packet (it has already been counted against `max_retx`).
    Retransmit(Box<Unacked>),
    /// Send a bare acknowledgement - either the cumulative-ack timer expired or the link has
    /// been idle long enough to need a keepalive.
    SendAck,
    /// `max_retx` is used up on the front packet.
    Dead { seq: u8, retx: u8 },
}

#[derive(Debug)]
pub struct LinkState {
    pub params: LinkParams,
    /// Sent, awaiting acknowledgement. Ordered oldest first, so the front is the retransmit
    /// candidate and the cumulative ack retires a prefix.
    unacked: VecDeque<Unacked>,
    /// Sequence number of the last payload-bearing packet we accepted, i.e. what our `ack`
    /// field carries. `None` until the accessory has sent us anything.
    last_rx_seq: Option<u8>,
    /// Received packets not yet covered by an acknowledgement we have sent.
    pending_acks: u32,
    /// When the oldest of those arrived, for the cumulative-ack timer.
    first_pending_at: Option<Instant>,
    /// Last time we put anything on the wire, for the keepalive.
    last_tx: Instant,
    /// Keepalives stay off during identification/auth: interleaving an out-of-band packet into
    /// that sequence confuses some accessories, and the exchange is busy enough not to need one.
    pub keepalive_enabled: bool,
}

impl LinkState {
    pub fn new() -> Self {
        LinkState {
            params: LinkParams::default(),
            unacked: VecDeque::new(),
            last_rx_seq: None,
            pending_acks: 0,
            first_pending_at: None,
            last_tx: Instant::now(),
            keepalive_enabled: false,
        }
    }

    /// Forget everything but the parameters - used when the SYN handshake is retried, so a
    /// half-finished attempt can't leave the window occupied or an ack pending.
    pub fn reset(&mut self) {
        self.unacked.clear();
        self.last_rx_seq = None;
        self.pending_acks = 0;
        self.first_pending_at = None;
        self.last_tx = Instant::now();
    }

    pub fn ack_field(&self) -> u8 {
        self.last_rx_seq.unwrap_or(0)
    }

    pub fn in_flight(&self) -> usize {
        self.unacked.len()
    }

    pub fn window_is_full(&self) -> bool {
        self.unacked.len() >= usize::from(self.params.max_outstanding)
    }

    pub fn record_sent(&mut self, packet: &Packet, payload: Vec<u8>) {
        self.last_tx = Instant::now();
        if payload.is_empty() {
            // A bare acknowledgement takes no sequence number and is never retransmitted, but it
            // does discharge whatever we owed.
            self.pending_acks = 0;
            self.first_pending_at = None;
            return;
        }
        self.unacked.push_back(Unacked {
            seq: packet.header.seq,
            control: packet.header.control,
            session_id: packet.header.session_id,
            payload,
            sent_at: Instant::now(),
            retx: 0,
        });
        // The ack rides along on this packet, so nothing is outstanding any more.
        self.pending_acks = 0;
        self.first_pending_at = None;
    }

    /// Note that a retransmission went out, restarting its timer.
    pub fn record_retransmit(&mut self, seq: u8) {
        self.last_tx = Instant::now();
        if let Some(entry) = self.unacked.iter_mut().find(|u| u.seq == seq) {
            entry.sent_at = Instant::now();
        }
        self.pending_acks = 0;
        self.first_pending_at = None;
    }

    /// Retire everything the accessory's cumulative `ack` field covers, returning how many.
    ///
    /// Bounded by what is in flight rather than a plain "at or before the ack" test: sequence
    /// numbers wrap, and an accessory that has not heard from us sends `ack = 0`, which a naive
    /// half-space comparison reads as covering our initial `0x9D`.
    pub fn retire_acked(&mut self, ack: u8) -> usize {
        let mut retired = 0;
        while let Some(front) = self.unacked.front() {
            let distance = usize::from(ack.wrapping_sub(front.seq));
            if distance >= self.unacked.len() {
                break;
            }
            self.unacked.pop_front();
            retired += 1;
        }
        retired
    }

    /// Retire the individual sequence numbers listed in an extended-ack payload. EAK covers
    /// packets received *out of order*, which a cumulative ack by definition cannot.
    pub fn retire_eak(&mut self, seqs: &[u8]) -> usize {
        let before = self.unacked.len();
        self.unacked.retain(|u| !seqs.contains(&u.seq));
        before - self.unacked.len()
    }

    /// Record an inbound payload-bearing packet. Returns `false` if it is a retransmission of
    /// something we have already taken, which must be re-acknowledged but not acted on twice.
    pub fn accept_rx(&mut self, seq: u8) -> bool {
        if self.last_rx_seq == Some(seq) {
            return false;
        }
        self.last_rx_seq = Some(seq);
        self.pending_acks += 1;
        self.first_pending_at.get_or_insert_with(Instant::now);
        true
    }

    /// Whether enough packets have piled up unacknowledged to force an ack right now.
    pub fn must_ack_now(&self) -> bool {
        self.pending_acks >= self.params.cumack_threshold()
    }

    /// When the link next needs attention - `now` if something is already due.
    ///
    /// Side-effect free, unlike `next_action`, so a caller can wait on it inside a `select!` and
    /// run `next_action` outside one - a transmit cancelled part-way loses or duplicates a packet.
    pub fn next_deadline(&self) -> Instant {
        let now = Instant::now();

        let retx_due = self
            .unacked
            .front()
            .map(|u| u.sent_at + self.params.retx_timeout);
        let ack_due = self
            .first_pending_at
            .map(|at| at + self.params.cumack_timeout);
        let keepalive_due = self
            .keepalive_enabled
            .then(|| self.last_tx + KEEPALIVE_INTERVAL);

        if self.must_ack_now() {
            return now;
        }
        [retx_due, ack_due, keepalive_due]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(now + self.params.cumack_timeout)
    }

    /// What the link needs next, and when to come back if the answer is "nothing yet".
    pub fn next_action(&mut self) -> LinkAction {
        let now = Instant::now();

        if let Some(front) = self.unacked.front_mut()
            && now.duration_since(front.sent_at) >= self.params.retx_timeout
        {
            if front.retx >= self.params.max_retx {
                return LinkAction::Dead {
                    seq: front.seq,
                    retx: front.retx,
                };
            }
            front.retx += 1;
            return LinkAction::Retransmit(Box::new(front.clone()));
        }

        let ack_due = self
            .first_pending_at
            .map(|at| at + self.params.cumack_timeout);
        if ack_due.is_some_and(|due| now >= due) || self.must_ack_now() {
            return LinkAction::SendAck;
        }

        let keepalive_due = self
            .keepalive_enabled
            .then(|| self.last_tx + KEEPALIVE_INTERVAL);
        if keepalive_due.is_some_and(|due| now >= due) {
            return LinkAction::SendAck;
        }

        // Nothing due: wake at the earliest deadline, or after a cumack interval if the link is
        // entirely quiet, so the caller re-enters and re-checks.
        let retx_due = self
            .unacked
            .front()
            .map(|u| u.sent_at + self.params.retx_timeout);
        let deadline = [retx_due, ack_due, keepalive_due]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(now + self.params.cumack_timeout);
        LinkAction::Idle(deadline)
    }
}

impl Default for LinkState {
    fn default() -> Self {
        LinkState::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(max_outstanding: u8) -> LinkParams {
        LinkParams {
            max_outstanding,
            ..LinkParams::default()
        }
    }

    fn sent(state: &mut LinkState, seq: u8) {
        let packet = Packet::new(ControlByte::with_ack(), seq, 0, 2, vec![1, 2, 3]);
        state.record_sent(&packet, vec![1, 2, 3]);
    }

    /// A real head unit's SYN, byte for byte.
    #[test]
    fn syn_parameters_are_carried_through() {
        let syn = SynPayload::from_bytes(&[
            0x01, 0x01, 0x10, 0x00, 0x03, 0xe8, 0x01, 0xf4, 0x0f, 0x00, 0x02, 0x00, 0x02,
        ])
        .unwrap();
        let params = LinkParams::from(&syn);
        assert_eq!(params.max_outstanding, 1);
        assert_eq!(params.max_length, 4096);
        assert_eq!(params.retx_timeout, Duration::from_millis(1000));
        assert_eq!(params.cumack_timeout, Duration::from_millis(500));
        assert_eq!(params.max_retx, 15);
        // Zero means unlimited, so only the timer decides and replies can piggyback.
        assert_eq!(params.max_cumack, 0);
        assert_eq!(params.cumack_threshold(), u32::MAX);
    }

    /// An accessory that has not heard from us sends `ack = 0`, which a naive wrapping comparison
    /// treats as "after" our initial `0x9D`.
    #[test]
    fn an_unrelated_ack_retires_nothing() {
        let mut state = LinkState::new();
        state.params = params(4);
        sent(&mut state, 0x9d);
        assert_eq!(state.retire_acked(0x00), 0);
        assert_eq!(state.in_flight(), 1);
    }

    /// A cumulative ack retires the whole prefix up to and including it, and no more.
    #[test]
    fn cumulative_ack_retires_a_prefix() {
        let mut state = LinkState::new();
        state.params = params(4);
        for seq in [0xa3, 0xa4, 0xa5] {
            sent(&mut state, seq);
        }
        assert_eq!(state.retire_acked(0xa4), 2);
        assert_eq!(state.in_flight(), 1);
        assert_eq!(state.retire_acked(0xa5), 1);
        assert_eq!(state.in_flight(), 0);
        // A repeat of the same ack retires nothing rather than under-flowing.
        assert_eq!(state.retire_acked(0xa5), 0);
    }

    #[test]
    fn cumulative_ack_wraps_past_255() {
        let mut state = LinkState::new();
        state.params = params(4);
        for seq in [0xfe, 0xff, 0x00, 0x01] {
            sent(&mut state, seq);
        }
        assert_eq!(state.retire_acked(0x00), 3);
        assert_eq!(state.in_flight(), 1);
    }

    /// The window is what stops us putting three packets on a link that negotiated one.
    #[test]
    fn the_window_fills_and_drains() {
        let mut state = LinkState::new();
        state.params = params(1);
        assert!(!state.window_is_full());
        sent(&mut state, 0xa3);
        assert!(state.window_is_full());
        state.retire_acked(0xa3);
        assert!(!state.window_is_full());
    }

    /// A retransmitted inbound packet has to be acknowledged again but not dispatched twice.
    #[test]
    fn a_repeated_inbound_sequence_is_a_duplicate() {
        let mut state = LinkState::new();
        assert!(state.accept_rx(0x2f));
        assert!(!state.accept_rx(0x2f));
        assert!(state.accept_rx(0x30));
        assert_eq!(state.ack_field(), 0x30);
    }

    /// With no `max_cumack` limit a received packet is not acknowledged straight away - the timer
    /// gives our reply a chance to carry the ack, keeping the handshake to one packet per exchange.
    #[test]
    fn a_reply_piggybacks_the_pending_ack() {
        let mut state = LinkState::new();
        state.params = LinkParams {
            max_cumack: 0,
            ..params(4)
        };
        state.accept_rx(0x2f);
        assert!(!state.must_ack_now());
        assert!(matches!(state.next_action(), LinkAction::Idle(_)));

        sent(&mut state, 0xa3);
        // The reply carried it, so nothing is owed and only the retransmit timer is left.
        assert!(!state.must_ack_now());
        assert!(matches!(state.next_action(), LinkAction::Idle(_)));
    }

    /// An accessory that *does* set a limit gets an acknowledgement as soon as that many
    /// packets have piled up, without waiting for the timer.
    #[test]
    fn max_cumack_forces_an_immediate_ack() {
        let mut state = LinkState::new();
        state.params = LinkParams {
            max_cumack: 2,
            ..params(4)
        };
        state.accept_rx(0x2f);
        assert!(!state.must_ack_now());
        state.accept_rx(0x30);
        assert!(state.must_ack_now());
        assert_eq!(state.next_action(), LinkAction::SendAck);
    }

    /// EAK acknowledges out-of-order packets that a cumulative ack cannot describe.
    #[test]
    fn extended_ack_retires_individual_packets() {
        let mut state = LinkState::new();
        state.params = params(4);
        for seq in [0xa3, 0xa4, 0xa5] {
            sent(&mut state, seq);
        }
        assert_eq!(state.retire_eak(&[0xa5]), 1);
        assert_eq!(state.in_flight(), 2);
    }

    /// An unacknowledged packet is retransmitted once its timer expires, and each retransmission
    /// restarts that timer rather than firing continuously.
    #[tokio::test(start_paused = true)]
    async fn an_unacknowledged_packet_is_retransmitted() {
        let mut state = LinkState::new();
        state.params = LinkParams {
            retx_timeout: Duration::from_millis(1000),
            max_retx: 3,
            ..params(1)
        };
        sent(&mut state, 0xa3);
        assert!(matches!(state.next_action(), LinkAction::Idle(_)));

        tokio::time::advance(Duration::from_millis(1001)).await;
        let LinkAction::Retransmit(unacked) = state.next_action() else {
            panic!("expected a retransmit once the timer expired");
        };
        assert_eq!(unacked.seq, 0xa3);
        assert_eq!(unacked.retx, 1);

        // Until the resend is recorded the timer has not restarted, but once it is we go quiet.
        state.record_retransmit(0xa3);
        assert!(matches!(state.next_action(), LinkAction::Idle(_)));
    }

    /// After `max_retx` attempts the accessory has stopped listening, and the session has to be
    /// torn down rather than retransmitting into the void forever.
    #[tokio::test(start_paused = true)]
    async fn retransmits_are_bounded_by_max_retx() {
        let mut state = LinkState::new();
        state.params = LinkParams {
            retx_timeout: Duration::from_millis(100),
            max_retx: 2,
            ..params(1)
        };
        sent(&mut state, 0xa3);

        for expected in 1..=2 {
            tokio::time::advance(Duration::from_millis(101)).await;
            let LinkAction::Retransmit(unacked) = state.next_action() else {
                panic!("expected retransmit {expected}");
            };
            assert_eq!(unacked.retx, expected);
            state.record_retransmit(0xa3);
        }

        tokio::time::advance(Duration::from_millis(101)).await;
        assert_eq!(state.next_action(), LinkAction::Dead { seq: 0xa3, retx: 2 });
    }

    /// A received packet must be acknowledged within the negotiated `cumack_timeout` even if we
    /// never send a reply it could ride on.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_packet_is_acknowledged_on_the_timer() {
        let mut state = LinkState::new();
        state.params = LinkParams {
            cumack_timeout: Duration::from_millis(500),
            max_cumack: 0,
            ..params(4)
        };
        state.accept_rx(0x2f);
        assert!(matches!(state.next_action(), LinkAction::Idle(_)));
        tokio::time::advance(Duration::from_millis(501)).await;
        assert_eq!(state.next_action(), LinkAction::SendAck);
    }

    /// With nothing else to say, an idle link still has to emit a keepalive or the accessory
    /// decides it is dead and drops the Bluetooth connection mid-AirPlay.
    #[tokio::test(start_paused = true)]
    async fn an_idle_link_emits_a_keepalive() {
        let mut state = LinkState::new();
        state.params = params(4);
        assert!(matches!(state.next_action(), LinkAction::Idle(_)));

        state.keepalive_enabled = true;
        tokio::time::advance(KEEPALIVE_INTERVAL + Duration::from_millis(1)).await;
        assert_eq!(state.next_action(), LinkAction::SendAck);
    }

    /// A zero window or timeout in the accessory's SYN would deadlock or busy-loop us, so the
    /// defaults stand in.
    #[test]
    fn nonsense_syn_values_fall_back_to_defaults() {
        let mut syn = SynPayload::new();
        syn.max_outstanding = 0;
        syn.retx_timeout = 0;
        syn.cumack_timeout = 0;
        syn.max_retx = 0;
        syn.max_length = 1;
        let params = LinkParams::from(&syn);
        assert_eq!(params.max_outstanding, 1);
        assert_eq!(params.max_retx, 1);
        assert_eq!(params.max_length, SynPayload::new().max_length);
        assert!(params.retx_timeout > Duration::ZERO);
        assert!(params.cumack_timeout > Duration::ZERO);
    }
}
