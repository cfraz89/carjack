use deku::DekuContainerWrite;
use deku::prelude::*;

pub const SOM: u16 = 0x4040;

#[derive(DekuRead, DekuWrite, Debug, Clone, Copy, PartialEq, Eq)]
#[deku(id_type = "u16", endian = "big")]
pub enum ControlMessageId {
    #[deku(id = "0xAA00")]
    RequestAuthenticationCertificate,
    #[deku(id = "0xAA01")]
    AuthenticationCertificate,
    #[deku(id = "0xAA02")]
    RequestAuthenticationChallengeResponse,
    #[deku(id = "0xAA03")]
    AuthenticationResponse,
    #[deku(id = "0xAA04")]
    AuthenticationFailed,
    #[deku(id = "0xAA05")]
    AuthenticationSucceeded,
    #[deku(id = "0xAA06")]
    AccessoryAuthenticationSerialNumber,
    #[deku(id = "0xAA10")]
    RequestDeviceAuthenticationCertificate,
    #[deku(id = "0xAA11")]
    DeviceAuthenticationCertificate,
    #[deku(id = "0xAA12")]
    RequestDeviceAuthenticationChallengeResponse,
    #[deku(id = "0xAA13")]
    DeviceAuthenticationResponse,
    #[deku(id = "0xAA14")]
    DeviceAuthenticationFailed,
    #[deku(id = "0xAA15")]
    DeviceAuthenticationSucceeded,
    #[deku(id = "0x1D00")]
    StartIdentification,
    #[deku(id = "0x1D01")]
    IdentificationInformation,
    #[deku(id = "0x1D02")]
    IdentificationAccepted,
    #[deku(id = "0x1D03")]
    IdentificationRejected,
    #[deku(id = "0x1D05")]
    CancelIdentification,
    #[deku(id = "0x1D06")]
    IdentificationInformationUpdate,
    #[deku(id = "0xEA00")]
    StartExternalAccessoryProtocolSession,
    #[deku(id = "0xEA01")]
    StopExternalAccessoryProtocolSession,
    #[deku(id = "0xEA02")]
    RequestAppLaunch,
    #[deku(id = "0xEA03")]
    StatusExternalAccessoryProtocolSession,
    #[deku(id = "0xAD00")]
    StartAppDiscoveryUpdates,
    #[deku(id = "0xAD01")]
    AppDiscoveryUpdate,
    #[deku(id = "0xAD02")]
    StopAppDiscoveryUpdates,
    #[deku(id = "0x5400")]
    StartAssistiveTouch,
    #[deku(id = "0x5401")]
    StopAssistiveTouch,
    #[deku(id = "0x5402")]
    StartAssistiveTouchInformation,
    #[deku(id = "0x5403")]
    AssistiveTouchInformation,
    #[deku(id = "0x5404")]
    StopAssistiveTouchInformation,
    #[deku(id = "0x4E01")]
    BluetoothComponentInformation,
    #[deku(id = "0x4E03")]
    StartBluetoothConnectionUpdates,
    #[deku(id = "0x4E04")]
    BluetoothConnectionUpdate,
    #[deku(id = "0x4E05")]
    StopBluetoothConnectionUpdates,
    #[deku(id = "0x4E09")]
    DeviceInformationUpdate,
    #[deku(id = "0x4E0A")]
    DeviceLanguageUpdate,
    #[deku(id = "0x4E0B")]
    DeviceTimeUpdate,
    #[deku(id = "0x4E0C")]
    DeviceUUIDUpdate,
    #[deku(id = "0x4E0D")]
    WirelessCarPlayUpdate,
    #[deku(id = "0x4E0E")]
    DeviceTransportIdentifierNotification,
    #[deku(id = "0x4154")]
    StartCallStateUpdates,
    #[deku(id = "0x4155")]
    CallStateUpdate,
    #[deku(id = "0x4156")]
    StopCallStateUpdates,
    #[deku(id = "0x4157")]
    StartCommunicationsUpdates,
    #[deku(id = "0x4158")]
    CommunicationsUpdate,
    #[deku(id = "0x4159")]
    StopCommunicationsUpdates,
    #[deku(id = "0x415A")]
    InitiateCall,
    #[deku(id = "0x415B")]
    AcceptCall,
    #[deku(id = "0x415C")]
    EndCall,
    #[deku(id = "0x415D")]
    SwapCalls,
    #[deku(id = "0x415E")]
    MergeCalls,
    #[deku(id = "0x415F")]
    HoldStatusUpdate,
    #[deku(id = "0x4160")]
    MuteStatusUpdate,
    #[deku(id = "0x4161")]
    SendDTMF,
    #[deku(id = "0x4C00")]
    StartMediaLibraryInformation,
    #[deku(id = "0x4C01")]
    MediaLibraryInformation,
    #[deku(id = "0x4C02")]
    StopMediaLibraryInformation,
    #[deku(id = "0x4C06")]
    PlayMediaLibraryCurrentSelection,
    #[deku(id = "0x5000")]
    StartNowPlayingUpdates,
    #[deku(id = "0x5001")]
    NowPlayingUpdate,
    #[deku(id = "0x5002")]
    StopNowPlayingUpdates,
    #[deku(id = "0x5003")]
    SetNowPlayingInformation,
    #[deku(id = "0xAE00")]
    StartPowerUpdates,
    #[deku(id = "0xAE01")]
    PowerUpdate,
    #[deku(id = "0xAE02")]
    StopPowerUpdates,
    #[deku(id = "0xAE03")]
    PowerSourceUpdate,
    #[deku(id = "0x6800")]
    StartHID,
    #[deku(id = "0x6801")]
    DeviceHIDReport,
    #[deku(id = "0x6802")]
    AccessoryHIDReport,
    #[deku(id = "0x6803")]
    StopHID,
    #[deku(id = "0x6806")]
    StartNativeHID,
    #[deku(id = "0xFFFA")]
    StartLocationInformation,
    #[deku(id = "0xFFFB")]
    LocationInformation,
    #[deku(id = "0xFFFC")]
    StopLocationInformation,
    #[deku(id = "0xDA00")]
    StartUSBDeviceModeAudio,
    #[deku(id = "0xDA01")]
    USBDeviceModeAudioInformation,
    #[deku(id = "0xDA02")]
    StopUSBDeviceModeAudio,
    #[deku(id = "0x5700")]
    RequestWiFiInformation,
    #[deku(id = "0x5701")]
    WiFiInformation,
    #[deku(id = "0x5702")]
    RequestAccessoryWiFiConfigurationInformation,
    #[deku(id = "0x5703")]
    AccessoryWiFiConfigurationInformation,
    #[deku(id = "0x0B00")]
    StartBluetoothPairing,
    #[deku(id = "0x0B01")]
    BluetoothPairingAccessoryInformation,
    #[deku(id = "0x0B02")]
    BluetoothPairingStatus,
    #[deku(id = "0x0B03")]
    StopBluetoothPairing,
    #[deku(id = "0xA100")]
    StartVehicleStatusUpdates,
    #[deku(id = "0xA101")]
    VehicleStatusUpdate,
    #[deku(id = "0xA102")]
    StopVehicleStatusUpdates,
    #[deku(id = "0x5200")]
    StartRouteGuidanceUpdates,
    #[deku(id = "0x5201")]
    RouteGuidanceUpdate,
    #[deku(id = "0x5202")]
    RouteGuidanceManeuverUpdate,
    #[deku(id = "0x5203")]
    StopRouteGuidanceUpdates,
    #[deku(id_pat = "_")]
    Unknown(u16),
}

