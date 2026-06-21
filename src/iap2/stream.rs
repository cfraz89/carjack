use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};

use bluer::rfcomm::Stream;
use iap2_proto::Packet;
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};

/// Parses raw iAP2 bytes from an RFCOMM stream into `Packet` values.
/// Also provides `send_packet` for writing typed packets back.
#[derive(Debug)]
pub struct Iap2 {
    stream: Stream,
    buffer: Vec<u8>,
}

impl Iap2 {
    pub fn new(stream: Stream) -> Self {
        Iap2 {
            stream,
            buffer: Vec::new(),
        }
    }
}

impl futures::Stream for Iap2 {
    type Item = Packet;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Packet>> {
        let this = self.get_mut();

        loop {
            if let Some(pkt) = parse_from_buffer(&mut this.buffer) {
                return Poll::Ready(Some(pkt));
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

fn parse_from_buffer(buf: &mut Vec<u8>) -> Option<Packet> {
    let ((rest, remaining), packet) = Packet::parse_from_bytes(buf).ok()?;
    buf.drain(..remaining);
    Some(packet)
}
