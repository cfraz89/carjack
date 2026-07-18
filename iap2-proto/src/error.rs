use bytes::Bytes;
use deku::DekuError;
use thiserror::Error;
use tokio::time::error::Elapsed;

use crate::packet::Packet;

#[derive(Error, Debug)]
pub enum Iap2Error {
    #[error("Unexpected bytes: {0} {0:?}")]
    UnexpectedBytes(String, Bytes),
    #[error("Unexpected packet: {0} {0:?}")]
    UnexpectedPacket(String, Packet),
    #[error("Error with packet decoding. Bytes: {0:?}, error: {1}")]
    PacketDecoding(Bytes, DekuError),
    #[error("Error with packet encoding. Packet: {0:?}, error: {1}")]
    PacketEncoding(Packet, DekuError),
    #[error("No packet received")]
    NoPacket,
    #[error("Control session error: {0}")]
    ControlSessionError(&'static str),
    #[error("Channel closed")]
    ChannelClosed,
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