#[derive(DekuRead, DekuWrite, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterId(#[deku(endian = "big")] pub u16);

impl ParameterId {
    pub const DISPLAY_NAME: Self = Self(0x4E01);
    pub const PROTOCOL_NAME: Self = Self(0x0001);
    pub const SESSION_IDENTIFIER: Self = Self(0x0001);
    pub const STATUS: Self = Self(0x0002);
    pub const SSID: Self = Self(0x0001);
    pub const PASSPHRASE: Self = Self(0x0002);
    pub const BSSID: Self = Self(0x0003);
    pub const VERSION: Self = Self(0x4E02);
    pub const FEATURES: Self = Self(0x4E03);
    pub const AUTH_CHALLENGE: Self = Self(0x0001);
}

#[derive(DekuRead, DekuWrite, Debug, Clone)]
pub struct Parameter {
    #[deku(update = "4 + self.data.len() as u16", endian = "big")]
    pub total_length: u16,
    pub id: ParameterId,
    #[deku(count = "total_length.saturating_sub(4) as usize")]
    pub data: Vec<u8>,
}

pub trait IntoParameter {
    fn into_parameter(self, id: ParameterId) -> Parameter;
}

impl IntoParameter for &str {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        Parameter {
            total_length: 0,
            id,
            data: self.as_bytes().to_vec(),
        }
    }
}

