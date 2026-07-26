use deku::prelude::*;
use iap2_proto::{
    error::Iap2Error,
    iap2::{Iap2, Stream, LINK_SESSION_ID},
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tap::prelude::*;
use tokio::sync::{mpsc, oneshot};

#[derive(DekuRead, Debug, Clone, Copy, PartialEq, Eq)]
#[deku(id_type = "u8")]
pub enum TouchEventType {
    #[deku(id = "0")]
    Down,
    #[deku(id = "1")]
    Motion,
    #[deku(id_pat = "2..=3")]
    Up,
}

#[derive(DekuRead, Debug, Clone)]
pub struct TouchEvent {
    pub event_type: TouchEventType,
    #[deku(endian = "little")]
    pub slot: u32,
    #[deku(
        map = "|x: u16| -> Result<f64, deku::DekuError> { Ok(f64::from(x)) }",
        endian = "big"
    )]
    pub x: f64,
    #[deku(
        map = "|y: u16| -> Result<f64, deku::DekuError> { Ok(f64::from(y)) }",
        endian = "big"
    )]
    pub y: f64,
}

/// Apple auth coprocessor 3.0 signs a 32-byte SHA-256 digest; 2.0C signs a 20-byte SHA-1 one.
const SHA256_CHALLENGE_LEN: usize = 32;
const SHA1_CHALLENGE_LEN: usize = 20;

/// OID prefix 1.2.840.10045 (ansi-X9-62, elliptic curve), as it appears in a DER OID body.
const OID_ANSI_X962_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d];

/// Pick the challenge digest length from the accessory's certificate.
///
/// Need to match challenge length with certificate type otherwise the connection conks out
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
    /// Where to reach the accessory's AirPlay receiver once we are on its AP, when it tells us
    /// (0x4301 carries an address and port; the older flat 0x5703 does not).
    ///
    /// Worth using rather than discovering: the CFMOTO unit hands this over and then simply
    /// waits - it never browses mDNS for our `_carplay-ctrl._tcp` and never sends the
    /// `ctrl-int/1/connect` invite, so whoever waits for the other to speak first waits
    /// forever. A link-local address here still needs a scope id, which only the caller (who
    /// knows the interface) can attach.
    pub airplay: Option<SocketAddr>,
}

/// Port to reach AirPlay on when the accessory named an address but no port.
const DEFAULT_AIRPLAY_PORT: u16 = 7000;

/// Who we tell the accessory we are, once authentication has succeeded.
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    /// Sent as `DeviceInformationUpdate` (0x4E09); an iPhone sends its device name here.
    pub name: String,
    /// Identifies our Bluetooth transport in `RequestAccessoryWiFiConfigurationInformation`.
    pub transport_identifier: String,
    /// The station address that will join the accessory's AP.
    pub wifi_mac: String,
}

enum SessionPhase {
    Auth,
    WaitingForWifiConfig,
    EaReady,
}

pub struct CarplaySession<S: Stream> {
    pub iap2: Iap2<S>,
    pub wifi_tx: mpsc::Sender<WifiCredentials>,
    identity: DeviceIdentity,
    phase: SessionPhase,
    ea_session_id: u8,
    /// `MessagesReceivedFromDevice` from the accessory's IdentificationInformation. Empty
    /// when it didn't declare one, in which case we fall back to the older dialect.
    accepted_messages: Vec<u16>,
    /// Length of the authentication challenge we sent, so a rejection names it.
    challenge_len: usize,
    ready_tx: Option<oneshot::Sender<()>>,
}

impl<S: Stream> CarplaySession<S> {
    pub fn new(
        iap2: Iap2<S>,
        wifi_tx: mpsc::Sender<WifiCredentials>,
        ready_tx: oneshot::Sender<()>,
        identity: DeviceIdentity,
    ) -> Self {
        CarplaySession {
            iap2,
            wifi_tx,
            identity,
            phase: SessionPhase::Auth,
            ea_session_id: 0,
            accepted_messages: Vec::new(),
            challenge_len: 0,
            ready_tx: Some(ready_tx),
        }
    }

