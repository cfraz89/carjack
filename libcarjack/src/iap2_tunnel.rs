//! iAP2 over the AirPlay control channel.
//!
//! CarPlay does not end the iAP2 session when it hands over to WiFi - it moves it. Once AirPlay is
//! up, iAP2 frames travel inside `iAPSendMessage` commands on the control channel, and the
//! accessory drops the Bluetooth link (the phone asks it to, with `disableBluetooth`). So the
//! `Connection reset by peer` on the RFCOMM channel a few seconds after the WiFi credentials is
//! the handover working; the control channel only *ends* if nobody picks it up here.
//!
//! This is a byte pipe, not a protocol: `Iap2` still owns framing, sequencing and acknowledgement.
//! One `iAPSendMessage` is not guaranteed to be one iAP2 frame, so the pipe is a stream and the
//! link layer re-frames it exactly as it does over RFCOMM.

use catplay_carplay::carplay_tx::{AirPlayTransmitter, AirPlayTransmitterProxyRef};
use catplay_carplay::msg::{Command, CommandIApSendMessage};
use iap2_proto::iap2::Iap2;
use deku::prelude::*;
use iap2_proto::ops::control::ControlMessage;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;

/// How long catplay may take to hand an outgoing command to the car before we give up on it.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Buffer for each direction of the pipe. Generous: iAP2 frames are small, and a stall here would
/// surface as a link-layer retransmit rather than backpressure the AirPlay event loop.
const PIPE_CAPACITY: usize = 64 * 1024;

/// The end of the tunnel an [`Iap2`] session runs on.
pub type TunnelStream = DuplexStream;

/// Wire an iAP2 byte pipe onto the AirPlay control channel.
///
/// Returns the stream to run a session on, and the sender that incoming `iAPSendMessage` payloads
/// must be pushed into - see [`Tunnel::deliver`].
pub fn open(tx: AirPlayTransmitterProxyRef) -> (TunnelStream, Tunnel) {
    let (ours, theirs) = tokio::io::duplex(PIPE_CAPACITY);
    let (inbound_tx, inbound_rx) = mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(pump(theirs, inbound_rx, tx));
    (ours, Tunnel { inbound: inbound_tx })
}

/// Handle for feeding the tunnel the payloads the car sends us.
#[derive(Clone)]
pub struct Tunnel {
    inbound: mpsc::Sender<Vec<u8>>,
}

impl Tunnel {
    /// Push one `iAPSendMessage` payload in. Dropping on a full channel is deliberate: the link
    /// layer retransmits, and blocking here would stall the whole AirPlay command handler.
    pub fn deliver(&self, data: Vec<u8>) {
        if self.inbound.try_send(data).is_err() {
            tracing::warn!("iAP2 tunnel inbound full; dropping a frame");
        }
    }
}

/// Move bytes between the pipe and the AirPlay control channel until either end goes away.
async fn pump(
    mut pipe: DuplexStream,
    mut inbound: mpsc::Receiver<Vec<u8>>,
    tx: AirPlayTransmitterProxyRef,
) {
    let mut out = vec![0u8; 4096];
    loop {
        tokio::select! {
            // Ours -> car.
            read = pipe.read(&mut out) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let cmd = Command::IApSendMessage(CommandIApSendMessage {
                        data: out[..n].to_vec().into(),
                    });
                    match tx.send_command_noresp(&cmd, SEND_TIMEOUT) {
                        Ok(_) => tracing::trace!(bytes = n, "iAP2 tunnel -> car"),
                        Err(e) => {
                            tracing::warn!(?e, "iAP2 tunnel send failed; closing");
                            break;
                        }
                    }
                }
            },
            // Car -> ours.
            frame = inbound.recv() => match frame {
                None => break,
                Some(data) => {
                    tracing::trace!(bytes = data.len(), "iAP2 tunnel <- car");
                    if pipe.write_all(&data).await.is_err() {
                        break;
                    }
                }
            },
        }
    }
    tracing::debug!("iAP2 tunnel closed");
}

/// Bring an iAP2 link up over the tunnel.
///
/// No iAP1 switch: that is an RFCOMM artefact, and the tunnel carries frames directly. Which side
/// sends the SYN over AirPlay is not established from a capture yet, so this waits for the
/// accessory's exactly as the Bluetooth path does.
pub async fn start_session(stream: TunnelStream) -> eyre::Result<Iap2<TunnelStream>> {
    tracing::info!("Bringing iAP2 up over the AirPlay tunnel");
    let iap2 = Iap2::start_established(stream).await?;
    tracing::info!("iAP2 tunnel link established");
    Ok(iap2)
}

/// Service the tunnelled link for the life of the AirPlay session.
///
/// The link layer still needs driving - keepalives, retransmits and acknowledgement timers - or
/// the accessory gives up on us exactly as it does over RFCOMM. Control messages are logged rather
/// than acted on: nothing has yet captured what an accessory sends over this channel, so acting on
/// a guess is how the Bluetooth side wasted a day.
pub async fn run<S>(iap2: Iap2<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    loop {
        tokio::select! {
            received = iap2.receive_packet() => match received {
                Ok(packet) => {
                    if packet.header.control.syn {
                        tracing::warn!("Accessory re-sent SYN on the tunnel; ending");
                        return;
                    }
                    match ControlMessage::from_bytes((&packet.payload, 0)) {
                        Ok((_, msg)) => tracing::info!(
                            id = ?msg.message_id,
                            params = msg.parameters.len(),
                            "iAP2 tunnel: control message"
                        ),
                        Err(_) => tracing::debug!(
                            bytes = packet.payload.len(),
                            "iAP2 tunnel: non-control session data"
                        ),
                    }
                }
                Err(e) => {
                    tracing::warn!(?e, "iAP2 tunnel link ended");
                    return;
                }
            },
            // Pure, cancellation-safe; the transmitting half must not be inside the select.
            () = iap2.wait_for_link_work() => {}
        }
        if let Err(e) = iap2.maintain().await {
            tracing::warn!(?e, "iAP2 tunnel maintenance failed");
            return;
        }
    }
}
