use deku::prelude::*;
use iap2_proto::{
    error::Iap2Error,
    iap2::{Iap2, Stream},
    ops::{
        control::{
            ControlMessage, ControlMessageId, ControlMessageSet, GetParam, Parameter, ParameterId,
        },
        ea::EaFrame,
    },
    packet::ControlByte,
    stream::SessionType,
};
use std::net::{IpAddr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Epoch seconds, minutes east of UTC, DST flag (0; nothing else observed).
fn local_time() -> (u64, u16, u8) {
    let now = chrono::Local::now();
    let secs = now.timestamp().max(0) as u64;
    let offset_minutes = ((now.offset().local_minus_utc() / 60) as i16) as u16;
    (secs, offset_minutes, 0)
}

/// Decoded from whichever wire shape `parse_touch_report` (libcarjack/src/airplay.rs) matched -
/// not itself a wire format, so it carries no deku framing of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchEventType {
    Down,
    Motion,
    Up,
}

#[derive(Debug, Clone)]
pub struct TouchEvent {
    pub event_type: TouchEventType,
    pub slot: u32,
    pub x: f64,
    pub y: f64,
}

/// Auth coprocessor 3.0 (ECC) signs a 32-byte SHA-256 digest; 2.0C (RSA) a 20-byte SHA-1 one.
const SHA256_CHALLENGE_LEN: usize = 32;
const SHA1_CHALLENGE_LEN: usize = 20;

/// OID prefix 1.2.840.10045 (ansi-X9-62, elliptic curve), as encoded in a DER OID body.
const OID_ANSI_X962_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d];

/// The challenge length must match the certificate's key type, or the link dies.
fn challenge_len_for_certificate(certificate: &[u8]) -> usize {
    if certificate
        .windows(OID_ANSI_X962_EC.len())
        .any(|window| window == OID_ANSI_X962_EC)
    {
        SHA256_CHALLENGE_LEN
    } else {
        SHA1_CHALLENGE_LEN
    }
}

const MSG_DEVICE_INFORMATION_UPDATE: u16 = 0x4E09;
const MSG_REQUEST_WIFI_CONFIG_GROUPED: u16 = 0x4300;

#[derive(Debug, Clone)]
pub struct WifiCredentials {
    pub ssid: String,
    pub passphrase: Option<String>,
    pub bssid: Option<String>,
    /// AirPlay receiver for when we're on its AP; 0x4301 names one, the flat 0x5703 dialect
    /// doesn't. Handed over rather than discovered: some accessories never browse for ours. Missing
    /// the interface's scope id, which only the caller can attach.
    pub airplay: Option<SocketAddr>,
    /// HomeKit pairing id when named; the invitation equivalent, and the only route to
    /// pair-verify with an accessory that never invites.
    pub pairing_id: Option<Uuid>,
}

/// Port to reach AirPlay on when the accessory named an address but no port.
const DEFAULT_AIRPLAY_PORT: u16 = 7000;

/// Who we tell the accessory we are. An iPhone puts its Bluetooth address in the second
/// WiFi-config group and joins the AP with an unrelated randomised MAC, so that group identifies
/// the bond rather than the radio.
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub name: String,
    pub transport_identifier: String,
    pub bluetooth_address: String,
}

enum SessionPhase {
    Auth,
    WaitingForWifiConfig,
    EaReady,
}

/// The iAP2 side of wireless CarPlay bring-up: identify, authenticate, exchange WiFi credentials,
/// open the EA session, then hold the link (keepalives, control traffic) until handover.
pub struct CarplayInit<S: Stream> {
    pub iap2: Iap2<S>,
    pub wifi_tx: mpsc::Sender<WifiCredentials>,
    identity: DeviceIdentity,
    phase: SessionPhase,
    ea_session_id: u8,
    /// Messages the accessory declared it receives; empty means fall back to the older dialect.
    accepted_messages: Vec<u16>,
    /// How long a shot we sent, so a rejection can name it.
    challenge_len: usize,
    ready_tx: Option<oneshot::Sender<()>>,
    /// Fired once handover is done: the point after which a link going away is the expected
    /// ending rather than a fault.
    handover: CancellationToken,
}

impl<S: Stream> CarplayInit<S> {
    pub fn new(
        iap2: Iap2<S>,
        wifi_tx: mpsc::Sender<WifiCredentials>,
        ready_tx: oneshot::Sender<()>,
        identity: DeviceIdentity,
        handover: CancellationToken,
    ) -> Self {
        CarplayInit {
            iap2,
            wifi_tx,
            identity,
            phase: SessionPhase::Auth,
            ea_session_id: 0,
            accepted_messages: Vec::new(),
            challenge_len: 0,
            ready_tx: Some(ready_tx),
            handover,
        }
    }

