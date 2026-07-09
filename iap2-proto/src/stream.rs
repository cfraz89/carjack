use backon::{ConstantBuilder, Retryable};
use bytes::BytesMut;
use deku::{DekuContainerRead, DekuContainerWrite};
use futures::SinkExt;
use pretty_hex::PrettyHex;
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::sleep;

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};

use crate::config::LinkConfig;
use crate::error::Iap2Error;
use crate::packet::{ControlByte, Header, Packet};
use crate::switch::REQUEST_IAP2_SWITCH_BYTES;

/// Models iAP2 protocol, manages read and write network streams, providing a `Packet` stream and sink.
#[derive(Debug)]
pub struct Iap2<R, W> {
    read_stream: Mutex<R>,
    write_stream: Mutex<W>,
    buffer: Vec<u8>,
    write_buffer: Option<Vec<u8>>,
    write_pos: usize,
    seq: u8,
    ack: u8,
    pub config: LinkConfig,
}

impl<R, W> Iap2<R, W>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    //Create an iap2 link, starting with an iap2 compatibility hanshake with the receiving end
    pub async fn start(
        read_stream: R,
        write_stream: W,
        config: LinkConfig,
    ) -> Result<Self, Iap2Error> {
        let link = Iap2 {
            read_stream: Mutex::new(read_stream),
            write_stream: Mutex::new(write_stream),
            buffer: Vec::new(),
            write_buffer: None,
            write_pos: 0,
            seq: config.initial_seq,
            ack: 0x00,
            config: config,
        };
        // link.reset().await?;
        link.request_iap2_switch().await?;
        Ok(link)
    }

    fn poll_write_buffer(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let this = self.get_mut();

        if let Some(buf) = &this.write_buffer {
            while this.write_pos < buf.len() {
                match Pin::new(this.write_stream.get_mut()).poll_write(cx, &buf[this.write_pos..]) {
                    Poll::Ready(Ok(0)) => {
                        return Poll::Ready(Err(Error::new(
                            ErrorKind::WriteZero,
                            "failed to write packet",
                        )));
                    }
                    Poll::Ready(Ok(n)) => {
                        this.write_pos += n;
                    }
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                    Poll::Pending => return Poll::Pending,
                }
            }

            this.write_buffer = None;
            this.write_pos = 0;
        }

        Poll::Ready(Ok(()))
    }

    /// Write out iAP2 switch bytes, and attempt to read back the same bytes as response.
    pub async fn request_iap2_switch(&self) -> Result<(), Iap2Error> {
        tokio::select!(
            _ = sleep(self.config.iap2_switch_timeout) => Err(Iap2Error::Timeout("iAP2 switch".to_string())),
            res = (async || -> Result<(), Iap2Error> {
                    loop {
                        let mut write_stream = self.write_stream.lock().await;
                        write_stream.write(&REQUEST_IAP2_SWITCH_BYTES).await?;
                        tracing::debug!("Wrote IAP2 switch packet");
                        sleep(Duration::from_secs(1)).await;
                    }
            }).retry(ConstantBuilder::new().with_delay(Duration::from_secs(1))) => res,
            res = (async || -> Result<(), Iap2Error> {
                let mut buf = BytesMut::with_capacity(6);
                let mut read_stream = self.read_stream.lock().await;
                let n = read_stream.read_exact(&mut buf).await?;
                tracing::debug!(buf = ?buf.hex_dump(), "Read stream");
                if n == 0 {
                    Err(Error::new(
                        ErrorKind::ConnectionAborted,
                        "Stream closed by remote side",
                    ))?;
                }
                if buf != REQUEST_IAP2_SWITCH_BYTES {
                    Err(Iap2Error::UnexpectedIAP2SwitchResponse(buf.freeze()))?;
                }
                Ok(())
            }).retry(ConstantBuilder::new().with_delay(Duration::from_secs(1))) => res,
        )
    }

    pub async fn reset(&mut self) -> Result<(), Iap2Error> {
        Ok(self
            .send(Packet {
                header: Header {
                    control: ControlByte {
                        rst: true,
                        ..Default::default()
                    },
                    length: 0,
                    seq: self.seq,
                    ack: self.ack,
                    session_id: 0,
                },
                header_checksum: 0,
                payload: Vec::new(),
                payload_checksum: None,
            })
            .await?)
    }
}

impl<R, W> futures::Stream for Iap2<R, W>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    type Item = Packet;

    // Try to create a packet from our persistent buffer, otherwise read into the buffer what we can
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Packet>> {
        let this = self.get_mut();

        loop {
            if let Some(((_, offset), packet)) = Packet::from_bytes((&mut this.buffer, 0)).ok() {
                this.buffer.drain(..offset);
                return Poll::Ready(Some(packet));
            }

            let mut buf = vec![0u8; 4096];
            let mut read_buf = ReadBuf::new(&mut buf);

            match Pin::new(this.read_stream.get_mut()).poll_read(cx, &mut read_buf) {
                Poll::Ready(Ok(())) => {
                    let n = read_buf.filled().len();
                    if n == 0 {
                        return Poll::Ready(None);
                    }
                    this.buffer.extend_from_slice(read_buf.filled());
                }
                Poll::Ready(Err(_)) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<R, W> futures::Sink<Packet> for Iap2<R, W>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_write_buffer(cx)
    }

    fn start_send(self: Pin<&mut Self>, packet: Packet) -> Result<(), Self::Error> {
        let this = self.get_mut();

        if this.write_buffer.is_some() {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "previous packet not yet flushed",
            ));
        }

        let bytes = packet
            .to_bytes()
            .map_err(|err| Error::new(ErrorKind::InvalidData, err.to_string()))?;

        this.write_buffer = Some(bytes);
        this.write_pos = 0;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        match Pin::new(&mut *this).poll_write_buffer(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            Poll::Pending => return Poll::Pending,
        }

        match Pin::new(this.write_stream.get_mut()).poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        match Pin::new(&mut *this).poll_flush(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            Poll::Pending => return Poll::Pending,
        }

        match Pin::new(this.write_stream.get_mut()).poll_shutdown(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }
}
