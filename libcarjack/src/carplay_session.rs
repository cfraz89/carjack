use deku::prelude::*;
use iap2_proto::{
    error::Iap2Error,
    iap2::{Iap2, Stream},
    ops::{
        control::{ControlMessage, ControlMessageId, ControlMessageSet, GetParam, ParameterId},
        ea::EaFrame,
    },
    packet::ControlByte,
    stream::SessionType,
};
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

#[derive(Debug, Clone)]
pub struct WifiCredentials {
    pub ssid: String,
    pub passphrase: Option<String>,
    pub bssid: Option<String>,
}

enum SessionPhase {
    Auth,
    WaitingForWifiConfig,
    EaReady,
}

pub struct CarplaySession<S: Stream> {
    pub iap2: Iap2<S>,
    pub wifi_tx: mpsc::Sender<WifiCredentials>,
    phase: SessionPhase,
    ea_session_id: u8,
    ready_tx: Option<oneshot::Sender<()>>,
}

impl<S: Stream> CarplaySession<S> {
    pub fn new(
        iap2: Iap2<S>,
        wifi_tx: mpsc::Sender<WifiCredentials>,
        ready_tx: oneshot::Sender<()>,
    ) -> Self {
        CarplaySession {
            iap2,
            wifi_tx,
            phase: SessionPhase::Auth,
            ea_session_id: 0,
            ready_tx: Some(ready_tx),
        }
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
                .send_packet(ControlByte::with_ack(), 1, Vec::new())
                .await.tap_err(|e| tracing::warn!("Keep-alive send error: {e}"))?
                        }
                    }
                }
        }
    }

    async fn do_identification(&mut self) -> Result<(), Iap2Error> {
        tracing::debug!("Sending StartIdentification");
        let payload = ControlMessage::start_identification()
            .to_bytes()
            .expect("serialize ControlMessage");
        tracing::debug!("StartIdentification payload: {:?}", payload);
        let ctrl = ControlByte {
            ack: true,
            ..Default::default()
        };
        self.iap2.send_packet(ctrl, 1, payload).await?;
        Ok(())
    }

    fn generate_challenge() -> [u8; 20] {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut challenge = [0u8; 20];
        let mut state = seed;
        for byte in challenge.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = (state >> 32) as u8;
        }
        challenge
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
        self.iap2
            .send_packet(ControlByte::with_ack(), 1, payload)
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
                    .get_param::<String>(ParameterId::DISPLAY_NAME)
                    .unwrap_or_else(|| String::from("Unknown"));
                tracing::info!("Accessory identified: {}", name);
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
                tracing::debug!("Received authentication certificate");
                let challenge = Self::generate_challenge();
                self.send_ctrl(ControlMessage::authentication_challenge(&challenge))
                    .await?;
            }
            ControlMessageId::AuthenticationResponse => {
                tracing::info!("Authentication complete");
                self.send_ctrl(ControlMessage::authentication_succeeded())
                    .await?;
                // Request WiFi config first, wait for response
                self.send_ctrl(ControlMessage::request_accessory_wifi_config())
                    .await?;
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
            ControlMessageId::AccessoryWiFiConfigurationInformation => {
                tracing::debug!("Received AccessoryWiFiConfigurationInformation");
                for p in &msg.parameters {
                    let id = p.id.raw();
                    tracing::info!(
                        "  WiFi param 0x{id:04x}: {} bytes: {:02x?}",
                        p.data.len(),
                        p.data
                    );
                }
                if let Some(ssid) = msg.parameters.get_param::<String>(ParameterId::SSID) {
                    let passphrase = msg.parameters.get_param::<String>(ParameterId::PASSPHRASE);
                    let bssid = msg.parameters.get_param::<String>(ParameterId::BSSID);
                    tracing::info!("Accessory WiFi: ssid={}", ssid);
                    if let Some(ref pw) = passphrase {
                        tracing::info!("Accessory WiFi: passphrase={}", pw);
                    }
                    if let Some(ref b) = bssid {
                        tracing::info!("Accessory WiFi: bssid={}", b);
                    }
                    let creds = WifiCredentials {
                        ssid,
                        passphrase,
                        bssid,
                    };
                    let _ = self.wifi_tx.try_send(creds);
                }
                // Now start EA session and signal ready
                let payload = ControlMessage::start_ea_session("com.apple.carplay");
                self.send_ctrl(payload).await?;
                tracing::info!("Started EA session");
                self.phase = SessionPhase::EaReady;
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