    fn accepts(&self, message_id: u16) -> bool {
        self.accepted_messages.contains(&message_id)
    }

    pub async fn run(&mut self) -> Result<(), Iap2Error> {
        let result = self.run_inner().await;
        // A proper goodbye, or the accessory waits out its retransmit timeout before it will
        // re-SYN. Skipped when it reset or after handover, where the link is already gone.
        if !matches!(&result, Err(Iap2Error::LinkReset)) && !self.handover.is_cancelled() {
            let _ = self.iap2.send_reset().await;
        }
        result
    }

    async fn run_inner(&mut self) -> Result<(), Iap2Error> {
        self.do_identification().await?;

        let mut handed_over = false;
        loop {
            tokio::select! {
                packet = self.iap2.receive_packet() => {
                    let packet = match packet {
                        Ok(packet) => packet,
                        // After handover the link going away *is* the ending, however it goes - the accessory
                        // drops the RFCOMM channel when AirPlay connects (ECONNRESET, not an iAP2
                        // reset). Reporting it would make a normal teardown look like a fault.
                        Err(e) if handed_over => {
                            tracing::info!("Accessory closed the iAP2 link after the handover ({e})");
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                    };
                    // `receive_packet` absorbed the link-layer traffic (acknowledgements, resets);
                    // what's left here is all session data.
                    if packet.header.control.syn {
                        // Renegotiation can't happen under a running session; fold back to a restart.
                        tracing::warn!("Accessory re-sent SYN mid-session; restarting");
                        return Err(Iap2Error::LinkReset);
                    }
                    if let Err(e) = self.dispatch_packet(packet).await {
                        tracing::warn!("Dispatch error: {e}");
                    }
                }
                // Keepalives paced by the SYN negotiation; only the wait lives in the `select!`, so
                // the work isn't cancelled half-done.
                _ = self.iap2.wait_for_link_work() => {
                    self.iap2.maintain().await?;
                }
                // Only stops the post-handover drop being reported as a failure; the arm disables
                // itself, since a cancelled token is ready forever.
                _ = self.handover.cancelled(), if !handed_over => {
                    handed_over = true;
                    tracing::info!("Handover complete; the accessory drops Bluetooth from here");
                }
            }
        }
    }

    async fn do_identification(&mut self) -> Result<(), Iap2Error> {
        tracing::debug!("Sending StartIdentification");
        self.send_ctrl(ControlMessage::start_identification()).await
    }

    /// Send a challenge of `len` bytes and remember the size so a rejection can name it.
    async fn send_challenge(&mut self, len: usize) -> Result<(), Iap2Error> {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut challenge = vec![0u8; len];
        let mut state = seed;
        for byte in challenge.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = (state >> 32) as u8;
        }
        self.challenge_len = len;
        self.send_ctrl(ControlMessage::authentication_challenge(&challenge))
            .await
    }

    async fn dispatch_packet(
        &mut self,
        packet: iap2_proto::packet::Packet,
    ) -> Result<(), Iap2Error> {
        let session_id = packet.header.session_id;
        let session_table = self.iap2.session_table.lock().await;
        let entry = session_table.get(session_id).cloned();
        drop(session_table);

        match entry {
            Some(entry) if entry.session_type == SessionType::Control => {
                self.handle_control(&packet.payload).await
            }
            Some(entry) if entry.session_type == SessionType::ExternalAccessory => {
                self.handle_ea(&packet.payload).await
            }
            _ => {
                tracing::warn!(session_id, "Unknown session");
                Ok(())
            }
        }
    }

    async fn handle_control(&mut self, payload: &[u8]) -> Result<(), Iap2Error> {
        tracing::trace!("Control payload size: {0}", payload.len());
        let (_, set) = ControlMessageSet::from_bytes((payload, 0))?;
        tracing::trace!("{:?}", set);
        for msg in set.messages {
            self.handle_control_message(msg).await?;
        }
        Ok(())
    }