impl IntoParameter for String {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        Parameter {
            total_length: 0,
            id,
            data: self.into_bytes(),
        }
    }
}

impl IntoParameter for u8 {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        Parameter {
            total_length: 0,
            id,
            data: vec![self],
        }
    }
}

impl IntoParameter for u16 {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        Parameter {
            total_length: 0,
            id,
            data: self.to_be_bytes().to_vec(),
        }
    }
}

impl IntoParameter for u32 {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        Parameter {
            total_length: 0,
            id,
            data: self.to_be_bytes().to_vec(),
        }
    }
}

impl IntoParameter for Vec<u8> {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        Parameter {
            total_length: 0,
            id,
            data: self,
        }
    }
}

impl IntoParameter for &[u8] {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        Parameter {
            total_length: 0,
            id,
            data: self.to_vec(),
        }
    }
}

impl Parameter {
    pub fn new<T: IntoParameter>(id: ParameterId, value: T) -> Self {
        let mut p = value.into_parameter(id);
        p.total_length = 4 + p.data.len() as u16;
        p
    }
}

pub trait TryFromParam: Sized {
    fn try_from_param(data: &[u8]) -> Option<Self>;
}

impl TryFromParam for String {
    fn try_from_param(data: &[u8]) -> Option<Self> {
        String::from_utf8(data.to_vec()).ok()
    }
}

impl TryFromParam for u32 {
    fn try_from_param(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(u32::from_be_bytes)
    }
}

impl TryFromParam for Vec<u8> {
    fn try_from_param(data: &[u8]) -> Option<Self> {
        Some(data.to_vec())
    }
}

pub trait GetParam {
    fn get_param<T: TryFromParam>(&self, id: ParameterId) -> Option<T>;
}

impl GetParam for [Parameter] {
    fn get_param<T: TryFromParam>(&self, id: ParameterId) -> Option<T> {
        self.iter()
            .find(|p| p.id == id)
            .and_then(|p| T::try_from_param(&p.data))
    }
}

#[derive(DekuRead, DekuWrite, Debug, Clone)]
pub struct ControlMessage {
    #[deku(endian = "big")]
    pub som: u16,
    #[deku(update = "6 + self.raw_parameters.len() as u16", endian = "big")]
    pub total_length: u16,
    pub message_id: ControlMessageId,
    #[deku(count = "total_length.saturating_sub(6) as usize")]
    pub raw_parameters: Vec<u8>,
}

impl ControlMessage {
    pub fn new(message_id: ControlMessageId, parameters: Vec<Parameter>) -> Self {
        let mut raw_parameters = Vec::new();
        for p in &parameters {
            raw_parameters.extend_from_slice(
                &<Parameter as deku::DekuContainerWrite>::to_bytes(p).unwrap_or_default(),
            );
        }
        let total_len = 6u16 + raw_parameters.len() as u16;
        ControlMessage {
            som: SOM,
            total_length: total_len,
            message_id,
            raw_parameters,
        }
    }

    pub fn parameters(&self) -> Vec<Parameter> {
        let mut params = Vec::new();
        let mut remaining: &[u8] = &self.raw_parameters;
        while !remaining.is_empty() {
            match <Parameter as deku::DekuContainerRead>::from_bytes((remaining, 0)) {
                Ok(((new_remaining, _), param)) => {
                    params.push(param);
                    remaining = new_remaining;
                }
                Err(_) => break,
            }
        }
        params
    }

    pub fn find_param(&self, id: ParameterId) -> Option<Parameter> {
        self.parameters().into_iter().find(|p| p.id == id)
    }

    pub fn param_string(&self, id: ParameterId) -> Option<String> {
        self.find_param(id)
            .map(|p| String::from_utf8_lossy(&p.data).to_string())
    }

    pub fn param_uint32(&self, id: ParameterId) -> Option<u32> {
        self.find_param(id).and_then(|p| {
            if p.data.len() == 4 {
                Some(u32::from_be_bytes([
                    p.data[0], p.data[1], p.data[2], p.data[3],
                ]))
            } else {
                None
            }
        })
    }
}