    fn accepts(&self, message_id: u16) -> bool {
        self.accepted_messages.contains(&message_id)
    }

    pub async fn run(&mut self) -> Result<(), Iap2Error> {
        self.do_identification().await?;

        // The accessory's SYN handshake negotiates a cumulative-ack timeout (typically
        // ~1s x 6 = ~6s); if we go that long without sending anything, it considers the
        // link dead and resets the Bluetooth connection - which otherwise happens right
        // in the middle of the (silent, on this channel) AirPlay/screen-sharing phase.
        let mut keepalive = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                    packet = self.iap2.receive_packet() => {
                        let packet = packet?;
                        if !packet.header.control.ack || packet.payload.is_empty() {
                            continue;
                        }
                        if let Err(e) = self.dispatch_packet(packet).await {
                            tracing::warn!("Dispatch error: {e}");
                        }
                    }
                    _ = keepalive.tick() => {
                        // Only once the identification/auth/wifi-config handshake has
                        // settled (ready_tx already fired) - interleaving an out-of-band
                        // packet during that sequence confuses some accessories.
                        if self.ready_tx.is_none() {
            self.iap2
                .send_packet(ControlByte::with_ack(), LINK_SESSION_ID, Vec::new())
                .await.tap_err(|e| tracing::warn!("Keep-alive send error: {e}"))?
                        }
                    }
                }
        }
    }

    async fn do_identification(&mut self) -> Result<(), Iap2Error> {
        tracing::debug!("Sending StartIdentification");
        self.send_ctrl(ControlMessage::start_identification()).await
    }

    /// Send an authentication challenge of `len` bytes, remembering the size so a rejection
    /// can name it. Pick `len` with [`challenge_len_for_certificate`].
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
                // Head units disagree about which WiFi-config dialect they speak, so take the
                // accessory at its word instead of guessing. Absent the declaration we keep the
                // 0x5702 behaviour that the AUTO BOX bench unit is known to answer.
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
                // An accessory that could not sign our challenge answers with an empty
                // AuthenticationResponse rather than AuthenticationFailed. Acknowledging that
                // with AuthenticationSucceeded leaves the accessory unauthenticated and
                // silent, which looks exactly like a hang - so check before proceeding.
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
                // Only the CFMOTO-style units want a DeviceInformationUpdate here; sending one
                // to a unit that never asked for it is the sort of thing that gets a session
                // dropped, so gate it on the declared list.
                if self.accepts(MSG_DEVICE_INFORMATION_UPDATE) {
                    self.send_ctrl(ControlMessage::device_information_update(
                        &self.identity.name,
                    ))
                    .await?;
                }
                // Ask for the AP credentials in whichever dialect this accessory speaks.
                if self.accepts(MSG_REQUEST_WIFI_CONFIG_GROUPED) {
                    tracing::debug!("Requesting WiFi config (0x4300, grouped)");
                    self.send_ctrl(ControlMessage::request_accessory_wifi_config_grouped(
                        &self.identity.transport_identifier,
                        &self.identity.wifi_mac,
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
                    let _ = tx.send(());
                }
            }
            ControlMessageId::AccessoryWiFiConfigurationInformation
            | ControlMessageId::AccessoryWiFiConfigurationInformationGrouped => {
                tracing::debug!("Received {:?}", msg.message_id);
                let grouped = msg.message_id
                    == ControlMessageId::AccessoryWiFiConfigurationInformationGrouped;
                let creds = if grouped {
                    // 0x4301: credentials nested in the group at top-level parameter 1, with
                    // the accessory's MAC, AirPlay port and firmware version beside it.
                    let group = msg
                        .parameters
                        .get_param::<Vec<Parameter>>(ParameterId::WIFI_CONFIG_GROUP)
                        .unwrap_or_default();
                    let port = msg
                        .parameters
                        .get_param::<u32>(ParameterId::WIFI_CONFIG_PORT)
                        .and_then(|p| u16::try_from(p).ok())
                        .unwrap_or(DEFAULT_AIRPLAY_PORT);
                    // The address lives inside the group, as a text link-local (e.g.
                    // "fe80::9083:8eff:feef:a7d3"). Parsing it here keeps the wire format in
                    // one place; the scope id gets attached once we know the interface.
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
                        })
                } else {
                    // 0x5703: flat parameters. Parameters 3 and 4 are a single byte each and
                    // are not a BSSID (the AUTO BOX sends 0x02 and 0x24); nothing downstream
                    // uses one, so don't invent it.
                    msg.parameters
                        .get_param::<String>(ParameterId::SSID)
                        .map(|ssid| WifiCredentials {
                            ssid,
                            passphrase: msg.parameters.get_param::<String>(ParameterId::PASSPHRASE),
                            bssid: None,
                            // This dialect carries no address, so the AUTO BOX path keeps
                            // discovering the receiver over mDNS exactly as before.
                            airplay: None,
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
                // Now start EA session and signal ready - but only the first time. The
                // accessory repeats its WiFi configuration every ~3s until we show up on its
                // AP (an iPhone gets it twice and answers neither, `iphone-cfmoto.pklg`), and
                // answering each repeat with another EA session start sent it eight times in
                // `cfmoto_10.pcapng`.
                if !matches!(self.phase, SessionPhase::EaReady) {
                    let payload = ControlMessage::start_ea_session("com.apple.carplay");
                    self.send_ctrl(payload).await?;
                    tracing::info!("Started EA session");
                    self.phase = SessionPhase::EaReady;
                }
                if let Some(tx) = self.ready_tx.take() {
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
                self.send_ctrl(ControlMessage::wireless_carplay_update())
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
        // Receive EA (External Accessory) session frames. Touch used to be decoded here, but it
        // actually arrives over the AirPlay HID channel (see libcarjack::airplay), so these
        // frames are currently just parsed and acknowledged.
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

    /// First 142 bytes of the AUTO BOX bench unit's AuthenticationCertificate (0xAA01),
    /// captured in `bench_1.pcapng` frame 386. RSA-1024 / `sha1WithRSAEncryption`, issued by
    /// the "Apple iPod Accessories Certification Authority" - an auth coprocessor 2.0C.
    const BENCH_CERTIFICATE_PREFIX: &str = concat!(
        "3082038806092a864886f70d010702a0820379308203750201013100300b",
        "06092a864886f70d010701a082035d3082035930820241a003020102020f2222",
        "aa180511aa06aa5392aa151525300d06092a864886f70d010105050030818331",
        "0b300906035504061302555331133011060355040a130a4170706c6520496e63",
        "2e31263024060355040b131d4170706c",
    );

    /// The same prefix of the CFMOTO bike's certificate, from `cfmoto_7.pcapng` frame 829.
    /// ECC / `ecdsa-with-SHA256`, issued by "Apple Accessories Certification Authority -
    /// 00000002" - an auth coprocessor 3.0.
    const BIKE_CERTIFICATE_PREFIX: &str = concat!(
        "3082025d06092a864886f70d010702a082024e3082024a0201013100300b",
        "06092a864886f70d010701a08202303082022c308201d1a003020102021024fb",
        "0606902e707205d314b0059a193d300a06082a8648ce3d040302308189310b30",
        "0906035504061302555331133011060355040a130a4170706c6520496e632e31",
        "263024060355040b131d4170706c6520",
    );

    #[test]
    fn challenge_length_follows_certificate_key_type() {
        assert_eq!(
            challenge_len_for_certificate(&unhex(BENCH_CERTIFICATE_PREFIX)),
            SHA1_CHALLENGE_LEN
        );
        assert_eq!(
            challenge_len_for_certificate(&unhex(BIKE_CERTIFICATE_PREFIX)),
            SHA256_CHALLENGE_LEN
        );
    }

    /// A unit whose certificate we failed to read at all must not get the length that hangs
    /// the older hardware.
    #[test]
    fn missing_certificate_falls_back_to_the_shorter_challenge() {
        assert_eq!(challenge_len_for_certificate(&[]), SHA1_CHALLENGE_LEN);
    }
}
