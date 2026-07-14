use bytes::Bytes;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Iap2Error {
    #[error("Unexpected response from IAP2 switch request")]
    UnexpectedIAP2SwitchResponse(Bytes),
    #[error("Timed out waiting for {0}")]
    Timeout(String),
    #[error("SYN exchange failed: {0}")]
    SynFailed(&'static str),
    #[error("IAP2 switch failed: {0}")]
    SwitchFailed(&'static str),
    #[error("Control session error: {0}")]
    ControlSessionError(&'static str),
    #[error("Channel closed")]
    ChannelClosed,
    #[error("Connection closed")]
    ConnectionClosed,
    #[error("IO Error")]
    IO(#[from] std::io::Error),
}
