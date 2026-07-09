use bytes::Bytes;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Iap2Error {
    #[error("Unexpected response from IAP2 switch request")]
    UnexpectedIAP2SwitchResponse(Bytes),
    #[error("Timed out waiting for {0}")]
    Timeout(String),
    #[error("IO Error")]
    IO(#[from] std::io::Error),
}
