use deku::{DekuContainerRead, DekuContainerWrite};
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::task::JoinHandle;

use bluer::rfcomm::Stream;
use iap2_proto::{IAP2_HANDSHAKE, Packet};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// Parses raw iAP2 bytes from an RFCOMM stream into `Packet` values.
/// Also provides `send_packet` for writing typed packets back.
#[derive(Debug)]
pub struct Iap2 {
    stream: Stream,
    buffer: Vec<u8>,
    write_buffer: Option<Vec<u8>>,
    write_pos: usize,
}

impl Iap2 {
    pub fn new(stream: Stream) -> Self {
        Iap2 {
            stream,
            buffer: Vec::new(),
            write_buffer: None,
            write_pos: 0,
        }
    }

    fn poll_write_buffer(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let this = self.get_mut();

        if let Some(buf) = &this.write_buffer {
            while this.write_pos < buf.len() {
                match Pin::new(&mut this.stream).poll_write(cx, &buf[this.write_pos..]) {
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

    pub async fn handshake(&mut self) -> Result<(), Error> {
        self.stream.write(&IAP2_HANDSHAKE).await?;
        //Ensure link responds with equal handshake packet
        let mut buf = [0u8; 6];
        self.stream.read_exact(&mut buf).await?;
        if buf != IAP2_HANDSHAKE {
            Err(Error::new(
                ErrorKind::ConnectionAborted,
                "IAP2 handshake failed",
            ))?
        }
        Ok(())
    }
}

impl futures::Stream for Iap2 {
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

            match Pin::new(&mut this.stream).poll_read(cx, &mut read_buf) {
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

impl futures::Sink<Packet> for Iap2 {
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

        match Pin::new(&mut this.stream).poll_flush(cx) {
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

        match Pin::new(&mut this.stream).poll_shutdown(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }
}