    async fn send_ctrl(&self, message: ControlMessage) -> Result<(), Iap2Error> {
        let payload = message.to_bytes()?;
        let session_id = self.iap2.control_session_id().await?;
        self.iap2
            .send_packet(ControlByte::with_ack(), session_id, payload)
            .await
    }

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<(), Iap2Error> {
        tracing::debug!(
            "Control message: {:?}, {} params",
            msg.message_id,
            msg.parameters.len()
        );

        match msg.message_id {
            ControlMessageId::IdentificationInformation => {
                let name = msg
                    .parameters
                    .get_param::<String>(ParameterId::ACCESSORY_NAME)
                    .or_else(|| {
                        msg.parameters
                            .get_param::<String>(ParameterId::DISPLAY_NAME)
                    })
                    .unwrap_or_else(|| String::from("Unknown"));
                tracing::info!("Accessory identified: {}", name);
                // The WiFi-config dialect is per-accessory; empty list means the flat 0x5702 behaviour.
                self.accepted_messages = msg
                    .parameters
                    .get_param::<Vec<u16>>(ParameterId::MESSAGES_RECEIVED_FROM_DEVICE)
                    .unwrap_or_default();
                tracing::debug!(
                    "Accessory accepts message ids: {:04x?}",
                    self.accepted_messages
                );
                self.send_ctrl(ControlMessage::identification_accepted())
                    .await?;
                self.send_ctrl(ControlMessage::request_auth_certificate_serial())
                    .await?;
            }
            ControlMessageId::AccessoryAuthenticationSerialNumber => {
                tracing::debug!("Received accessory serial number");
                self.send_ctrl(ControlMessage::request_auth_certificate())
                    .await?;
            }
            ControlMessageId::AuthenticationCertificate => {
                let certificate = msg
                    .parameters
                    .get_param::<Vec<u8>>(ParameterId::AUTH_DATA)
                    .unwrap_or_default();
                let len = challenge_len_for_certificate(&certificate);
                tracing::debug!(
                    "Received authentication certificate ({} bytes, auth IC {}); challenging \
                     with {} bytes",
                    certificate.len(),
                    if len == SHA256_CHALLENGE_LEN {
                        "3.0 (ECC)"
                    } else {
                        "2.0C (RSA)"
                    },
                    len
                );
                self.send_challenge(len).await?;
            }
            ControlMessageId::AuthenticationResponse => {
                // An empty reply means the accessory couldn't sign; acknowledging it would leave it
                // unauthenticated and silent - looks exactly like a hang - so bail.
                let signature = msg
                    .parameters
                    .get_param::<Vec<u8>>(ParameterId::AUTH_DATA)
                    .unwrap_or_default();
                if signature.is_empty() {
                    tracing::error!(
                        "Accessory returned an empty AuthenticationResponse to our {}-byte \
                         challenge - it cannot sign it. Not sending AuthenticationSucceeded.",
                        self.challenge_len
                    );
                    return Ok(());
                }
                tracing::info!(
                    "Authentication complete ({} byte signature)",
                    signature.len()
                );
                self.send_ctrl(ControlMessage::authentication_succeeded())
                    .await?;
                // Only declared accessories want a DeviceInformationUpdate; unsolicited, it drops the session.
                if self.accepts(MSG_DEVICE_INFORMATION_UPDATE) {
                    self.send_ctrl(ControlMessage::device_information_update(
                        &self.identity.name,
                    ))
                    .await?;
                }
                if self.accepts(MSG_REQUEST_WIFI_CONFIG_GROUPED) {
                    tracing::debug!("Requesting WiFi config (0x4300, grouped)");
                    self.send_ctrl(ControlMessage::request_accessory_wifi_config_grouped(
                        &self.identity.transport_identifier,
                        &self.identity.bluetooth_address,
                    ))
                    .await?;
                } else {
                    tracing::debug!("Requesting WiFi config (0x5702, flat)");
                    self.send_ctrl(ControlMessage::request_accessory_wifi_config())
                        .await?;
                }
                self.phase = SessionPhase::WaitingForWifiConfig;
            }
            ControlMessageId::RequestAppLaunch
            | ControlMessageId::StatusExternalAccessoryProtocolSession => {
                tracing::info!("Received EA session request");
                self.ea_session_id = match msg
                    .parameters
                    .get_param::<u32>(ParameterId::SESSION_IDENTIFIER)
                {
                    Some(v) if v <= 0xFF => v as u8,
                    _ => 2,
                };
                let payload = ControlMessage::start_ea_session("com.apple.carplay");
                self.send_ctrl(payload).await?;
                tracing::info!("Started EA session");
                self.phase = SessionPhase::EaReady;
                if let Some(tx) = self.ready_tx.take() {
                    // Exchange over; the link goes quiet for AirPlay, so keepalives start now.
                    self.iap2.set_keepalive_enabled(true).await;
                    let _ = tx.send(());
                }
            }
            ControlMessageId::AccessoryWiFiConfigurationInformation
            | ControlMessageId::AccessoryWiFiConfigurationInformationGrouped => {
                tracing::debug!("Received {:?}", msg.message_id);
                let grouped = msg.message_id
                    == ControlMessageId::AccessoryWiFiConfigurationInformationGrouped;
                let creds = if grouped {
                    // 0x4301: credentials nested at top-level parameter 1, the accessory's MAC,
                    // AirPlay port and firmware version beside it.
                    let group = msg
                        .parameters
                        .get_param::<Vec<Parameter>>(ParameterId::WIFI_CONFIG_GROUP)
                        .unwrap_or_default();
                    let port = msg
                        .parameters
                        .get_param::<u32>(ParameterId::WIFI_CONFIG_PORT)
                        .and_then(|p| u16::try_from(p).ok())
                        .unwrap_or(DEFAULT_AIRPLAY_PORT);
                    // The address is a text link-local inside the group; scope id is attached once the
                    // caller knows the interface.
                    let airplay = group
                        .get_param::<String>(ParameterId::WIFI_CONFIG_IPV6)
                        .and_then(|a| match a.parse::<IpAddr>() {
                            Ok(ip) => Some(SocketAddr::new(ip, port)),
                            Err(e) => {
                                tracing::warn!("Accessory AirPlay address {a:?} unparseable: {e}");
                                None
                            }
                        });
                    match airplay {
                        Some(addr) => tracing::info!("Accessory AirPlay endpoint: {addr}"),
                        None => tracing::warn!(
                            "Accessory gave no AirPlay address; will have to discover it"
                        ),
                    }
                    if let Some(v) = msg
                        .parameters
                        .get_param::<String>(ParameterId::WIFI_CONFIG_VERSION)
                    {
                        tracing::debug!("Accessory firmware version: {}", v);
                    }
                    if let Some(ch) = group.get_param::<u8>(ParameterId::WIFI_CONFIG_CHANNEL) {
                        tracing::info!("Accessory WiFi: channel={}", ch);
                    }
                    let pairing_id = msg
                        .parameters
                        .get_param::<String>(ParameterId::WIFI_CONFIG_UUID)
                        .and_then(|u| Uuid::parse_str(&u).ok());
                    group
                        .get_param::<String>(ParameterId::WIFI_CONFIG_SSID)
                        .map(|ssid| WifiCredentials {
                            ssid,
                            passphrase: group
                                .get_param::<String>(ParameterId::WIFI_CONFIG_PASSPHRASE),
                            bssid: msg
                                .parameters
                                .get_param::<String>(ParameterId::WIFI_CONFIG_MAC),
                            airplay,
                            pairing_id,
                        })
                } else {
                    // 0x5703: flat. Parameters 3/4 are one byte each and not a BSSID; nothing
                    // downstream consumes one, so don't invent it.
                    msg.parameters
                        .get_param::<String>(ParameterId::SSID)
                        .map(|ssid| WifiCredentials {
                            ssid,
                            passphrase: msg.parameters.get_param::<String>(ParameterId::PASSPHRASE),
                            bssid: None,
                            // No address in this dialect; the receiver gets discovered over mDNS.
                            airplay: None,
                            pairing_id: None,
                        })
                };
                if let Some(creds) = creds {
                    tracing::info!("Accessory WiFi: ssid={}", creds.ssid);
                    if let Some(ref pw) = creds.passphrase {
                        tracing::info!("Accessory WiFi: passphrase={}", pw);
                    }
                    if let Some(ref b) = creds.bssid {
                        tracing::info!("Accessory WiFi: bssid={}", b);
                    }
                    let _ = self.wifi_tx.try_send(creds);
                } else {
                    tracing::warn!("{:?} carried no SSID", msg.message_id);
                    for p in &msg.parameters {
                        let id = p.id.raw();
                        tracing::info!(
                            "  WiFi param 0x{id:04x}: {} bytes: {:02x?}",
                            p.data.len(),
                            p.data
                        );
                    }
                }
                // Eager handshake once it has the configuration; only the first time. Nothing else goes here -
                // we don't start the EA session unprompted; that happens only when the accessory
                // asks (`RequestAppLaunch`/`StatusExternalAccessoryProtocolSession`).
                if !matches!(self.phase, SessionPhase::EaReady) {
                    let (secs, offset_minutes, dst) = local_time();
                    self.send_ctrl(ControlMessage::device_time_update(
                        secs,
                        offset_minutes,
                        dst,
                    ))
                    .await?;
                    self.send_ctrl(ControlMessage::device_language_update("en"))
                        .await?;
                    self.send_ctrl(ControlMessage::wireless_carplay_update(true))
                        .await?;
                    tracing::info!("Sent time, language and wireless-CarPlay updates");
                    self.phase = SessionPhase::EaReady;
                }
                if let Some(tx) = self.ready_tx.take() {
                    // Exchange over; the link goes quiet for AirPlay, so keepalives start now.
                    self.iap2.set_keepalive_enabled(true).await;
                    let _ = tx.send(());
                }
            }
            ControlMessageId::RequestWiFiInformation => {
                tracing::info!("Accessory requested WiFi information");
                if let Some(ssid) = msg.parameters.get_param::<String>(ParameterId::SSID) {
                    let passphrase = msg
                        .parameters
                        .get_param::<String>(ParameterId::PASSPHRASE)
                        .unwrap_or_default();
                    let bssid = msg.parameters.get_param::<String>(ParameterId::BSSID);
                    let creds = WifiCredentials {
                        ssid,
                        passphrase: Some(passphrase),
                        bssid,
                        airplay: None,
                        pairing_id: None,
                    };
                    let _ = self.wifi_tx.send(creds).await;
                }
                let payload = ControlMessage::wifi_information("CarJack-WiFi", "carjack123");
                self.send_ctrl(payload).await?;
            }
            ControlMessageId::StartNowPlayingUpdates => {
                tracing::debug!("Accessory requested now playing updates");
                self.send_ctrl(ControlMessage::now_playing_update()).await?;
            }
            ControlMessageId::WirelessCarPlayUpdate => {
                tracing::debug!("Received WirelessCarPlayUpdate");
                for p in &msg.parameters {
                    let id = p.id.raw();
                    tracing::info!(
                        "  WirelessCarPlay param 0x{id:04x}: {:?} bytes",
                        p.data.len()
                    );
                }
                self.send_ctrl(ControlMessage::wireless_carplay_update(true))
                    .await?;
            }
            ControlMessageId::BluetoothComponentInformation => {
                tracing::debug!("Received BluetoothComponentInformation");
            }
            ControlMessageId::DeviceInformationUpdate => {
                tracing::debug!("Received DeviceInformationUpdate");
            }
            ControlMessageId::DeviceTimeUpdate
            | ControlMessageId::DeviceLanguageUpdate
            | ControlMessageId::DeviceUUIDUpdate => {
                tracing::debug!("Received device info update: {:?}", msg.message_id);
            }
            _ => {
                tracing::debug!("Unhandled control message: {:?}", msg.message_id);
            }
        }
        Ok(())
    }

