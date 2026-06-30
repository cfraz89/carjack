use backon::{ConstantBuilder, Retryable};
use deku::{DekuContainerRead, DekuContainerWrite};
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::sleep;

use iap2_proto::{IAP2_HANDSHAKE, Packet};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};

use crate::iap2::config::LinkConfig;

/// Models iAP2 protocol, abstracting RFCOMM stream into `Packet` stream and sink.
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
    pub async fn start(read_stream: R, write_stream: W, config: LinkConfig) -> Result<Self, Error> {
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
        link.handshake().await?;
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

    //Keep reading until we find the iap2 handshake bytes
    async fn read_iap2_handshake(&self) -> Result<(), Error> {
        let mut buf = [0u8; 256];

        //Accumulate bytes in case the 6 bytes arrive fragmented across reads
        let mut accumulated = Vec::with_capacity(1024);

        loop {
            // Read whatever is currently waiting in the OS buffer
            let n = self.read_stream.lock().await.read(&mut buf).await?;
            if n == 0 {
                Err(Error::new(
                    ErrorKind::ConnectionAborted,
                    "Stream closed by remote side",
                ))?;
            }

            accumulated.extend_from_slice(&buf[..n]);

            if accumulated
                .windows(IAP2_HANDSHAKE.len())
                .find(|w| *w == IAP2_HANDSHAKE)
                .is_some()
            {
                return Ok(());
            }

            //Keep our accumulationg buffer from growing too large
            if accumulated.len() > 512 {
                accumulated.drain(506..);
            }
        }
    }

    pub async fn handshake(&self) -> Result<(), Error> {
        tokio::select!(
            _ = sleep(self.config.handshake_timeout) => {
                Err(Error::new(ErrorKind::TimedOut, "Timed out waiting for iAP2 handshake"))
            },
            _ = (|| async { self.write_stream.lock().await.write(&IAP2_HANDSHAKE).await })
                    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(1)))
            => {
                Err(Error::new(ErrorKind::BrokenPipe, "Sender ended prematurely"))
            },
            res = (|| async { self.read_iap2_handshake().await })
                    .retry(ConstantBuilder::new().with_delay(Duration::from_secs(1))) => {
                res
            },
        )
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
