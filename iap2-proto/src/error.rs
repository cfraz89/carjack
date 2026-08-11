use bytes::Bytes;
use deku::DekuError;
use thiserror::Error;

use crate::packet::{Frame, Packet};

#[derive(Error, Debug)]
pub enum Iap2Error {
    #[error("Unexpected packet: {0} {0:?}")]
    UnexpectedPacket(String, Packet),
    #[error("Error with packet decoding. Bytes: {0:?}, error: {1}")]
    PacketDecoding(Bytes, DekuError),
    #[error("Error with frame encoding. Frame: {0:?}, error: {1}")]
    FrameEncoding(Frame, DekuError),
    #[error("No packet received")]
    NoPacket,
    #[error("Control session error: {0}")]
    ControlSessionError(&'static str),
    #[error("Channel closed")]
    ChannelClosed,
    /// The accessory set the link-reset bit. The link is gone; the session has to be rebuilt
    /// from a fresh SYN rather than carried on with.
    #[error("Accessory reset the link")]
    LinkReset,
    /// A packet went unacknowledged through all `max_retx` retransmissions - the accessory has
    /// stopped listening, so there is nothing left to do but tear the session down.
    #[error("Packet seq {seq:#04x} unacknowledged after {retx} retransmissions")]
    RetransmitsExhausted { seq: u8, retx: u8 },
    /// Bigger than the `max_length` the accessory advertised in its SYN. Sending it anyway
    /// would overrun the accessory's receive buffer and cost us the link.
    #[error("Packet of {len} bytes exceeds the negotiated maximum of {max}")]
    PacketTooLarge { len: usize, max: u16 },
    #[error("Connection closed")]
    ConnectionClosed,
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("IO Error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deku error: {0}")]
    Deku(#[from] deku::error::DekuError),
    #[error("Other Error")]
    Other(String),
}
