use deku::prelude::*;
use iap2_proto::{
    error::Iap2Error,
    iap2::{Iap2, Stream},
    ops::control::{
        self, parse_control_payload, ControlMessage, ControlMessageId, GetParam, ParameterId,
    },
    ops::ea::EaFrame,
    packet::ControlByte,
    stream::SessionType,
};
use std::time::{SystemTime, UNIX_EPOCH};
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

enum AuthStep {
    WaitingForSerial,
    WaitingForCertificate,
    WaitingForAuthResponse,
}

pub struct CarplaySession<S: Stream> {
    pub iap2: Iap2<S>,
    pub video_rx: mpsc::Receiver<Vec<u8>>,
    pub touch_tx: mpsc::Sender<TouchEvent>,
    pub wifi_tx: mpsc::Sender<WifiCredentials>,
    auth_step: Option<AuthStep>,
    ea_session_id: u8,
    ea_transfer_id: u16,
    ready_tx: Option<oneshot::Sender<()>>,
}

impl<S: Stream> CarplaySession<S> {
    pub fn new(
        iap2: Iap2<S>,
        video_rx: mpsc::Receiver<Vec<u8>>,
        touch_tx: mpsc::Sender<TouchEvent>,
        wifi_tx: mpsc::Sender<WifiCredentials>,
        ready_tx: oneshot::Sender<()>,
    ) -> Self {
        CarplaySession {
            iap2,
            video_rx,
            touch_tx,
            wifi_tx,
            auth_step: None,
            ea_session_id: 0,
            ea_transfer_id: 0,
            ready_tx: Some(ready_tx),
        }
    }

    pub async fn run(&mut self) -> Result<(), Iap2Error> {
        self.do_identification().await?;

        loop {
            tokio::select! {
                packet = self.iap2.receive_packet() => {
                    let packet = packet?;
                    if !packet.header.control.ack || packet.payload.is_empty() {
                        continue;
                    }
                    self.dispatch_packet(packet).await?;
                }
                video = self.video_rx.recv() => {
                    if self.ea_session_id != 0 {
                        let data = video.ok_or(Iap2Error::ChannelClosed)?;
                        self.send_video_frame(data).await?;
                    }
                }
                else => break,
            }

            if self.ready_tx.is_none() {
                break;
            }
        }
        Ok(())
    }

    async fn do_identification(&mut self) -> Result<(), Iap2Error> {
        tracing::debug!("Sending StartIdentification");
        let payload = control::make_start_identification();
        let ctrl = ControlByte {
            ack: true,
            ..Default::default()
        };
        self.iap2.send_packet(ctrl, 0, payload).await?;
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
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
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
        let messages = parse_control_payload(payload);
        for msg in messages {
            self.handle_control_message(msg).await?;
        }
        Ok(())
    }

    async fn send_ctrl(&self, payload: Vec<u8>) -> Result<(), Iap2Error> {
        let ctrl = ControlByte {
            ack: true,
            ..Default::default()
        };
        self.iap2.send_packet(ctrl, 0, payload).await
    }

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<(), Iap2Error> {
        let params = msg.parameters();
        tracing::debug!(
            "Control message: {:?}, {} params",
            msg.message_id,
            params.len()
        );

        match msg.message_id {
            ControlMessageId::IdentificationInformation => {
                let name = params
                    .get_param::<String>(ParameterId::DISPLAY_NAME)
                    .unwrap_or_else(|| String::from("Unknown"));
                tracing::info!("Accessory identified: {}", name);
                self.send_ctrl(control::make_identification_accepted()).await?;
                self.send_ctrl(control::make_request_auth_certificate_serial()).await?;
                self.auth_step = Some(AuthStep::WaitingForSerial);
            }
            ControlMessageId::AccessoryAuthenticationSerialNumber => {
                tracing::debug!("Received accessory serial number");
                self.send_ctrl(control::make_request_auth_certificate()).await?;
                self.auth_step = Some(AuthStep::WaitingForCertificate);
            }
            ControlMessageId::AuthenticationCertificate => {
                tracing::debug!("Received authentication certificate");
                let challenge = Self::generate_challenge();
                self.send_ctrl(control::make_authentication_challenge(&challenge)).await?;
                self.auth_step = Some(AuthStep::WaitingForAuthResponse);
            }
            ControlMessageId::AuthenticationResponse => {
                tracing::info!("Authentication complete");
                self.send_ctrl(control::make_authentication_succeeded()).await?;
                self.auth_step = None;
            }
            ControlMessageId::RequestAppLaunch
            | ControlMessageId::StatusExternalAccessoryProtocolSession => {
                tracing::info!("Received EA session request");
                self.ea_session_id = match params.get_param::<u32>(ParameterId::SESSION_IDENTIFIER)
                {
                    Some(v) if v <= 0xFF => v as u8,
                    _ => 2,
                };
                let payload = control::make_start_ea_session("com.apple.carplay");
                self.send_ctrl(payload).await?;
                tracing::info!("Started EA session");
                if let Some(tx) = self.ready_tx.take() {
                    let _ = tx.send(());
                }
            }
            ControlMessageId::RequestWiFiInformation => {
                tracing::info!("Accessory requested WiFi information");
                if let Some(ssid) = params.get_param::<String>(ParameterId::SSID) {
                    let passphrase = params
                        .get_param::<String>(ParameterId::PASSPHRASE)
                        .unwrap_or_default();
                    let bssid = params.get_param::<String>(ParameterId::BSSID);
                    let creds = WifiCredentials {
                        ssid,
                        passphrase: Some(passphrase),
                        bssid,
                    };
                    let _ = self.wifi_tx.send(creds).await;
                }
                let payload = control::make_wifi_information("CarJack-WiFi", "carjack123");
                self.send_ctrl(payload).await?;
            }
            ControlMessageId::BluetoothComponentInformation => {
                tracing::debug!("Received BluetoothComponentInformation");
            }
            ControlMessageId::DeviceInformationUpdate => {
                tracing::debug!("Received DeviceInformationUpdate");
            }
            _ => {
                tracing::debug!("Unhandled control message: {:?}", msg.message_id);
            }
        }
        Ok(())
    }

    async fn handle_ea(&mut self, payload: &[u8]) -> Result<(), Iap2Error> {
        if let Ok((frame, _)) = EaFrame::from_bytes(payload) {
            self.ea_transfer_id = frame.transfer_id;
            if !frame.data.is_empty() {
                if let Ok((_, event)) = TouchEvent::from_bytes((&frame.data[..], 0)) {
                    let _ = self.touch_tx.send(event).await;
                }
            }
        }
        Ok(())
    }

    async fn send_video_frame(&mut self, h264_data: Vec<u8>) -> Result<(), Iap2Error> {
        let ea_session = self.ea_session_id;
        if ea_session == 0 {
            return Ok(());
        }
        let self_transfer_id = self.ea_transfer_id;
        let frame = EaFrame::new(self_transfer_id, h264_data.into());
        let payload = frame.to_bytes().to_vec();
        let ctrl = ControlByte {
            ack: true,
            ..Default::default()
        };
        self.iap2.send_packet(ctrl, ea_session, payload).await?;
        self.ea_transfer_id = self.ea_transfer_id.wrapping_add(1);
        Ok(())
    }
}
