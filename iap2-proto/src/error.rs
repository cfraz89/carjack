use bytes::Bytes;
use thiserror::Error;
use tokio::time::error::Elapsed;

use crate::packet::Packet;

#[derive(Error, Debug)]
pub enum Iap2Error {
    #[error("Unexpected bytes: {0} {0:?}")]
    UnexpectedBytes(String, Bytes),
    #[error("Unexpected packet: {0} {0:?}")]
    UnexpectedPacket(String, Packet),
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
    #[error("Other Error")]
    Other(String),
}