pub fn parse_control_payload(data: &[u8]) -> Vec<ControlMessage> {
    let mut messages = Vec::new();
    let mut remaining = data;
    while !remaining.is_empty() {
        match <ControlMessage as deku::DekuContainerRead>::from_bytes((remaining, 0)) {
            Ok(((new_remaining, _), msg)) => {
                messages.push(msg);
                remaining = new_remaining;
            }
            Err(_) => break,
        }
    }
    messages
}

pub fn build_control_payload(messages: &[ControlMessage]) -> Vec<u8> {
    let mut buf = Vec::new();
    for msg in messages {
        buf.extend_from_slice(
            &<ControlMessage as deku::DekuContainerWrite>::to_bytes(msg).unwrap_or_default(),
        );
    }
    buf
}

pub fn make_start_identification() -> Vec<u8> {
    ControlMessage::new(
        ControlMessageId::StartIdentification,
        vec![
            Parameter::new(ParameterId::DISPLAY_NAME, "CarJack"),
            Parameter::new(ParameterId::VERSION, 0x01020000u32),
            Parameter::new(ParameterId::FEATURES, 0x0001u16),
        ],
    )
    .to_bytes()
    .expect("ControlMessage serialization")
}

pub fn make_identification_accepted() -> Vec<u8> {
    ControlMessage::new(ControlMessageId::IdentificationAccepted, vec![])
        .to_bytes()
        .expect("ControlMessage serialization")
}

pub fn make_request_auth_certificate() -> Vec<u8> {
    ControlMessage::new(ControlMessageId::RequestAuthenticationCertificate, vec![])
        .to_bytes()
        .expect("ControlMessage serialization")
}

pub fn make_request_auth_certificate_serial() -> Vec<u8> {
    ControlMessage::new(
        ControlMessageId::RequestAuthenticationCertificate,
        vec![Parameter::new(ParameterId(0x0000), &[][..])],
    )
    .to_bytes()
    .expect("ControlMessage serialization")
}

pub fn make_authentication_challenge(challenge: &[u8]) -> Vec<u8> {
    ControlMessage::new(
        ControlMessageId::RequestAuthenticationChallengeResponse,
        vec![Parameter::new(ParameterId(0x0000), challenge)],
    )
    .to_bytes()
    .expect("ControlMessage serialization")
}

pub fn make_authentication_succeeded() -> Vec<u8> {
    ControlMessage::new(ControlMessageId::AuthenticationSucceeded, vec![])
        .to_bytes()
        .expect("ControlMessage serialization")
}

pub fn make_wireless_carplay_update() -> Vec<u8> {
    ControlMessage::new(
        ControlMessageId::WirelessCarPlayUpdate,
        vec![
            Parameter::new(ParameterId(0x0001), 0x00000001u32),
            Parameter::new(ParameterId(0x0002), 0x01u8),
        ],
    )
    .to_bytes()
    .expect("ControlMessage serialization")
}

pub fn make_now_playing_update() -> Vec<u8> {
    ControlMessage::new(ControlMessageId::NowPlayingUpdate, vec![])
        .to_bytes()
        .expect("ControlMessage serialization")
}

pub fn make_request_accessory_wifi_config() -> Vec<u8> {
    ControlMessage::new(ControlMessageId::RequestAccessoryWiFiConfigurationInformation, vec![])
        .to_bytes()
        .expect("ControlMessage serialization")
}

pub fn make_wifi_information(ssid: &str, passphrase: &str) -> Vec<u8> {
    ControlMessage::new(
        ControlMessageId::WiFiInformation,
        vec![
            Parameter::new(ParameterId::SSID, ssid),
            Parameter::new(ParameterId::PASSPHRASE, passphrase),
        ],
    )
    .to_bytes()
    .expect("ControlMessage serialization")
}

pub fn make_start_ea_session(protocol: &str) -> Vec<u8> {
    ControlMessage::new(
        ControlMessageId::StartExternalAccessoryProtocolSession,
        vec![Parameter::new(ParameterId::PROTOCOL_NAME, protocol)],
    )
    .to_bytes()
    .expect("ControlMessage serialization")
}

pub fn make_ea_session_status(session_id: u8, status: u8) -> Vec<u8> {
    ControlMessage::new(
        ControlMessageId::StatusExternalAccessoryProtocolSession,
        vec![
            Parameter::new(ParameterId::SESSION_IDENTIFIER, session_id as u16),
            Parameter::new(ParameterId::STATUS, status),
        ],
    )
    .to_bytes()
    .expect("ControlMessage serialization")
}