    async fn handle_ea(&mut self, payload: &[u8]) -> Result<(), Iap2Error> {
        // EA frames once carried touch, but touch actually arrives over the AirPlay HID channel
        // (see libcarjack::airplay); these are now parsed and acknowledged, nothing more.
        let _ = EaFrame::from_bytes(payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A captured RSA-1024 / `sha1WithRSAEncryption` certificate prefix (auth coprocessor 2.0C).
    const RSA_CERTIFICATE_PREFIX: &str = concat!(
        "3082038806092a864886f70d010702a0820379308203750201013100300b",
        "06092a864886f70d010701a082035d3082035930820241a003020102020f2222",
        "aa180511aa06aa5392aa151525300d06092a864886f70d010105050030818331",
        "0b300906035504061302555331133011060355040a130a4170706c6520496e63",
        "2e31263024060355040b131d4170706c",
    );

    /// A captured ECC / `ecdsa-with-SHA256` certificate prefix (auth coprocessor 3.0).
    const ECC_CERTIFICATE_PREFIX: &str = concat!(
        "3082025d06092a864886f70d010702a082024e3082024a0201013100300b",
        "06092a864886f70d010701a08202303082022c308201d1a003020102021024fb",
        "0606902e707205d314b0059a193d300a06082a8648ce3d040302308189310b30",
        "0906035504061302555331133011060355040a130a4170706c6520496e632e31",
        "263024060355040b131d4170706c6520",
    );

    #[test]
    fn challenge_length_follows_certificate_key_type() {
        assert_eq!(
            challenge_len_for_certificate(&unhex(RSA_CERTIFICATE_PREFIX)),
            SHA1_CHALLENGE_LEN
        );
        assert_eq!(
            challenge_len_for_certificate(&unhex(ECC_CERTIFICATE_PREFIX)),
            SHA256_CHALLENGE_LEN
        );
    }

    /// A certificate we failed to read must not get the length that hangs the older hardware.
    #[test]
    fn missing_certificate_falls_back_to_the_shorter_challenge() {
        assert_eq!(challenge_len_for_certificate(&[]), SHA1_CHALLENGE_LEN);
    }
}
